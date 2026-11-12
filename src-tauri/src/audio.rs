//! Real audio levels for the music Live Activity: WASAPI loopback on the default output device (what you hear,
//! whatever app plays it), a 1024-point FFT, and 6 frequency bands (bass on the left .. treble on the right) sent to the UI.
//! The capture only runs while the UI asks for it (music activity visible on the closed notch).
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

static ENABLED: AtomicBool = AtomicBool::new(false);

const N: usize = 1024;
/// Band edges in Hz, roughly logarithmic: one band per bar, bass -> treble.
const BANDS: usize = 6;
const EDGES: [f32; BANDS + 1] = [40.0, 120.0, 320.0, 850.0, 2200.0, 5500.0, 14000.0];

#[tauri::command]
pub fn audio_meter_enable(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (i + k, i + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
            i += len;
        }
        len <<= 1;
    }
}

struct Analyzer {
    window: Vec<f32>,
    peak: [f32; BANDS],
}

impl Analyzer {
    fn new() -> Self {
        let window = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (N as f32 - 1.0)).cos()).collect();
        Self { window, peak: [1e-3; BANDS] }
    }

    /// `samples`: the latest N mono samples. Returns one level per band in 0..=1.
    fn levels(&mut self, samples: &[f32], rate: f32) -> [f32; BANDS] {
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        if rms < 1e-4 {
            for p in &mut self.peak {
                *p *= 0.9; // silence: let the auto-gain relax
            }
            return [0.0; BANDS];
        }
        let mut re: Vec<f32> = samples.iter().zip(&self.window).map(|(s, w)| s * w).collect();
        let mut im = vec![0.0f32; N];
        fft(&mut re, &mut im);
        let hz = rate / N as f32;
        let mut mags = [0.0f32; BANDS];
        for b in 0..BANDS {
            let lo = ((EDGES[b] / hz).ceil() as usize).max(1);
            let hi = ((EDGES[b + 1] / hz).floor() as usize).clamp(lo, N / 2 - 1);
            let mut e = 0.0;
            for k in lo..=hi {
                e += re[k] * re[k] + im[k] * im[k];
            }
            mags[b] = (e / (hi - lo + 1) as f32).sqrt() / (N as f32 / 4.0);
        }
        let loudest = mags.iter().cloned().fold(0.0f32, f32::max);
        let mut out = [0.0f32; BANDS];
        for b in 0..BANDS {
            let mag = mags[b];
            // per-band auto-gain: the loudest recent level of THIS band maps to full height, so quiet highs still move.
            // The floor (8% of the loudest band right now) stops a band that only holds leakage from being stretched to full.
            self.peak[b] = (self.peak[b] * 0.992).max(mag).max(loudest * 0.08).max(2e-3);
            let db = 20.0 * (mag / self.peak[b]).max(1e-6).log10(); // 0 at the band's own peak
            // 18 dB window + a curve: only genuine peaks reach the top, the rest of the groove sits lower (more contrast)
            let lv = ((db + 18.0) / 18.0).clamp(0.0, 1.0).powf(1.7);
            // quiet passages shouldn't look as lively as loud ones
            out[b] = lv * (rms * 14.0).clamp(0.25, 1.0);
        }
        out
    }
}

fn session(app: &AppHandle) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let f: WAVEFORMATEX = std::ptr::read_unaligned(fmt);
        let (channels, rate, bits) = (f.nChannels as usize, f.nSamplesPerSec as f32, f.wBitsPerSample);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 10_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        init?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let _ = app.emit("audio-levels", [0.0f32; BANDS]); // heartbeat: tells the UI the capture is alive
        let mut an = Analyzer::new();
        let mut ring: Vec<f32> = vec![0.0; N];
        let mut fresh = 0usize;
        let mut last = [0.0f32; BANDS];
        let mut last_packet = Instant::now();
        while ENABLED.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(8));
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
                    ring.push(acc / channels.max(1) as f32);
                }
                fresh += frames as usize;
                cap.ReleaseBuffer(frames)?;
            }
            if ring.len() > N * 4 {
                let cut = ring.len() - N;
                ring.drain(0..cut);
            }
            let step = (rate * 0.033) as usize;
            if fresh >= step && ring.len() >= N {
                fresh = 0;
                last_packet = Instant::now();
                last = an.levels(&ring[ring.len() - N..], rate);
                let _ = app.emit("audio-levels", last);
            } else if last_packet.elapsed() > Duration::from_millis(150) && last.iter().any(|v| *v > 0.0) {
                // loopback delivers no packets while nothing plays: drop to zero so the bars fall instead of freezing
                last = [0.0; BANDS];
                let _ = app.emit("audio-levels", last);
            }
        }
        let _ = client.Stop();
        let _ = app.emit("audio-levels", [0.0f32; BANDS]);
        Ok(())
    }
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut quiet_ticks = 0u32;
        loop {
            if !ENABLED.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            if session(&app).is_err() {
                // device changed / unavailable: retry shortly
                quiet_ticks += 1;
                std::thread::sleep(Duration::from_millis(500 * quiet_ticks.min(6) as u64));
            } else {
                quiet_ticks = 0;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, rate: f32) -> Vec<f32> {
        (0..N).map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin()).collect()
    }

    #[test]
    fn bass_tone_lights_the_bass_band() {
        let mut a = Analyzer::new();
        let l = a.levels(&tone(100.0, 48000.0), 48000.0);
        assert!(l[0] > 0.9, "{l:?}");
        assert!(l[BANDS - 1] < 0.2, "{l:?}");
    }

    #[test]
    fn tones_light_their_own_band_left_to_right() {
        // 100 Hz -> band 0 ... 400 Hz -> band 2 ... 5 kHz -> band 4 (bass on the left, treble on the right)
        for (hz, band) in [(100.0, 0usize), (400.0, 2), (5000.0, 4)] {
            let mut a = Analyzer::new();
            let l = a.levels(&tone(hz, 48000.0), 48000.0);
            let top = l.iter().cloned().enumerate().fold((0, 0.0f32), |m, (i, v)| if v > m.1 { (i, v) } else { m });
            assert_eq!(top.0, band, "{hz} Hz -> {l:?}");
        }
    }

    #[test]
    fn silence_is_zero() {
        let mut a = Analyzer::new();
        assert_eq!(a.levels(&vec![0.0; N], 48000.0), [0.0; BANDS]);
    }
}
