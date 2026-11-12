import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type MediaState = {
  has_session: boolean;
  title: string;
  artist: string;
  app: string;
  playing: boolean;
  position_ms: number;
  duration_ms: number;
  thumb: string | null;
};

class Media {
  s = $state<MediaState>({ has_session: false, title: "", artist: "", app: "", playing: false, position_ms: 0, duration_ms: 0, thumb: null });
  thumb = $state<string | null>(null);
  stamp = 0; // performance.now() when `s` was received, to extrapolate the playhead
  #key = "";

  async init() {
    await listen<MediaState>("media-state", (e) => {
      const p = e.payload;
      const key = `${p.app}|${p.title}|${p.artist}`;
      if (key !== this.#key) {
        this.#key = key;
        this.thumb = p.thumb;
      } else if (p.thumb) this.thumb = p.thumb;
      this.s = p;
      this.stamp = performance.now();
    });
  }

  position(now = performance.now()) {
    const p = this.s.position_ms + (this.s.playing ? now - this.stamp : 0);
    return this.s.duration_ms > 0 ? Math.min(p, this.s.duration_ms) : p;
  }

  /** Jump to `ms`: update the playhead right away (optimistic), the player confirms on its next state event. */
  seek(ms: number) {
    this.s = { ...this.s, position_ms: ms };
    this.stamp = performance.now();
    invoke("media_seek", { positionMs: Math.round(ms) }).catch(() => {});
  }

  control(action: "play_pause" | "next" | "previous") {
    invoke("media_control", { action }).catch(() => {});
  }
}

export type PomoState = {
  phase: "focus" | "short_break" | "long_break";
  running: boolean;
  remaining_ms: number;
  total_ms: number;
  ends_at_ms: number | null;
  completed: number;
};

class Pomo {
  s = $state<PomoState>({ phase: "focus", running: false, remaining_ms: 25 * 60_000, total_ms: 25 * 60_000, ends_at_ms: null, completed: 0 });

  async init() {
    await listen<PomoState>("pomo-state", (e) => (this.s = e.payload));
    this.s = await invoke<PomoState>("pomo_control", { action: "status" });
  }

  remaining(now = Date.now()) {
    return this.s.running && this.s.ends_at_ms ? Math.max(0, this.s.ends_at_ms - now) : this.s.remaining_ms;
  }

  control(action: "start" | "pause" | "reset" | "skip" | "focus" | "short_break" | "long_break") {
    invoke("pomo_control", { action });
  }
}

export type Reminder = { id: number; text: string; due_at_ms: number; fired: boolean; done: boolean };

class Reminders {
  list = $state<Reminder[]>([]);

  async refresh() {
    this.list = await invoke<Reminder[]>("reminder_list");
  }

  async init() {
    await listen("reminders-changed", () => this.refresh());
    await this.refresh();
  }

  async add(text: string, dueAtMs: number) {
    await invoke("reminder_add", { text, dueAtMs });
  }

  async setDone(id: number, done: boolean) {
    await invoke("reminder_set_done", { id, done });
  }

  async remove(id: number) {
    await invoke("reminder_delete", { id });
  }
}

export type Alert = { kind: "reminder" | "pomodoro"; id?: number; title: string; body: string };

/** Shared 4 Hz clock so countdowns/playheads re-render without every component owning a timer. */
export const clock = $state({ now: Date.now(), perf: performance.now() });
if (typeof window !== "undefined") {
  setInterval(() => {
    clock.now = Date.now();
    clock.perf = performance.now();
  }, 250);
}

export const media = new Media();
export const pomo = new Pomo();
export const reminders = new Reminders();

export async function initStores(onAlert: (a: Alert) => void) {
  await Promise.all([media.init(), pomo.init(), reminders.init()]);
  await listen<Alert>("alert", (e) => onAlert(e.payload));
}
