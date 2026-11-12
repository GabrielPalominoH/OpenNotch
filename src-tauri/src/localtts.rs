//! Local neural voice: Kokoro-82M (fp32 ONNX) through sherpa-onnx, fully offline. Spanish text is phonemized by
//! `g2p_es` and fed as phoneme codepoints (sherpa-onnx >= 2 no longer bundles espeak-ng).
//! Files live in `<app data>/tts/kokoro/` (model.onnx, voices.bin, tokens.txt); without them the UI falls back to Gemini TTS.
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

/// `ef_dora`, the female Spanish voice of kokoro-multi-lang-v1_0.
const SPEAKER: i32 = 28;
const SPEED: f32 = 1.05;

struct Engine(OfflineTts);
// The engine is only ever used behind the mutex, one request at a time.
unsafe impl Send for Engine {}

static ENGINE: OnceLock<Result<Mutex<Engine>, String>> = OnceLock::new();

fn model_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("tts").join("kokoro"))
}

fn engine(app: &AppHandle) -> &'static Result<Mutex<Engine>, String> {
    ENGINE.get_or_init(|| {
        let dir = model_dir(app)?;
        let path = |f: &str| dir.join(f).to_string_lossy().replace('\\', "/");
        for f in ["model.onnx", "voices.bin", "tokens.txt"] {
            if !dir.join(f).exists() {
                return Err(format!("Falta el modelo local de voz ({f}) en {}", dir.display()));
            }
        }
        let cfg = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(path("model.onnx")),
                    voices: Some(path("voices.bin")),
                    tokens: Some(path("tokens.txt")),
                    ..Default::default()
                },
                num_threads: 4,
                ..Default::default()
            },
            ..Default::default()
        };
        OfflineTts::create(&cfg).map(|t| Mutex::new(Engine(t))).ok_or_else(|| "No pude cargar el modelo de voz local.".to_string())
    })
}

/// 16-bit mono PCM WAV from float samples.
pub fn wav_from_f32(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    w
}

/// Load the model in the background at startup so the first sentence is not delayed.
pub fn preload(app: AppHandle) {
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        match engine(&app) {
            Ok(_) => eprintln!("[tts-local] modelo cargado en {:?}", t.elapsed()),
            Err(e) => eprintln!("[tts-local] {e}"),
        }
    });
}

/// Text -> speech, offline. Returns base64 WAV; errors tell the UI to fall back to the cloud voice.
#[tauri::command]
pub async fn tts_local(app: AppHandle, text: String) -> Result<String, String> {
    use base64::Engine as _;
    tauri::async_runtime::spawn_blocking(move || {
        let eng = engine(&app).as_ref().map_err(|e| e.clone())?;
        let ipa = crate::g2p_es::phonemize(&text);
        if ipa.is_empty() {
            return Err("Nada que decir.".to_string());
        }
        let cps: Vec<i32> = ipa.chars().map(|c| c as i32).collect();
        let cfg = GenerationConfig { sid: SPEAKER, speed: SPEED, phoneme_codepoints: Some(vec![cps]), ..Default::default() };
        let t = std::time::Instant::now();
        let audio = eng
            .lock()
            .map_err(|_| "motor de voz bloqueado".to_string())?
            .0
            .generate_with_config::<fn(&[f32], f32) -> bool>(&text, &cfg, None)
            .ok_or("El modelo no generó audio.")?;
        eprintln!("[tts-local] {:?} para {:.1}s de audio", t.elapsed(), audio.samples().len() as f32 / audio.sample_rate() as f32);
        Ok(base64::engine::general_purpose::STANDARD.encode(wav_from_f32(audio.samples(), audio.sample_rate() as u32)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_clamping() {
        let w = wav_from_f32(&[0.0, 1.5, -1.5], 24_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 24_000);
        assert_eq!(i16::from_le_bytes(w[46..48].try_into().unwrap()), 32767);
        assert_eq!(i16::from_le_bytes(w[48..50].try_into().unwrap()), -32767);
        assert_eq!(w.len(), 44 + 6);
    }
}
