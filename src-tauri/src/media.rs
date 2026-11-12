//! Now-playing + transport controls through Windows' GlobalSystemMediaTransportControls
//! (Spotify, browsers, the system player... anything that registers a media session).
use base64::Engine;
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Serialize, Clone, Default, PartialEq)]
pub struct MediaState {
    pub has_session: bool,
    pub title: String,
    pub artist: String,
    pub app: String,
    pub playing: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    /// `data:` URL, only set on the update right after the track changed.
    pub thumb: Option<String>,
}

fn com_init() {
    // Already-initialised threads return S_FALSE / RPC_E_CHANGED_MODE: both are fine here.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn session() -> windows::core::Result<Session> {
    Manager::RequestAsync()?.join()?.GetCurrentSession()
}

fn read_thumb(props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<String> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 8_000_000 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    let mime = stream.ContentType().map(|s| s.to_string()).unwrap_or_default();
    let mime = if mime.is_empty() { "image/jpeg".to_string() } else { mime };
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(buf)))
}

/// Snapshot of the current session. `with_thumb` skips the (heavier) cover fetch.
fn snapshot(with_thumb: bool) -> windows::core::Result<MediaState> {
    let s = session()?;
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let tl = s.GetTimelineProperties()?;
    let ticks_to_ms = |t: windows::Foundation::TimeSpan| t.Duration / 10_000;
    let start = tl.StartTime().map(ticks_to_ms).unwrap_or(0);
    let playing = s.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    let duration_ms = tl.EndTime().map(ticks_to_ms).unwrap_or(0) - start;
    let mut position_ms = tl.Position().map(ticks_to_ms).unwrap_or(0) - start;
    // `Position` is only valid as of `LastUpdatedTime`, and browsers refresh it rarely (or only on seek/pause), so while
    // playing the real playhead is Position + time elapsed since then. Without this the bar froze at the stale value.
    if playing {
        if let Ok(updated) = tl.LastUpdatedTime() {
            const UNIX_TO_FILETIME_SECS: i64 = 11_644_473_600;
            if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                let now_ticks = (now.as_secs() as i64 + UNIX_TO_FILETIME_SECS) * 10_000_000 + (now.subsec_nanos() / 100) as i64;
                let elapsed_ms = (now_ticks - updated.UniversalTime) / 10_000;
                if updated.UniversalTime > 0 && (0..6 * 3600 * 1000).contains(&elapsed_ms) {
                    position_ms += elapsed_ms;
                }
            }
        }
        if duration_ms > 0 {
            position_ms = position_ms.min(duration_ms);
        }
    }
    Ok(MediaState {
        has_session: true,
        title: props.Title()?.to_string(),
        artist: props.Artist()?.to_string(),
        app: s.SourceAppUserModelId().map(|a| a.to_string()).unwrap_or_default(),
        playing,
        position_ms,
        duration_ms,
        thumb: if with_thumb { read_thumb(&props) } else { None },
    })
}

pub fn spawn_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        com_init();
        let mut last_key = String::new();
        let mut thumb_sent = false;
        loop {
            let need_thumb = !thumb_sent;
            let state = snapshot(need_thumb).unwrap_or_default();
            let key = format!("{}|{}|{}", state.app, state.title, state.artist);
            let mut out = state.clone();
            if key != last_key {
                last_key = key;
                thumb_sent = false;
                // Re-read immediately so the cover goes out with the first update of a new track.
                out = snapshot(true).unwrap_or(state);
            }
            if out.thumb.is_some() {
                thumb_sent = true;
            }
            let _ = app.emit("media-state", out);
            std::thread::sleep(Duration::from_millis(1000));
        }
    });
}

/// Blocking; call from `spawn_blocking`. `action`: play_pause | next | previous | status.
pub fn control(action: &str) -> Result<String, String> {
    com_init();
    let s = session().map_err(|_| "No hay ningún reproductor activo.".to_string())?;
    let err = |e: windows::core::Error| e.to_string();
    match action {
        "play_pause" => s.TryTogglePlayPauseAsync().map_err(err)?.join().map_err(err)?,
        "next" => s.TrySkipNextAsync().map_err(err)?.join().map_err(err)?,
        "previous" => s.TrySkipPreviousAsync().map_err(err)?.join().map_err(err)?,
        "status" => {
            let st = snapshot(false).map_err(err)?;
            return Ok(format!(
                "{} — {} ({}, {})",
                st.title,
                st.artist,
                st.app,
                if st.playing { "reproduciendo" } else { "en pausa" }
            ));
        }
        other => return Err(format!("acción desconocida: {other}")),
    };
    Ok("ok".into())
}

#[tauri::command]
pub async fn media_control(action: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || control(&action))
        .await
        .map_err(|e| e.to_string())?
}

/// System master volume (the default output device), 0.0..=1.0. Per-app volume isn't exposed by the media session.
fn endpoint() -> windows::core::Result<windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume> {
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    com_init();
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        dev.Activate(CLSCTX_ALL, None)
    }
}

#[tauri::command]
pub async fn volume_get() -> Result<f32, String> {
    tauri::async_runtime::spawn_blocking(|| (|| unsafe { endpoint()?.GetMasterVolumeLevelScalar() })().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn volume_set(level: f32) -> Result<(), String> {
    let level = level.clamp(0.0, 1.0);
    tauri::async_runtime::spawn_blocking(move || (|| unsafe { endpoint()?.SetMasterVolumeLevelScalar(level, std::ptr::null()) })().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Seek the current session to `position_ms` (the progress bar in the Music module). Not every player allows it.
#[tauri::command]
pub async fn media_seek(position_ms: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        com_init();
        let s = session().map_err(|_| "No hay ningún reproductor activo.".to_string())?;
        let err = |e: windows::core::Error| e.to_string();
        let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|t| t.Duration).unwrap_or(0);
        let ticks = start + position_ms.max(0) * 10_000;
        let ok = s.TryChangePlaybackPositionAsync(ticks).map_err(err)?.join().map_err(err)?;
        if ok {
            Ok(())
        } else {
            Err("El reproductor no permite saltar a esa posición.".to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
