//! Voice input: record the default microphone (WASAPI, shared mode), stop on its own after the user stops talking,
//! and hand the audio to Gemini for transcription. The UI only sees `mic-level`, `mic-transcribing` and `mic-result`.
//! Captured in Rust rather than with `getUserMedia` because the WebView's permission prompt never shows up on this
//! click-through, non-focused window.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::STGM_READ;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

static RECORDING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Startup probe: find the working microphone silently (no UI events) so the first real use does not lose speech.
static PROBE: AtomicBool = AtomicBool::new(false);
/// Name of the microphone that last delivered sound: tried first next time, so no speech is lost probing dead devices.
static GOOD_DEVICE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

const RATE: u32 = 16_000;
const FRAME: usize = 480; // 30 ms
const FRAME_MS: u32 = 30;
const END_SILENCE_MS: u32 = 800;
const NO_SPEECH_MS: u32 = 8_000;
const MAX_MS: u32 = 30_000;

#[derive(Debug, PartialEq)]
enum Verdict {
    Continue,
    /// `true` if the user said something.
    Done(bool),
}

/// Energy-based end-of-speech detector. The first 300 ms calibrate the room's noise floor.
struct Vad {
    floor: f32,
    frames: u32,
    loud_run: u32,
    quiet_ms: u32,
    total_ms: u32,
    speech: bool,
}

impl Vad {
    fn new() -> Self {
        Self { floor: 0.0, frames: 0, loud_run: 0, quiet_ms: 0, total_ms: 0, speech: false }
    }

    fn threshold(&self) -> f32 {
        (self.floor * 2.5).max(0.01)
    }

    fn push(&mut self, rms: f32) -> Verdict {
        self.frames += 1;
        self.total_ms += FRAME_MS;
        if self.frames <= 10 {
            self.floor = self.floor.max(rms.min(0.05));
            return Verdict::Continue;
        }
        if rms > self.threshold() {
            self.loud_run += 1;
            self.quiet_ms = 0;
            if self.loud_run >= 3 {
                self.speech = true;
            }
        } else {
            self.loud_run = 0;
            if self.speech {
                self.quiet_ms += FRAME_MS;
            }
        }
        if self.speech && self.quiet_ms >= END_SILENCE_MS {
            Verdict::Done(true)
        } else if !self.speech && self.total_ms >= NO_SPEECH_MS {
            Verdict::Done(false)
        } else if self.total_ms >= MAX_MS {
            Verdict::Done(self.speech)
        } else {
            Verdict::Continue
        }
    }
}

/// Averaging decimator from any capture rate (>= 16 kHz) down to 16 kHz.
struct Resampler {
    rate: u32,
    pos: u32,
    acc: f32,
    n: u32,
}

impl Resampler {
    fn new(rate: u32) -> Self {
        Self { rate: rate.max(RATE), pos: 0, acc: 0.0, n: 0 }
    }

    fn push(&mut self, s: f32, out: &mut Vec<f32>) {
        self.acc += s;
        self.n += 1;
        self.pos += RATE;
        if self.pos >= self.rate {
            self.pos -= self.rate;
            out.push(self.acc / self.n as f32);
            self.acc = 0.0;
            self.n = 0;
        }
    }
}

/// 16-bit mono PCM WAV.
fn to_wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

enum Outcome {
    /// The device delivered nothing but exact zeros (muted, nothing plugged in, virtual).
    Dead,
    Audio { samples: Vec<i16>, speech: bool },
    Cancelled,
}

/// Virtual "microphones" (Steam Streaming, VB-Cable, ...) are often the Windows default but never carry your voice.
fn is_virtual(name: &str) -> bool {
    let n = name.to_lowercase();
    ["steam", "virtual", "cable", "voicemeeter", "stereo mix", "mezcla est"].iter().any(|k| n.contains(k))
}

fn device_name(dev: &IMMDevice) -> String {
    unsafe {
        dev.OpenPropertyStore(STGM_READ)
            .and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

/// Active microphones in the order they are worth trying: the default one first, physical before virtual.
fn candidate_devices(en: &IMMDeviceEnumerator) -> Vec<(String, IMMDevice)> {
    let mut all: Vec<(String, IMMDevice)> = Vec::new();
    unsafe {
        if let Ok(d) = en.GetDefaultAudioEndpoint(eCapture, eConsole) {
            all.push((device_name(&d), d));
        }
        if let Ok(list) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
            for i in 0..list.GetCount().unwrap_or(0) {
                if let Ok(d) = list.Item(i) {
                    let name = device_name(&d);
                    if !all.iter().any(|(n, _)| *n == name) {
                        all.push((name, d));
                    }
                }
            }
        }
    }
    all.sort_by_key(|(n, _)| is_virtual(n)); // stable: default stays first within each group
    all
}

fn capture_on(app: &AppHandle, dev: &IMMDevice) -> windows::core::Result<Outcome> {
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let f: WAVEFORMATEX = std::ptr::read_unaligned(fmt);
        let (channels, rate, bits) = (f.nChannels as usize, f.nSamplesPerSec, f.wBitsPerSample);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        init?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut rs = Resampler::new(rate);
        let mut vad = Vad::new();
        let mut pcm: Vec<i16> = Vec::new();
        let mut frame: Vec<f32> = Vec::with_capacity(FRAME * 2);
        let mut level = 0.0f32;
        let started = Instant::now();
        let mut heard_signal = false;
        let mut outcome = None;
        'run: while outcome.is_none() {
            std::thread::sleep(Duration::from_millis(10));
            if CANCEL.load(Ordering::Relaxed) {
                outcome = Some(Outcome::Cancelled);
                break;
            }
            loop {
                let pending = cap.GetNextPacketSize()?;
                if pending == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let (mut frames, mut flags) = (0u32, 0u32);
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                for i in 0..frames as usize {
                    let mut acc = 0.0f32;
                    if !silent {
                        for c in 0..channels {
                            acc += match bits {
                                32 => std::ptr::read_unaligned((data as *const f32).add(i * channels + c)),
                                16 => std::ptr::read_unaligned((data as *const i16).add(i * channels + c)) as f32 / 32768.0,
                                _ => 0.0,
                            };
                        }
                    }
                    rs.push(acc / channels.max(1) as f32, &mut frame);
                }
                cap.ReleaseBuffer(frames)?;
                while frame.len() >= FRAME {
                    let chunk: Vec<f32> = frame.drain(0..FRAME).collect();
                    let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
                    if !heard_signal && chunk.iter().any(|s| s.abs() > 1e-4) {
                        heard_signal = true;
                        if PROBE.load(Ordering::Relaxed) {
                            outcome = Some(Outcome::Cancelled);
                            break 'run;
                        }
                        let _ = app.emit("mic-ready", ());
                    }
                    pcm.extend(chunk.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                    level += ((rms * 9.0).clamp(0.0, 1.0) - level) * 0.5;
                    if !PROBE.load(Ordering::Relaxed) {
                        let _ = app.emit("mic-level", level);
                    }
                    if let Verdict::Done(speech) = vad.push(rms) {
                        outcome = Some(Outcome::Audio { samples: std::mem::take(&mut pcm), speech });
                        break 'run;
                    }
                }
            }
            if !heard_signal && started.elapsed() > Duration::from_millis(600) {
                outcome = Some(Outcome::Dead);
                break;
            }
            if STOP.load(Ordering::Relaxed) {
                // manual stop: keep whatever was said (needs at least ~0.4 s of audio)
                let speech = vad.speech || pcm.len() > (RATE as usize * 2 / 5) && started.elapsed() > Duration::from_millis(600);
                outcome = Some(Outcome::Audio { samples: std::mem::take(&mut pcm), speech });
            }
        }
        let _ = client.Stop();
        Ok(outcome.unwrap_or(Outcome::Cancelled))
    }
}

/// Tries each microphone until one carries a signal.
fn capture(app: &AppHandle) -> windows::core::Result<Outcome> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut devices = candidate_devices(&en);
        let good = GOOD_DEVICE.lock().map(|g| g.clone()).unwrap_or_default();
        if let Some(p) = devices.iter().position(|(n, _)| *n == good) {
            let d = devices.remove(p);
            devices.insert(0, d);
        }
        if devices.is_empty() {
            return Err(windows::core::Error::from(windows::core::HRESULT(0x80070490u32 as i32)));
        }
        let names: Vec<&str> = devices.iter().map(|(n, _)| n.as_str()).collect();
        eprintln!("[mic] candidatos: {names:?}");
        for (name, dev) in &devices {
            match capture_on(app, dev) {
                Ok(Outcome::Dead) => eprintln!("[mic] {name}: sin señal, pruebo el siguiente"),
                Ok(o) => {
                    eprintln!("[mic] usando: {name}");
                    if let Ok(mut g) = GOOD_DEVICE.lock() {
                        *g = name.clone();
                    }
                    return Ok(o);
                }
                Err(e) => eprintln!("[mic] {name}: error {e}"),
            }
        }
        Ok(Outcome::Dead)
    }
}

fn result(app: &AppHandle, text: &str, error: &str) {
    let _ = app.emit("mic-result", serde_json::json!({ "text": text, "error": error }));
}

#[tauri::command]
pub fn mic_start(app: AppHandle) -> Result<(), String> {
    if RECORDING.swap(true, Ordering::SeqCst) {
        return Err("Ya estoy escuchando.".into());
    }
    STOP.store(false, Ordering::SeqCst);
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let out = capture(&app);
        let _ = app.emit("mic-level", 0.0f32);
        RECORDING.store(false, Ordering::SeqCst);
        match out {
            Err(e) => result(&app, "", &format!("No pude abrir el micrófono ({e}). Revisa que haya uno conectado.")),
            Ok(Outcome::Cancelled) => {}
            Ok(Outcome::Dead) => result(&app, "", "Ningún micrófono envía sonido. Revisa que esté conectado y no silenciado, y en Windows: Configuración → Privacidad → Micrófono → permitir apps de escritorio."),
            Ok(Outcome::Audio { samples, speech }) => {
                if samples.iter().all(|s| *s == 0) {
                    result(&app, "", "El micrófono no envía sonido. Revisa Configuración de Windows → Privacidad → Micrófono (permitir apps de escritorio).");
                } else if !speech {
                    result(&app, "", "");
                } else {
                    let _ = app.emit("mic-transcribing", ());
                    if cfg!(debug_assertions) {
                        if let Ok(dir) = tauri::Manager::path(&app).app_data_dir() {
                            let _ = std::fs::write(dir.join("last_capture.wav"), to_wav(&samples));
                        }
                    }
                    let wav = to_wav(&samples);
                    tauri::async_runtime::spawn(async move {
                        match crate::chat::transcribe_wav(&app, &wav).await {
                            Ok(t) => result(&app, &t, ""),
                            Err(e) => result(&app, "", &e),
                        }
                    });
                }

            }
        }
    });
    Ok(())
}

/// Shortly after startup, find which microphone really carries sound (virtual / unplugged ones are skipped).
pub fn warmup(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        if RECORDING.swap(true, Ordering::SeqCst) {
            return;
        }
        PROBE.store(true, Ordering::SeqCst);
        CANCEL.store(false, Ordering::SeqCst);
        let t = Instant::now();
        let _ = capture(&app);
        PROBE.store(false, Ordering::SeqCst);
        RECORDING.store(false, Ordering::SeqCst);
        eprintln!("[mic] calentamiento: {:?}, micrófono elegido: {:?}", t.elapsed(), GOOD_DEVICE.lock().map(|g| g.clone()).unwrap_or_default());
    });
}

/// Finish now and transcribe what was recorded.
#[tauri::command]
pub fn mic_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// Drop the recording without transcribing.
#[tauri::command]
pub fn mic_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Dev only: transcribe a WAV file from disk, to check the Gemini audio path without a working microphone.
#[tauri::command]
pub async fn debug_transcribe(app: AppHandle, path: String) -> Result<String, String> {
    if !cfg!(debug_assertions) {
        return Err("solo en desarrollo".into());
    }
    let wav = std::fs::read(&path).map_err(|e| e.to_string())?;
    crate::chat::transcribe_wav(&app, &wav).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_devices_are_recognised() {
        assert!(is_virtual("Micrófono (Steam Streaming Microphone)"));
        assert!(is_virtual("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(!is_virtual("Microphone Array (Realtek(R) Audio)"));
    }

    #[test]
    fn vad_ends_after_speech_then_silence() {
        let mut v = Vad::new();
        let mut done = None;
        // 0.5 s room noise, 1 s speech, then silence
        for i in 0..200 {
            let rms = if i < 17 { 0.004 } else if i < 50 { 0.08 } else { 0.004 };
            if let Verdict::Done(s) = v.push(rms) {
                done = Some((i, s));
                break;
            }
        }
        let (i, s) = done.expect("should finish");
        assert!(s);
        assert!((74..=78).contains(&i), "ended at frame {i}"); // ~0.8 s after speech stopped at frame 50
    }

    #[test]
    fn vad_gives_up_without_speech() {
        let mut v = Vad::new();
        let mut r = Verdict::Continue;
        for _ in 0..400 {
            r = v.push(0.003);
            if r != Verdict::Continue {
                break;
            }
        }
        assert_eq!(r, Verdict::Done(false));
    }

    #[test]
    fn vad_ignores_a_single_click() {
        let mut v = Vad::new();
        for i in 0..300 {
            let rms = if i == 40 { 0.2 } else { 0.003 };
            v.push(rms);
        }
        assert!(!v.speech);
    }

    #[test]
    fn resampler_48k_to_16k_averages_triples() {
        let mut rs = Resampler::new(48_000);
        let mut out = Vec::new();
        for s in [0.3, 0.3, 0.3, 0.9, 0.9, 0.9] {
            rs.push(s, &mut out);
        }
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.3).abs() < 1e-6 && (out[1] - 0.9).abs() < 1e-6);
        let mut rs = Resampler::new(44_100);
        let mut out = Vec::new();
        for _ in 0..44_100 {
            rs.push(0.1, &mut out);
        }
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn wav_header_is_valid() {
        let w = to_wav(&[1, -2, 3]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(w.len(), 44 + 6);
    }
}
