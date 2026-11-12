mod audio;
mod memory;
mod chat;
#[cfg(windows)]
mod media;
mod g2p_es;
mod localtts;
mod mic;
mod screen;
mod pomodoro;
mod reminders;

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, PhysicalPosition, WebviewWindow};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

/// Pill rectangle in logical px, relative to the window's top-left.
#[derive(Default, Clone, Copy)]
struct PillRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct PillState(Mutex<PillRect>);

/// Where the window must sit (physical px). Enforced inside the window procedure, see `pin_window`.
static TARGET_X: AtomicI32 = AtomicI32::new(i32::MIN);
static TARGET_Y: AtomicI32 = AtomicI32::new(i32::MIN);

/// Docks / menu-bar replacements (MyDockFinder) reserve a strip at the top of the screen and push other windows
/// below it, which made the notch "jump" a few px down for a frame. Rewriting the position inside
/// WM_WINDOWPOSCHANGING means that move never happens at all, instead of being undone a frame later.
#[cfg(windows)]
unsafe extern "system" fn pin_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::DefSubclassProc;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SWP_NOMOVE, WINDOWPOS, WM_WINDOWPOSCHANGING};
    // SAFETY: for WM_WINDOWPOSCHANGING, Windows guarantees `lparam` points to a valid, writable WINDOWPOS.
    unsafe {
        if msg == WM_WINDOWPOSCHANGING {
            let wp = &mut *(lparam as *mut WINDOWPOS);
            let (tx, ty) = (TARGET_X.load(Ordering::Relaxed), TARGET_Y.load(Ordering::Relaxed));
            if wp.flags & SWP_NOMOVE == 0 && tx != i32::MIN {
                wp.x = tx;
                wp.y = ty;
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

#[cfg(windows)]
fn pin_window(win: &WebviewWindow) {
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            SetWindowSubclass(hwnd.0 as _, Some(pin_proc), 1, 0);
        }
    }
}
#[cfg(not(windows))]
fn pin_window(_win: &WebviewWindow) {}

/// Set by the UI while the notch is open/being used, so it never auto-hides under the user's hands.
static IN_USE: AtomicBool = AtomicBool::new(false);
/// Tray toggle: keep the notch visible over every app instead of only on the desktop.
static ALWAYS_VISIBLE: AtomicBool = AtomicBool::new(false);
/// Last visibility decision sent to the UI.
static VISIBLE: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn set_in_use(in_use: bool) {
    IN_USE.store(in_use, Ordering::Relaxed);
}

/// Voice activation hook. The wake-word engine (later phase) calls this with `true` when it hears the
/// trigger phrase and `false` when the voice session ends; the UI opens in its translucent "voice" look.
#[tauri::command]
fn voice_activate(app: tauri::AppHandle, active: bool) {
    let _ = app.emit("voice-active", active);
}

#[tauri::command]
fn notch_visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

#[tauri::command]
fn set_pill_rect(state: tauri::State<PillState>, x: f64, y: f64, w: f64, h: f64, r: Option<f64>) {
    *state.0.lock().unwrap() = PillRect { x, y, w, h };
    let _ = r;
}

#[cfg(windows)]
mod backdrop {
    //! Live blurred backdrop. A dedicated thread keeps sampling the desktop behind the notch (a screen BitBlt waits for the
    //! compositor, ~1 frame, so it must never run on the UI/IPC path); the page just asks for the newest frame.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Graphics::Gdi::*;

    const SCALE: f64 = 6.0;

    #[derive(Clone, Copy, Default, PartialEq)]
    pub struct Req {
        pub logical: [i32; 4], // x, y, w, h in window px (echoed back so the page can place the canvas)
        pub screen: [i32; 4],  // x, y, w, h in physical screen px
    }
    struct Shared {
        req: Mutex<(Req, Instant)>,
        frame: Mutex<(u64, u64, Vec<u8>)>, // seq, generation, [cw, ch, lx, ly, lw, lh (i32 LE each)] + RGBA
    }
    /// Bumped each time the page starts asking again after a pause, so a frame from the previous opening is never shown.
    static GEN: AtomicU64 = AtomicU64::new(1);
    static SHARED: OnceLock<Shared> = OnceLock::new();

    fn shared() -> &'static Shared {
        SHARED.get_or_init(|| {
            std::thread::spawn(worker);
            Shared { req: Mutex::new((Req::default(), Instant::now() - Duration::from_secs(60))), frame: Mutex::new((0, 0, Vec::new())) }
        })
    }

    pub fn request(req: Req) {
        let s = shared();
        let mut g = s.req.lock().unwrap();
        if g.1.elapsed() > Duration::from_millis(400) {
            GEN.fetch_add(1, Ordering::Relaxed);
        }
        *g = (req, Instant::now());
    }

    /// The newest frame if it is newer than `seen`, else an 8-byte "nothing new" reply.
    pub fn latest(seen: u64) -> Vec<u8> {
        let s = shared();
        let f = s.frame.lock().unwrap();
        if f.2.is_empty() || f.0 <= seen || f.1 != GEN.load(Ordering::Relaxed) {
            return vec![0u8; 32];
        }
        let mut out = Vec::with_capacity(f.2.len() + 8);
        out.extend_from_slice(&f.0.to_le_bytes());
        out.extend_from_slice(&f.2);
        out
    }

    fn worker() {
        let mut last_hash = 0u64;
        let mut seq = 0u64;
        loop {
            let s = shared();
            let (req, at) = *s.req.lock().unwrap();
            // Idle when the page stopped asking (notch closed): no capture at all.
            if at.elapsed() > Duration::from_millis(400) || req.screen[2] < 3 || req.screen[3] < 3 {
                std::thread::sleep(Duration::from_millis(40));
                last_hash = 0;
                continue;
            }
            let generation = GEN.load(Ordering::Relaxed);
            let [sx, sy, pw, ph] = req.screen;
            let (cw, ch) = (((pw as f64 / SCALE).round() as i32).max(2), ((ph as f64 / SCALE).round() as i32).max(2));
            if cw >= 600 || ch >= 400 {
                std::thread::sleep(Duration::from_millis(40));
                continue;
            }
            let mut out = vec![0u8; 24 + (cw * ch * 4) as usize];
            unsafe {
                let screen = GetDC(std::ptr::null_mut());
                let dc = CreateCompatibleDC(screen);
                let bmp = CreateCompatibleBitmap(screen, cw, ch);
                let old = SelectObject(dc, bmp as _);
                SetStretchBltMode(dc, HALFTONE);
                SetBrushOrgEx(dc, 0, 0, std::ptr::null_mut());
                StretchBlt(dc, 0, 0, cw, ch, screen, sx, sy, pw, ph, SRCCOPY);
                let mut bi: BITMAPINFO = std::mem::zeroed();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = cw;
                bi.bmiHeader.biHeight = -ch;
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                GetDIBits(dc, bmp, 0, ch as u32, out[24..].as_mut_ptr() as _, &mut bi, DIB_RGB_COLORS);
                SelectObject(dc, old);
                DeleteObject(bmp as _);
                DeleteDC(dc);
                ReleaseDC(std::ptr::null_mut(), screen);
            }
            let mut hash: u64 = 0xcbf29ce484222325 ^ ((req.logical[0] as u64) << 40) ^ ((req.logical[1] as u64) << 20) ^ ((cw as u64) << 8) ^ ch as u64;
            for px in out[24..].chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
                hash = (hash ^ u32::from_le_bytes([px[0], px[1], px[2], 0]) as u64).wrapping_mul(0x100000001b3);
            }
            let unchanged = hash == last_hash && s.frame.lock().unwrap().1 == generation;
            if unchanged {
                std::thread::sleep(Duration::from_millis(30)); // still desktop: sample at ~30 Hz instead of flat out
            }
            if !unchanged {
                last_hash = hash;
                seq += 1;
                for (i, v) in [cw, ch, req.logical[0], req.logical[1], req.logical[2], req.logical[3]].iter().enumerate() {
                    out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                *s.frame.lock().unwrap() = (seq, generation, out);
            }
        }
    }
}

/// Newest live-backdrop frame for the band `x, y, w, h` (logical window px). `seen` = sequence number of the frame the page
/// already has. Reply: seq u64, then cw, ch, lx, ly, lw, lh (i32 LE), then RGBA pixels; seq 0 = nothing new.
#[tauri::command]
fn backdrop_snapshot(win: WebviewWindow, x: f64, y: f64, w: f64, h: f64, seen: u64) -> tauri::ipc::Response {
    #[cfg(windows)]
    {
        if let Ok(pos) = win.outer_position() {
            let s = win.scale_factor().unwrap_or(1.0);
            let req = backdrop::Req {
                logical: [x as i32, y as i32, w as i32, h as i32],
                screen: [pos.x + (x * s).round() as i32, pos.y + (y * s).round() as i32, (w * s).round() as i32, (h * s).round() as i32],
            };
            backdrop::request(req);
            return tauri::ipc::Response::new(backdrop::latest(seen));
        }
    }
    let _ = (&win, x, y, w, h, seen);
    tauri::ipc::Response::new(vec![0u8; 32])
}

/// Hide the notch window from screen capture (BitBlt/duplication). The live blur samples the desktop *behind* the notch every
/// frame; without this it would photograph itself. Set OPENNOTCH_CAPTURABLE=1 to disable (e.g. for screenshot-based testing).
#[cfg(windows)]
fn exclude_from_capture(win: &WebviewWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity;
    if std::env::var_os("OPENNOTCH_CAPTURABLE").is_some() {
        return;
    }
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            SetWindowDisplayAffinity(hwnd.0 as _, 0x11); // WDA_EXCLUDEFROMCAPTURE
        }
    }
}
#[cfg(not(windows))]
fn exclude_from_capture(_win: &WebviewWindow) {}

#[tauri::command]
fn focus_window(win: WebviewWindow) {
    let _ = win.set_focus();
    force_foreground(&win);
}

/// Windows refuses focus to background apps; attaching to the foreground thread's input queue lets us take it.
#[cfg(windows)]
fn force_foreground(win: &WebviewWindow) {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows_sys::Win32::System::Threading::AttachThreadInput;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };
    let Ok(hwnd) = win.hwnd() else { return };
    unsafe {
        let fg = GetForegroundWindow();
        if fg == hwnd.0 as _ {
            return;
        }
        let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, std::ptr::null_mut()) };
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0;
        SetForegroundWindow(hwnd.0 as _);
        SetFocus(hwnd.0 as _);
        if attached {
            AttachThreadInput(me, fg_thread, 0);
        }
    }
}
#[cfg(not(windows))]
fn force_foreground(_win: &WebviewWindow) {}

/// Where the window should sit: centered on the top edge of the primary monitor.
fn anchor_target(win: &WebviewWindow) -> Option<PhysicalPosition<i32>> {
    let monitor = win.primary_monitor().ok()??;
    let win_w = win.outer_size().ok()?.width as i32;
    let x = monitor.position().x + (monitor.size().width as i32 - win_w) / 2;
    Some(PhysicalPosition::new(x, monitor.position().y))
}

fn anchor_top_center(win: &WebviewWindow) {
    if let Some(p) = anchor_target(win) {
        TARGET_X.store(p.x, Ordering::Relaxed);
        TARGET_Y.store(p.y, Ordering::Relaxed);
        let _ = win.set_position(p);
    }
}

/// Windows keeps one z-order band for all "topmost" windows, so launchers/docks
/// (MyDockFinder, etc.) can end up above us. Re-assert HWND_TOPMOST periodically
/// to stay at the top of that band without stealing focus.
#[cfg(windows)]
fn raise_topmost(win: &WebviewWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            SetWindowPos(hwnd.0 as _, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}
#[cfg(not(windows))]
fn raise_topmost(_win: &WebviewWindow) {}

/// True while a full-screen app (game, F11 video, presentation) owns the screen.
#[cfg(windows)]
fn fullscreen_app_running() -> bool {
    use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
    let mut state = 0i32;
    // 2 = busy/fullscreen app, 3 = Direct3D fullscreen, 4 = presentation mode
    unsafe { SHQueryUserNotificationState(&mut state) >= 0 && matches!(state, 2 | 3 | 4) }
}
#[cfg(not(windows))]
fn fullscreen_app_running() -> bool {
    false
}

/// Hide/show without activating (a plain `show()` would steal focus from the app you're using).
#[cfg(windows)]
fn set_visible(win: &WebviewWindow, visible: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE};
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            ShowWindow(hwnd.0 as _, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
}
#[cfg(not(windows))]
fn set_visible(win: &WebviewWindow, visible: bool) {
    let _ = if visible { win.show() } else { win.hide() };
}

struct CursorInfo {
    /// Logical px relative to the window's top-left.
    lx: f64,
    ly: f64,
    /// Window width in logical px.
    win_w: f64,
}

/// Read straight from Win32. (tauri's `cursor_position()` / `outer_position()` round-trip through
/// the UI thread; polling them every 16 ms was the main idle-CPU cost.)
#[cfg(windows)]
fn cursor_info(hwnd: isize) -> Option<CursorInfo> {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
    let h = hwnd as _;
    let mut pt = POINT { x: 0, y: 0 };
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe {
        if GetCursorPos(&mut pt) == 0 || GetWindowRect(h, &mut r) == 0 {
            return None;
        }
        let scale = GetDpiForWindow(h).max(96) as f64 / 96.0;
        Some(CursorInfo {
            lx: (pt.x - r.left) as f64 / scale,
            ly: (pt.y - r.top) as f64 / scale,
            win_w: (r.right - r.left) as f64 / scale,
        })
    }
}
#[cfg(not(windows))]
fn cursor_info(_hwnd: isize) -> Option<CursorInfo> {
    None
}

/// True when the foreground window is the desktop/shell (nothing else is in front).
#[cfg(windows)]
fn desktop_in_front(own_hwnd: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, IsIconic, IsWindowVisible};
    unsafe {
        let fg = GetForegroundWindow();
        // Nothing focused, our own window, or a minimized/hidden window still "focused" after
        // Win+D / minimize-all: no app is actually on screen, so the desktop is showing.
        if fg.is_null() || fg as isize == own_hwnd || IsIconic(fg) != 0 || IsWindowVisible(fg) == 0 {
            return true;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, buf.as_mut_ptr(), buf.len() as i32);
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}
#[cfg(not(windows))]
fn desktop_in_front(_own_hwnd: isize) -> bool {
    true
}

#[cfg(windows)]
fn left_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    // Bit 15 = held right now; bit 0 = pressed since the previous call. The latter catches very
    // short clicks that start and end between two 16 ms polls.
    unsafe { (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8001) != 0 }
}
#[cfg(not(windows))]
fn left_button_down() -> bool {
    false
}

/// Single housekeeping thread:
/// - every 16 ms: click-through hit test, outside-click detection, hot-zone peek
/// - every ~64 ms: decide whether the notch should be visible (desktop in front / in use / peeking)
/// - every ~250 ms: re-assert topmost
/// - every ~500 ms: re-anchor (monitor/resolution/work-area changes) and hide for full-screen apps
fn spawn_window_loop(win: WebviewWindow) {
    std::thread::spawn(move || {
        let mut ignoring: Option<bool> = None;
        let mut hidden = false;
        let mut tick: u32 = 0;
        let hwnd = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
        let mut hot_since: Option<Instant> = None;
        let mut peek_until: Option<Instant> = None;
        let mut desktop = true;
        let mut was_down = false;
        loop {
            std::thread::sleep(Duration::from_millis(16));
            tick = tick.wrapping_add(1);

            if tick % 30 == 0 {
                let want_hidden = fullscreen_app_running();
                if want_hidden != hidden {
                    hidden = want_hidden;
                    set_visible(&win, !hidden);
                }
                if !hidden {
                    if let (Some(target), Ok(pos)) = (anchor_target(&win), win.outer_position()) {
                        // new monitor / resolution: refresh the pinned target first, then move
                        TARGET_X.store(target.x, Ordering::Relaxed);
                        TARGET_Y.store(target.y, Ordering::Relaxed);
                        if pos.x != target.x || pos.y != target.y {
                            let _ = win.set_position(target);
                        }
                    }
                }
            }
            if hidden {
                continue;
            }
            if tick % 6 == 0 {
                raise_topmost(&win);
            }
            if tick % 4 == 0 {
                desktop = desktop_in_front(hwnd);
            }

            let Some(c) = cursor_info(hwnd) else { continue };
            let rect = *win.state::<PillState>().0.lock().unwrap();
            let inside = rect.w > 0.0 && c.lx >= rect.x && c.lx <= rect.x + rect.w && c.ly >= rect.y && c.ly <= rect.y + rect.h;

            // Click-through: only the notch captures the mouse.
            let want_ignore = !inside;
            if ignoring != Some(want_ignore) {
                let _ = win.set_ignore_cursor_events(want_ignore);
                ignoring = Some(want_ignore);
            }

            // Clicks outside the notch never reach the webview (click-through), so detect them here.
            let down = left_button_down();
            if down && !was_down && !inside && IN_USE.load(Ordering::Relaxed) {
                let _ = win.emit("outside-click", ());
            }
            was_down = down;

            // Hot zone: pushing the cursor against the top edge, centered on the notch, peeks it into view.
            let now = Instant::now();
            let in_hot_zone = c.ly <= 1.5 && c.ly >= -2.0 && (c.lx - c.win_w / 2.0).abs() <= 130.0;
            if in_hot_zone {
                let since = *hot_since.get_or_insert(now);
                if now.duration_since(since) >= Duration::from_millis(180) {
                    peek_until = Some(now + Duration::from_millis(2500));
                }
            } else {
                hot_since = None;
            }
            if inside {
                peek_until = Some(now + Duration::from_millis(1500));
            }
            let peeking = peek_until.is_some_and(|t| t > now);

            let visible = ALWAYS_VISIBLE.load(Ordering::Relaxed) || IN_USE.load(Ordering::Relaxed) || desktop || peeking;
            if VISIBLE.swap(visible, Ordering::Relaxed) != visible {
                let _ = win.emit("notch-visible", visible);
            }
        }
    });
}

/// Look of the level-meter bars ("Barras" in the tray menu). Persisted in ui.json next to settings.json.
#[derive(Clone, Copy, PartialEq, serde::Serialize)]
struct BarSettings {
    /// The cover is shrunk to NxN px before painting the bars, so a SMALLER N is a stronger blur.
    blur: u8,
    /// CSS brightness() / saturate() in percent.
    brightness: u16,
    saturation: u16,
    /// Opacity (percent) of the white layer over the bars.
    veil: u8,
}

const BAR_DEFAULTS: BarSettings = BarSettings { blur: 3, brightness: 135, saturation: 170, veil: 18 };

/// (key, submenu title, [(value, label)]).
type BarGroup = (&'static str, &'static str, &'static [(u16, &'static str)]);
const BAR_GROUPS: [BarGroup; 4] = [
    ("blur", "Difuminado", &[(8, "Bajo"), (5, "Medio"), (3, "Alto"), (2, "Máximo")]),
    ("brightness", "Brillo", &[(100, "100 % (original)"), (115, "115 %"), (135, "135 %"), (155, "155 %")]),
    ("saturation", "Saturación", &[(100, "100 % (original)"), (130, "130 %"), (170, "170 %"), (200, "200 %")]),
    ("veil", "Capa blanca", &[(0, "Ninguna"), (9, "Suave"), (18, "Normal"), (30, "Fuerte")]),
];

impl BarSettings {
    fn get(&self, key: &str) -> u16 {
        match key {
            "blur" => self.blur as u16,
            "brightness" => self.brightness,
            "saturation" => self.saturation,
            _ => self.veil as u16,
        }
    }
    fn set(&mut self, key: &str, v: u16) {
        match key {
            "blur" => self.blur = v as u8,
            "brightness" => self.brightness = v,
            "saturation" => self.saturation = v,
            _ => self.veil = v as u8,
        }
    }
}

fn ui_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("ui.json"))
}

fn load_bars(app: &tauri::AppHandle) -> BarSettings {
    let mut s = BAR_DEFAULTS;
    let json = ui_path(app).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    if let Some(v) = json {
        for (key, _, opts) in BAR_GROUPS {
            if let Some(n) = v.get("bars").and_then(|b| b.get(key)).and_then(|n| n.as_u64()) {
                if opts.iter().any(|(o, _)| *o as u64 == n) {
                    s.set(key, n as u16);
                }
            }
        }
    }
    s
}

fn save_bars(app: &tauri::AppHandle, s: &BarSettings) {
    if let Some(path) = ui_path(app) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::json!({ "bars": s }).to_string());
    }
}

#[tauri::command]
fn get_bar_settings(app: tauri::AppHandle) -> BarSettings {
    load_bars(&app)
}

/// The notch has no taskbar entry, so the tray icon is the only way to quit or configure it.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let auto = CheckMenuItem::with_id(app, "autostart", "Iniciar con Windows", true, autostart_on, None::<&str>)?;
    let always = CheckMenuItem::with_id(app, "always", "Siempre visible (sobre todas las apps)", true, false, None::<&str>)?;
    let voice = CheckMenuItem::with_id(app, "voice", "Simular activación por voz", true, false, None::<&str>)?;
    let key = MenuItem::with_id(app, "clearkey", "Borrar API key", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir de OpenNotch", true, None::<&str>)?;
    let current = load_bars(app.handle());
    let mut bar_items: Vec<(&'static str, u16, CheckMenuItem<tauri::Wry>)> = Vec::new();
    let mut groups: Vec<tauri::menu::Submenu<tauri::Wry>> = Vec::new();
    for (key, title, opts) in BAR_GROUPS {
        let items = opts
            .iter()
            .map(|(v, label)| CheckMenuItem::with_id(app, format!("bars:{key}:{v}"), *label, true, current.get(key) == *v, None::<&str>))
            .collect::<tauri::Result<Vec<_>>>()?;
        let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
        groups.push(tauri::menu::Submenu::with_items(app, title, true, &refs)?);
        for ((v, _), item) in opts.iter().zip(items) {
            bar_items.push((key, *v, item));
        }
    }
    let reset = MenuItem::with_id(app, "bars:reset", "Restablecer", true, None::<&str>)?;
    let mut parts: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = groups.iter().map(|g| g as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
    parts.push(&reset);
    let bars_menu = tauri::menu::Submenu::with_items(app, "Barras", true, &parts)?;
    let menu = Menu::with_items(app, &[&auto, &always, &voice, &bars_menu, &key, &quit])?;

    let auto_item = auto.clone();
    let always_item = always.clone();
    let voice_item = voice.clone();
    let mut tray = TrayIconBuilder::new().tooltip("OpenNotch").menu(&menu).on_menu_event(move |app, ev| {
        match ev.id().as_ref() {
            "autostart" => {
                let launcher = app.autolaunch();
                let _ = if auto_item.is_checked().unwrap_or(false) { launcher.enable() } else { launcher.disable() };
            }
            "always" => ALWAYS_VISIBLE.store(always_item.is_checked().unwrap_or(false), Ordering::Relaxed),
            "voice" => {
                let _ = app.emit("voice-active", voice_item.is_checked().unwrap_or(false));
            }
            "clearkey" => {
                let _ = chat::clear_api_key();
                let _ = app.emit("key-cleared", ());
            }
            "quit" => app.exit(0),
            id if id.starts_with("bars:") => {
                let mut s = load_bars(app);
                let rest = &id["bars:".len()..];
                if rest == "reset" {
                    s = BAR_DEFAULTS;
                } else if let Some((key, v)) = rest.split_once(':') {
                    if let Ok(v) = v.parse::<u16>() {
                        s.set(key, v);
                    }
                }
                for (key, v, item) in &bar_items {
                    let _ = item.set_checked(s.get(key) == *v);
                }
                save_bars(app, &s);
                let _ = app.emit("bar-settings", s);
            }
            _ => {}
        }
    });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second launch (e.g. autostart + manual) just exits; the running notch stays.
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_notification::init())
        .manage(PillState(Mutex::new(PillRect::default())))
        .manage(pomodoro::PomoState_(Mutex::new(pomodoro::Pomo::new())))
        .invoke_handler(tauri::generate_handler![
            set_pill_rect,
            focus_window, backdrop_snapshot,
            set_in_use,
            voice_activate,
            notch_visible,
            chat::has_api_key,
            chat::set_api_key,
            chat::clear_api_key,
            chat::get_model,
            chat::set_model,
            chat::chat_send,
            memory::memory_list,
            memory::memory_clear,
            media::media_control,
            media::volume_get,
            media::volume_set,
            media::media_seek,
            audio::audio_meter_enable,
            mic::mic_start,
            mic::mic_stop,
            mic::mic_cancel,
            chat::tts_synthesize,
            localtts::tts_local,
            mic::debug_transcribe,
            get_bar_settings,
            pomodoro::pomo_control,
            reminders::reminder_add,
            reminders::reminder_list,
            reminders::reminder_set_done,
            reminders::reminder_delete
        ])
        .setup(|app| {
            let win = app.get_webview_window("main").expect("main window");
            anchor_top_center(&win);
            pin_window(&win);
            win.set_ignore_cursor_events(true)?;
            exclude_from_capture(&win);
            app.manage(reminders::open(app.handle())?);
            reminders::spawn_scheduler(app.handle().clone());
            pomodoro::spawn_ticker(app.handle().clone());
            #[cfg(windows)]
            media::spawn_watcher(app.handle().clone());
            audio::spawn(app.handle().clone());
            localtts::preload(app.handle().clone());
            mic::warmup(app.handle().clone());
            build_tray(app)?;
            win.show()?;
            spawn_window_loop(win);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
