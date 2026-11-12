//! Pomodoro state machine. Runs in Rust so it keeps ticking while the notch is collapsed.
use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const FOCUS_MS: i64 = 25 * 60_000;
pub const SHORT_MS: i64 = 5 * 60_000;
pub const LONG_MS: i64 = 15 * 60_000;
pub const CYCLES_BEFORE_LONG: u32 = 4;

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
}

impl Phase {
    fn duration(self) -> i64 {
        match self {
            Phase::Focus => FOCUS_MS,
            Phase::ShortBreak => SHORT_MS,
            Phase::LongBreak => LONG_MS,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct PomoState {
    pub phase: Phase,
    pub running: bool,
    pub remaining_ms: i64,
    pub total_ms: i64,
    /// Wall-clock deadline (epoch ms) while running, so the UI can count down on its own.
    pub ends_at_ms: Option<i64>,
    /// Focus sessions completed in the current set of 4.
    pub completed: u32,
}

pub struct Pomo {
    phase: Phase,
    running: bool,
    remaining_ms: i64,
    ends_at_ms: Option<i64>,
    completed: u32,
}

impl Pomo {
    pub fn new() -> Self {
        Self { phase: Phase::Focus, running: false, remaining_ms: FOCUS_MS, ends_at_ms: None, completed: 0 }
    }

    pub fn state(&self) -> PomoState {
        PomoState {
            phase: self.phase,
            running: self.running,
            remaining_ms: self.remaining_ms,
            total_ms: self.phase.duration(),
            ends_at_ms: self.ends_at_ms,
            completed: self.completed,
        }
    }

    pub fn start(&mut self, now_ms: i64) {
        if !self.running {
            self.running = true;
            self.ends_at_ms = Some(now_ms + self.remaining_ms);
        }
    }

    pub fn pause(&mut self, now_ms: i64) {
        if self.running {
            self.remaining_ms = (self.ends_at_ms.unwrap_or(now_ms) - now_ms).max(0);
            self.running = false;
            self.ends_at_ms = None;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Switch straight to a phase (paused, full time). Keeps the completed-session count.
    pub fn select(&mut self, phase: Phase) {
        self.phase = phase;
        self.running = false;
        self.ends_at_ms = None;
        self.remaining_ms = phase.duration();
    }

    /// Jump to the next phase (paused), as after a natural finish.
    pub fn skip(&mut self) -> Phase {
        let finished = self.phase;
        match finished {
            Phase::Focus => {
                self.completed += 1;
                self.phase = if self.completed >= CYCLES_BEFORE_LONG { Phase::LongBreak } else { Phase::ShortBreak };
            }
            Phase::ShortBreak => self.phase = Phase::Focus,
            Phase::LongBreak => {
                self.phase = Phase::Focus;
                self.completed = 0;
            }
        }
        self.running = false;
        self.ends_at_ms = None;
        self.remaining_ms = self.phase.duration();
        finished
    }

    /// Returns the phase that just finished, if the deadline passed.
    pub fn tick(&mut self, now_ms: i64) -> Option<Phase> {
        match (self.running, self.ends_at_ms) {
            (true, Some(end)) if now_ms >= end => Some(self.skip()),
            _ => None,
        }
    }
}

pub struct PomoState_(pub Mutex<Pomo>);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn emit(app: &AppHandle, p: &Pomo) {
    let _ = app.emit("pomo-state", p.state());
}

pub fn control(app: &AppHandle, action: &str) -> Result<PomoState, String> {
    let state = app.state::<PomoState_>();
    let mut p = state.0.lock().unwrap();
    match action {
        "start" => p.start(now_ms()),
        "pause" => p.pause(now_ms()),
        "reset" => p.reset(),
        "skip" => {
            p.skip();
        }
        "focus" => p.select(Phase::Focus),
        "short_break" => p.select(Phase::ShortBreak),
        "long_break" => p.select(Phase::LongBreak),
        "status" => {}
        other => return Err(format!("acción desconocida: {other}")),
    }
    emit(app, &p);
    Ok(p.state())
}

#[tauri::command]
pub fn pomo_control(app: AppHandle, action: String) -> Result<PomoState, String> {
    control(&app, &action)
}

pub fn spawn_ticker(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let state = app.state::<PomoState_>();
        let mut p = state.0.lock().unwrap();
        if let Some(finished) = p.tick(now_ms()) {
            emit(&app, &p);
            let next = p.phase;
            drop(p);
            let (title, body) = match (finished, next) {
                (Phase::Focus, Phase::LongBreak) => ("Pomodoro completo", "Descanso largo de 15 min. Buen trabajo."),
                (Phase::Focus, _) => ("Foco terminado", "Descanso de 5 min."),
                _ => ("Descanso terminado", "Vuelve al foco cuando estés listo."),
            };
            let _ = app.emit("alert", serde_json::json!({ "kind": "pomodoro", "title": title, "body": body }));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_and_moves_to_break() {
        let mut p = Pomo::new();
        p.start(1_000);
        assert_eq!(p.tick(1_000 + FOCUS_MS - 1), None);
        assert_eq!(p.tick(1_000 + FOCUS_MS), Some(Phase::Focus));
        assert_eq!(p.state().phase, Phase::ShortBreak);
        assert!(!p.state().running);
        assert_eq!(p.state().remaining_ms, SHORT_MS);
    }

    #[test]
    fn pause_keeps_remaining_time() {
        let mut p = Pomo::new();
        p.start(0);
        p.pause(10_000);
        assert_eq!(p.state().remaining_ms, FOCUS_MS - 10_000);
        p.start(50_000);
        assert_eq!(p.state().ends_at_ms, Some(50_000 + FOCUS_MS - 10_000));
    }

    #[test]
    fn select_switches_phase_without_losing_progress_count() {
        let mut p = Pomo::new();
        p.skip(); // one focus done
        p.select(Phase::Focus);
        assert_eq!((p.state().phase, p.state().completed, p.state().running), (Phase::Focus, 1, false));
        p.start(0);
        p.select(Phase::ShortBreak);
        assert_eq!((p.state().remaining_ms, p.state().running), (SHORT_MS, false));
    }

    #[test]
    fn long_break_after_four_focus_sessions() {
        let mut p = Pomo::new();
        for i in 1..=4 {
            p.skip(); // finish focus
            if i < 4 {
                assert_eq!(p.state().phase, Phase::ShortBreak);
                p.skip(); // finish break
            }
        }
        assert_eq!(p.state().phase, Phase::LongBreak);
        p.skip();
        assert_eq!((p.state().phase, p.state().completed), (Phase::Focus, 0));
    }
}
