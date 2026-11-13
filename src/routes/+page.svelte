<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { cubicOut } from "svelte/easing";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { createSpring } from "$lib/spring";
  import { chat } from "$lib/chat.svelte";
  import { initStores, media, pomo, reminders, clock, type Alert } from "$lib/stores.svelte";
  import Chat from "$lib/components/Chat.svelte";
  import Music from "$lib/components/Music.svelte";
  import Meter from "$lib/components/Meter.svelte";
  import Pomodoro from "$lib/components/Pomodoro.svelte";
  import Reminders from "$lib/components/Reminders.svelte";

  type Mode = "idle" | "expanded";
  /** w,h: size. r: bottom corner radius. f: concave "shoulder" radius where the notch meets the screen edge. */
  type Size = { w: number; h: number; r: number; f: number };

  const WIN_W = 1000;
  const CX = WIN_W / 2;
  const WIN_H = 520;
  const SIZES = {
    idle: { w: 190, h: 31, r: 15, f: 8 },
    live: { w: 266, h: 31, r: 15, f: 8 }, // "compact" Live-Activity width: music playing / pomodoro running
    press: { w: 182, h: 29, r: 14, f: 8 },
    expanded: { w: 640, h: 232, r: 42, f: 24 },
  } satisfies Record<string, Size>;
  const SIDE = 13; // closed-notch content keeps this distance from the left/right edges (clear of the rounded corners)
  const LIFT = 4; // closed-notch content is lifted: a launcher bar above the notch makes the visible black start lower
  const BELLY_W = 0.22; // how much of the belly-vs-corner lag shows on the sides
  const BELLY_H = 0.3; // ...and on the bottom edge
  const MAX_H = 430; // the notch never grows past this; longer answers scroll inside
  const PANEL_CHROME = 84; // panel rows that aren't the page: header 46 + dots 18 + paddings 12
  const HIDDEN_Y = -80; // slid fully above the screen edge
  const BLEED = 60; // body extends above the screen edge so the top stays flush
  const PAGES = [
    { id: "chat", label: "Chat", title: "Asistente", color: "#e5e5ea" },
    { id: "music", label: "Música", title: "", color: "#ffffff" },
    { id: "pomodoro", label: "Pomodoro", title: "Pomodoro", color: "#ff3b4f" },
    { id: "reminders", label: "Recordatorios", title: "Recordatorios", color: "#30d158" },
  ] as const;

  const reduceMotion = typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  let mode: Mode = $state("idle");
  let hovered = $state(false);
  let pressed = $state(false);
  let visible = $state(true);
  // Voice-activated session: same notch, but translucent with a top-black -> bottom-clear gradient.
  let voice = $state(false);
  // Corner springs (w,h,r,f,y) plus a softer "belly" pair (w2,h2) that drives the edge midpoints.
  let box = $state({ w: SIZES.idle.w, h: SIZES.idle.h, r: SIZES.idle.r, f: SIZES.idle.f, y: 0, w2: SIZES.idle.w, h2: SIZES.idle.h });

  let wSpring: ReturnType<typeof createSpring<{ w: number }>>;
  let hSpring: ReturnType<typeof createSpring<{ h: number; r: number; f: number; y: number }>>;
  let bSpring: ReturnType<typeof createSpring<{ w2: number; h2: number }>>;
  let slide: ReturnType<typeof createSpring<{ x: number }>>;
  let lastSent = "";

  // --- Live Activities: one per module, shown on the closed notch while that module is "on" -------------
  type Activity = { id: "chat" | "music" | "pomodoro" | "reminders"; page: number; color: string; label: string; value: string };
  // The meter bars are "painted" with a heavily blurred copy of the cover: shrink it to a few pixels and let the browser
  // upscale it smoothly, so each bar picks up the colours of the song at its own position.
  let blurUrl = $state("");
  // look of the bars, edited from the tray menu (Barras): blur = cover shrunk to blur x blur px (smaller = stronger), the rest in %
  let bars = $state({ blur: 3, brightness: 135, saturation: 170, veil: 18 });
  $effect(() => {
    const src = media.thumb;
    const n = bars.blur;
    if (!src) {
      blurUrl = "";
      return;
    }
    const img = new Image();
    img.onload = () => {
      const tiny = document.createElement("canvas");
      tiny.width = tiny.height = n;
      tiny.getContext("2d")?.drawImage(img, 0, 0, n, n);
      const out = document.createElement("canvas");
      out.width = 66;
      out.height = 40;
      const cx = out.getContext("2d");
      if (!cx) return;
      cx.imageSmoothingEnabled = true;
      cx.imageSmoothingQuality = "high";
      cx.drawImage(tiny, 0, 0, 66, 40);
      blurUrl = out.toDataURL("image/png");
    };
    img.src = src;
  });
  const hhmm = (ms: number) => new Date(ms).toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit", hour12: false });
  // elapsed fraction of the current pomodoro phase (0 -> 1), for the filling ring
  const pomoProgress = $derived(pomo.s.total_ms > 0 ? Math.min(1, Math.max(0, 1 - pomo.remaining(clock.now) / pomo.s.total_ms)) : 0);
  const nextReminder = $derived(
    reminders.list
      .filter((r) => !r.done && r.due_at_ms > clock.now - 60_000 && r.due_at_ms < clock.now + 24 * 3600_000)
      .sort((a, b) => a.due_at_ms - b.due_at_ms)[0],
  );
  const acts = $derived.by(() => {
    const out: Activity[] = [];
    if (chat.busy) out.push({ id: "chat", page: 0, color: "#e5e5ea", label: "Respondiendo…", value: "" });
    if (media.s.playing)
      out.push({ id: "music", page: 1, color: "#ffffff", label: [media.s.title, media.s.artist].filter(Boolean).join(" · ") || "Reproduciendo", value: "" });
    if (pomo.s.running)
      out.push({
        id: "pomodoro",
        page: 2,
        color: pomo.s.phase === "focus" ? "#ff3b4f" : "#30d158", // red = focus, green = break
        label: pomo.s.phase === "focus" ? "Foco" : "Descanso",
        value: mmss(pomo.remaining(clock.now)),
      });
    if (nextReminder)
      out.push({ id: "reminders", page: 3, color: "#30d158", label: nextReminder.text, value: hhmm(nextReminder.due_at_ms) });
    return out;
  });
  // The closed notch is a little carousel too: one slot per ACTIVE activity plus a final empty slot ("none") to dismiss them.
  let sel = $state("");
  const slots = $derived(acts.length ? [...acts.map((a) => a.id as string), "none"] : []);
  const cur = $derived(sel === "none" ? undefined : (acts.find((a) => a.id === sel) ?? acts[0]));
  const live = $derived(cur !== undefined);
  const curIdx = $derived(cur ? slots.indexOf(cur.id) : Math.max(0, slots.length - 1));
  let knownIds: string[] = [];
  $effect(() => {
    const ids = acts.map((a) => a.id as string);
    const added = ids.filter((id) => !knownIds.includes(id));
    if (added.length) sel = added[0]; // something just switched on: show it (even if you had dismissed the others)
    else if (sel !== "none" && sel && !ids.includes(sel)) sel = ids[0] ?? "";
    knownIds = ids;
  });
  function stepAct(d: number) {
    if (slots.length < 2) return;
    sel = slots[Math.max(0, Math.min(slots.length - 1, curIdx + d))];
  }


  const now = $derived(new Date(clock.now));
  const dateStr = $derived(
    now.toLocaleDateString("es", { weekday: "short", day: "numeric", month: "short" }).replace(/\./g, ""),
  );
  const timeStr = $derived(now.toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit", hour12: false }));

  // --- carousel -------------------------------------------------------------
  let index = $state(0);
  let tx = $state(0);
  let pageW = $state(0);
  let chatNeeded = $state(0); // px the chat page wants to show everything without scrolling
  let chatRef: ReturnType<typeof Chat> | undefined = $state();
  let dragging = false;
  let banner = $state("");
  let bannerTimer: ReturnType<typeof setTimeout>;

  const maxIndex = PAGES.length - 1;
  const clampIndex = (i: number) => Math.max(0, Math.min(maxIndex, i));
  /**
   * Expanded height. Only the chat page stretches (long answers); any other module gets the base height,
   * so switching modules makes the notch shrink back (or grow again) through the same springs.
   */
  const expandedH = $derived(
    index === 0 && chatNeeded > 0
      ? Math.round(Math.min(MAX_H, Math.max(SIZES.expanded.h, chatNeeded + PANEL_CHROME)))
      : SIZES.expanded.h,
  );
  // Which page is "in front" while swiping (fractional), used to cross-fade the accent glow.
  const pos = $derived(pageW > 0 ? -tx / pageW : index);
  const weight = (i: number) => Math.max(0, 1 - Math.abs(pos - i));

  // --- real audio meter (WASAPI loopback in Rust): 6 band levels, one per bar, bass on the left -> treble on the right -------------------------
  let meter = $state([0, 0, 0, 0, 0, 0]); // smoothed levels actually drawn
  let meterAlive = $state(false); // false until the first event arrives -> CSS fallback animation
  let meterTarget = [0, 0, 0, 0, 0, 0];
  // the meter is drawn on the closed notch (music activity) and on the open Music page; capture only runs while one is visible
  const meterOn = $derived((mode === "idle" && visible && cur?.id === "music") || ((mode as Mode) === "expanded" && index === 1 && media.s.playing));
  $effect(() => {
    const on = meterOn;
    invoke("audio_meter_enable", { on }).catch(() => {});
    if (!on) {
      meterAlive = false;
      meterTarget = [0, 0, 0, 0, 0, 0];
      meter = [0, 0, 0, 0, 0, 0];
      return;
    }
    let raf = 0;
    const tick = () => {
      // fast attack, slower release: reads as a level meter, not as jitter
      meter = meter.map((v, i) => v + (meterTarget[i] - v) * (meterTarget[i] > v ? 0.6 : 0.16));
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  /** Used when the notch is opening: land on the page instantly instead of sliding across the other modules. */
  function jumpTo(i: number) {
    const target = (index = clampIndex(i));
    const pin = () => {
      // pageW is 0 until the panel has been laid out for the first time, so re-pin once it is known.
      if (slide && !dragging && index === target && pageW > 0) slide.set({ x: -target * pageW });
    };
    pin();
    requestAnimationFrame(() => requestAnimationFrame(pin));
    setTimeout(pin, 120);
    setTimeout(pin, 400);
  }

  function goTo(i: number) {
    index = clampIndex(i);
    slide.to({ x: -index * pageW });
  }

  // While the window grows/shrinks, page width changes: keep the current page pinned.
  $effect(() => {
    if (slide && !dragging) slide.set({ x: -index * pageW });
  });

  // apple-design: rubber-band past the ends instead of a hard stop.
  function rubberband(over: number, dim: number, c = 0.55) {
    return (over * dim * c) / (dim + c * Math.abs(over));
  }

  let drag: { x0: number; base: number; lastX: number; lastT: number; vx: number; active: boolean } | null = null;

  function onPointerDown(e: PointerEvent) {
    if ((e.target as HTMLElement).closest("input, textarea, button, a, [role=slider]")) return;
    drag = { x0: e.clientX, base: tx, lastX: e.clientX, lastT: performance.now(), vx: 0, active: false };
  }

  function onPointerMove(e: PointerEvent) {
    if (!drag) return;
    const dx = e.clientX - drag.x0;
    if (!drag.active && Math.abs(dx) > 10) {
      drag.active = true;
      dragging = true;
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    }
    if (!drag.active) return;
    const now = performance.now();
    const inst = ((e.clientX - drag.lastX) / Math.max(1, now - drag.lastT)) * 1000;
    drag.vx = drag.vx * 0.6 + inst * 0.4; // smoothed release velocity
    drag.lastX = e.clientX;
    drag.lastT = now;

    let x = drag.base + dx;
    const min = -maxIndex * pageW;
    if (x > 0) x = rubberband(x, pageW);
    else if (x < min) x = min - rubberband(min - x, pageW);
    slide.set({ x });
  }

  function onPointerUp() {
    if (!drag) return;
    const d = drag;
    drag = null;
    dragging = false;
    if (!d.active) return;
    // Project where the flick is heading, snap to the nearest page, hand the velocity to the spring.
    const projected = tx + d.vx * 0.15;
    index = clampIndex(Math.round(-projected / pageW));
    slide.to({ x: -index * pageW }, { x: d.vx });
  }

  // Measured on a real trackpad (WebView2): one swipe is a burst of 20-60 px/event that decays smoothly into a tail of
  // 1-5 px events arriving every ~8 ms for SECONDS, and the tail only stops when the mouse moves. So "wait for events to
  // stop" never unlocks. Instead, after a page change, a new swipe is recognised by its signature: a sudden jump to
  // >= 3x the recent minimum (the tail never jumps), or a reversal of direction.
  // `axis: "any"` also accepts a plain vertical mouse wheel (used on the closed notch).
  function makeSwipe(step: (dir: number) => void, axis: "x" | "any") {
  let wheelAcc = { x: 0, y: 0, last: 0 };
  let wheel = { dir: 0, lockUntil: 0, lastT: 0, recent: [] as number[] };
  return function onWheel(e: WheelEvent) {
    if ((e.target as HTMLElement).closest?.("[role=slider]")) return;
    const now = performance.now();
    const dX = axis === "any" && Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
    const dY = axis === "any" ? 0 : e.deltaY;
    const a = Math.abs(dX);
    if (wheel.dir !== 0) {
      const gap = now - wheel.lastT;
      wheel.lastT = now;
      const floor = wheel.recent.length >= 3 ? Math.max(1, Math.min(...wheel.recent.slice(-8))) : Infinity;
      const sign = Math.sign(dX);
      const free = now >= wheel.lockUntil; // never within 450 ms of a page change: the burst itself is noisy
      const fresh = gap > 130 || (free && a >= 15 && a >= 3 * floor) || (free && sign !== 0 && sign !== wheel.dir && a >= 10);
      if (a > 0) wheel.recent.push(a);
      if (wheel.recent.length > 16) wheel.recent.shift();
      if (!fresh) return;
      wheel = { dir: 0, lockUntil: 0, lastT: now, recent: [] };
      wheelAcc = { x: 0, y: 0, last: now };
    }
    if (now - wheelAcc.last > 140) wheelAcc = { x: 0, y: 0, last: now };
    wheelAcc.x += dX;
    wheelAcc.y += dY;
    wheelAcc.last = now;
    const ax = Math.abs(wheelAcc.x);
    if (ax < 24 || ax < Math.abs(wheelAcc.y) * 1.2) return;
    const dir = Math.sign(wheelAcc.x);
    wheelAcc = { x: 0, y: 0, last: now };
    wheel = { dir, lockUntil: now + 450, lastT: now, recent: [] };
    step(dir);
  };
  }
  const carouselSwipe = makeSwipe((d) => goTo(index + d), "x");
  const idleSwipe = makeSwipe(stepAct, "any");

  // --- notch shape ----------------------------------------------------------
  /**
   * One closed path = the whole notch. Corners are "continuous" (squircle-ish) like Apple's, the top has
   * concave shoulders that blend into the screen edge (MacBook notch), and the edge MIDPOINTS (w2,h2) follow
   * a softer spring than the corners (w,h): that lag is what reads as a wobbling liquid belly.
   */
  function shapePath(b: typeof box): string {
    const f = Math.min(b.f, b.h * 0.5);
    const L = Math.max(4, Math.min(b.r * 1.18, b.h - f - 1, b.w / 2 - 2)); // corner extent
    const xl = CX - b.w / 2;
    const xr = CX + b.w / 2;
    // Belly offsets are damped (the raw soft-vs-stiff spring gap is huge for a 190 -> 720 px jump), and the sides
    // may only pinch a few px: a deeper pinch reads as a notch eating into the shoulders.
    const y0 = f;
    const y1 = b.h - L;
    // Swell is also capped by the straight part of the side: on a short pill, a bulge right under the shoulder reads as an "ear".
    const side = Math.max(0, y1 - y0);
    const sw = Math.max(-side * 0.06, Math.min(((b.w2 - b.w) / 2) * BELLY_W, side * 0.14)); // positive = bulging outward
    const bm = b.h + (b.h2 - b.h) * BELLY_H; // bottom-edge midpoint y
    const ym = (y0 + y1) / 2;
    const a = (ym - y0) * 0.55;
    const half = (xr - L - (xl + L)) / 2;
    const c = half * 0.5;
    const k = 0.28; // squircle handle (a circle would be ~0.447)
    const n = (v: number) => v.toFixed(2);
    // Every join below has a shared tangent (vertical on the sides, horizontal on the bottom) so no kink can
    // appear where the swell meets the shoulders or corners: a kink there showed up as a white sliver while closing.
    return [
      `M${n(xl - f)} ${-BLEED}`,
      `L${n(xr + f)} ${-BLEED}`,
      `L${n(xr + f)} 0`,
      `A${n(f)} ${n(f)} 0 0 0 ${n(xr)} ${n(f)}`, // right shoulder (concave)
      `C${n(xr)} ${n(y0 + a)} ${n(xr + sw)} ${n(ym - a)} ${n(xr + sw)} ${n(ym)}`,
      `C${n(xr + sw)} ${n(ym + a)} ${n(xr)} ${n(y1 - a)} ${n(xr)} ${n(y1)}`,
      `C${n(xr)} ${n(b.h - L * k)} ${n(xr - L * k)} ${n(b.h)} ${n(xr - L)} ${n(b.h)}`, // bottom-right corner
      `C${n(xr - L - c)} ${n(b.h)} ${n(CX + c)} ${n(bm)} ${n(CX)} ${n(bm)}`,
      `C${n(CX - c)} ${n(bm)} ${n(xl + L + c)} ${n(b.h)} ${n(xl + L)} ${n(b.h)}`,
      `C${n(xl + L * k)} ${n(b.h)} ${n(xl)} ${n(b.h - L * k)} ${n(xl)} ${n(y1)}`, // bottom-left corner
      `C${n(xl)} ${n(y1 - a)} ${n(xl - sw)} ${n(ym + a)} ${n(xl - sw)} ${n(ym)}`,
      `C${n(xl - sw)} ${n(ym - a)} ${n(xl)} ${n(y0 + a)} ${n(xl)} ${n(f)}`,
      `A${n(f)} ${n(f)} 0 0 0 ${n(xl - f)} 0`, // left shoulder (concave)
      "Z",
    ].join(" ");
  }
  const d = $derived(shapePath(box));
  // Translucent look (solid black at the top edge, clearing toward the bottom): voice mode and every module
  const fadeOn = $derived(voice || (mode as Mode) === "expanded");

  function report(v: typeof box) {
    const gone = v.y < HIDDEN_Y / 2; // mostly slid away: stop capturing the mouse
    const w = Math.max(v.w, v.w2);
    const x = CX - w / 2;
    const h = Math.max(v.h, v.h2);
    const key = gone ? "gone" : `${x.toFixed(0)}|${v.y.toFixed(0)}|${w.toFixed(0)}|${h.toFixed(0)}|${v.r.toFixed(0)}`;
    if (key === lastSent) return;
    lastSent = key;
    invoke("set_pill_rect", gone ? { x: 0, y: 0, w: 0, h: 0 } : { x, y: v.y, w, h, r: v.r * 1.18 });
  }

  function currentTarget(): Size {
    if (mode === "expanded") return { ...SIZES.expanded, h: expandedH };
    // The press-shrink is only feedback for the plain pill. With a Live Activity showing (already hovered/widened) it made the notch
    // collapse and then re-open on click; there it must open straight away.
    if (pressed && !live) return SIZES.press;
    const base = live ? SIZES.live : SIZES.idle;
    // Hovering a Live Activity opens it up: room for the label (the page dots live inline, so the height barely changes and the margins stay even).
    if (hovered && live) return { w: 380, h: base.h + 4, r: base.r + 4, f: base.f };
    return hovered ? { w: base.w + 22, h: base.h + 4, r: base.r + 2, f: base.f } : base;
  }

  function sync() {
    const t = currentTarget();
    const y = mode === "idle" && !visible ? HIDDEN_Y : 0;
    wSpring.to({ w: t.w });
    hSpring.to({ h: t.h, r: t.r, f: t.f, y });
    bSpring.to({ w2: t.w, h2: t.h });
  }

  $effect(() => {
    live; // re-target when the Live-Activity state flips
    slots.length;
    expandedH; // ...or when the expanded height changes (long answer / module switch)
    if (wSpring) sync();
  });

  let snapEl = $state<HTMLCanvasElement>();
  /** Grab the screen under the window (taken before the notch grows, so it only holds the desktop) for the blurred backdrop. */
  // Live backdrop: only the translucent last quarter of the body shows the desktop, so only that band is sampled. The Rust side
  // skips unchanged frames and the loop is paced to the display (rAF), so a still desktop is nearly free.
  let snapRect = $state({ x: 0, y: 0, w: 0, h: 0 });
  $effect(() => {
    if (!fadeOn || !snapEl) return;
    let alive = true;
    let seen = 0;
    const ctx = untrack(() => snapEl!.getContext("2d", { alpha: false }));
    const loop = async () => {
      if (!alive) return;
      const { w, h, y } = untrack(() => ({ w: Math.max(box.w, box.w2), h: Math.max(box.h, box.h2), y: box.y })); // read, don't subscribe
      if (w > 60 && y + h > 0) {
        try {
          const buf = (await invoke("backdrop_snapshot", {
            x: Math.floor(CX - w / 2 - 6),
            y: Math.floor(y + h * 0.66),
            w: Math.ceil(w + 12),
            h: Math.ceil(h * 0.34 + 6),
            seen,
          })) as ArrayBuffer;
          const dv = new DataView(buf);
          const n = Number(dv.getBigUint64(0, true));
          if (alive && n > seen && ctx) {
            const cw = dv.getInt32(8, true);
            const ch = dv.getInt32(12, true);
            if (snapEl!.width !== cw || snapEl!.height !== ch) {
              snapEl!.width = cw;
              snapEl!.height = ch;
            }
            ctx.putImageData(new ImageData(new Uint8ClampedArray(buf, 32, cw * ch * 4), cw, ch), 0, 0);
            snapRect = { x: dv.getInt32(16, true), y: dv.getInt32(20, true), w: dv.getInt32(24, true), h: dv.getInt32(28, true) };
            seen = n; // the canvas sits exactly where its pixels came from, so it never slides against the desktop
          }
        } catch {
          /* no backdrop this frame */
        }
      }
      if (alive) requestAnimationFrame(loop);
    };
    loop();
    return () => {
      alive = false;
    };
  });

  function setMode(next: Mode) {
    const wasExpanded = mode === "expanded";
    mode = next;
    if (next === "idle") {
      voice = false;
      chat.cancelVoice(); // closing the notch ends any listening / speaking
    }
    invoke("set_in_use", { inUse: next === "expanded" });
    sync();
    if (next === "expanded") {
      if (!wasExpanded && chat.voice === "idle") chat.clear(); // every opening starts an empty chat (no-op while an answer is streaming)
      invoke("focus_window");
      // keep trying until the field really owns the keyboard (the window may take a moment to become active)
      for (const ms of [60, 160, 320, 600, 1000]) setTimeout(() => index === 0 && mode === "expanded" && chatRef?.focus(), ms);
    }
  }
  function openFromIdle() {
    if (mode !== "idle") return;
    const page = cur?.page ?? 0; // the module of the activity shown on the closed notch; the empty slot -> assistant chat
    toggle();
    jumpTo(page);
  }
  const toggle = () => setMode(mode === "idle" ? "expanded" : "idle");

  // Reminders that fired and wait for an answer (shown as a card over the modules until dismissed)
  let ringing = $state<{ id: number; text: string }[]>([]);
  const ring = $derived(ringing[0]);

  /** Two-note bell made with Web Audio (no asset to ship). */
  function chime() {
    try {
      const ac = new AudioContext();
      const t0 = ac.currentTime;
      [[880, 0], [1318.5, 0.16], [880, 0.9], [1318.5, 1.06]].forEach(([f, at]) => {
        const o = ac.createOscillator();
        const g = ac.createGain();
        o.type = "sine";
        o.frequency.value = f;
        g.gain.setValueAtTime(0.0001, t0 + at);
        g.gain.exponentialRampToValueAtTime(0.22, t0 + at + 0.02);
        g.gain.exponentialRampToValueAtTime(0.0001, t0 + at + 0.75);
        o.connect(g).connect(ac.destination);
        o.start(t0 + at);
        o.stop(t0 + at + 0.8);
      });
      setTimeout(() => ac.close(), 2500);
    } catch {
      /* audio unavailable */
    }
  }
  function ringDone() {
    if (!ring) return;
    reminders.setDone(ring.id, true);
    ringing = ringing.slice(1);
    if (!ringing.length) setMode("idle"); // answered: the notch goes back to its closed state
  }
  function ringSnooze() {
    if (!ring) return;
    reminders.add(ring.text, Date.now() + 5 * 60_000);
    reminders.setDone(ring.id, true);
    ringing = ringing.slice(1);
    if (!ringing.length) setMode("idle");
  }

  function onAlert(a: Alert) {
    if (a.kind === "reminder" && a.id != null) {
      ringing = [...ringing, { id: a.id, text: a.body }];
      chime();
      setMode("expanded");
      return;
    }
    banner = a.body ? `${a.title}: ${a.body}` : a.title;
    clearTimeout(bannerTimer);
    bannerTimer = setTimeout(() => (banner = ""), 8000);
    const wasOpen = mode === "expanded";
    setMode("expanded");
    (wasOpen ? goTo : jumpTo)(a.kind === "pomodoro" ? 2 : 3);
  }

  /** Dynamic-Island content swap: blur + scale + fade, anchored at the top edge of the notch. */
  function morph(_node: Element, { delay = 0, duration = 280 }: { delay?: number; duration?: number } = {}) {
    return {
      delay,
      duration,
      easing: cubicOut,
      css: (t: number) => `opacity:${t};filter:blur(${((1 - t) * 12).toFixed(1)}px);transform:scale(${(0.88 + 0.12 * t).toFixed(3)})`,
    };
  }

  onMount(() => {
    // Corners: Apple-like response ~0.5 s, damping ratio ~0.8 => ~1.5% overshoot, then settle.
    wSpring = createSpring({ w: SIZES.idle.w }, (v) => {
      box.w = v.w;
      report(box);
    }, reduceMotion ? { stiffness: 220, damping: 30 } : { stiffness: 175, damping: 21 });
    hSpring = createSpring({ h: SIZES.idle.h, r: SIZES.idle.r, f: SIZES.idle.f, y: 0 }, (v) => {
      box.h = v.h;
      box.r = v.r;
      box.f = v.f;
      box.y = Math.min(0, v.y); // an overshoot below 0 lifted the element's top edge off the cursor parked at y=0 -> mouseleave
      report(box);
    }, reduceMotion ? { stiffness: 220, damping: 30 } : { stiffness: 150, damping: 19 });
    // Belly: same targets, much softer + underdamped (ratio ~0.58). The gap between belly and corners IS the jelly.
    bSpring = createSpring({ w2: SIZES.idle.w, h2: SIZES.idle.h }, (v) => {
      box.w2 = v.w2;
      box.h2 = v.h2;
      report(box);
    }, reduceMotion ? { stiffness: 220, damping: 30 } : { stiffness: 110, damping: 12.2 });
    // Page slide: near-critical damping (no bounce unless the flick carried momentum).
    slide = createSpring({ x: 0 }, (v) => (tx = v.x), { stiffness: 260, damping: 30 });
    report(box);
    chat.init();
    if (import.meta.env.DEV) (window as unknown as { __chat: typeof chat }).__chat = chat; // dev-only test hook
    initStores(onAlert);

    const subs = [
      listen<typeof bars>("bar-settings", (e) => (bars = e.payload)),
      listen<number[]>("audio-levels", (e) => {
        meterTarget = e.payload;
        meterAlive = true;
      }),
      listen<boolean>("notch-visible", (e) => {
        visible = e.payload;
        sync();
      }),
      // Wake word heard (or tray test): open straight into the chat in the translucent voice look.
      listen<boolean>("voice-active", (e) => {
        voice = e.payload;
        if (voice) {
          const wasOpen = mode === "expanded";
          setMode("expanded");
          (wasOpen ? goTo : jumpTo)(0);
        }
      }),
      // Rust detects clicks outside the notch (the window is click-through there) and reports them.
      listen("outside-click", () => mode === "expanded" && setMode("idle")),
    ];
    invoke<typeof bars>("get_bar_settings").then((s) => (bars = s)).catch(() => {});
    invoke<boolean>("notch_visible").then((v) => {
      visible = v;
      sync();
    });

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && mode === "expanded") return toggle();
      if (mode !== "expanded" || (e.key !== "ArrowLeft" && e.key !== "ArrowRight")) return;
      const typing = (e.target as HTMLElement)?.matches?.("input, textarea");
      if (typing && !e.ctrlKey) return;
      e.preventDefault();
      goTo(index + (e.key === "ArrowRight" ? 1 : -1));
    };
    window.addEventListener("keydown", onKey);
    // Listen on the window, not only on the page strip: the swipe works anywhere over the open notch (header, dots, gaps),
    // and keeps working right after the page under the cursor changed.
    const onWin = (e: WheelEvent) => mode === "expanded" && carouselSwipe(e);
    window.addEventListener("wheel", onWin, { passive: true });
    return () => {
      wSpring.destroy();
      hSpring.destroy();
      bSpring.destroy();
      slide.destroy();
      subs.forEach((s) => s.then((un) => un()));
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("wheel", onWin);
    };
  });

  const mmss = (ms: number) => {
    const s = Math.ceil(ms / 1000);
    return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  };
</script>

<div class="stage" style="opacity:{Math.max(0, 1 + box.y / 60)}">
  <!-- Lightly blurred snapshot of what is behind the notch, clipped to the exact notch shape (only for the translucent look) -->
  <div class="backdrop" class:on={fadeOn} style="clip-path:path('{d}');transform:translateY({box.y}px)" aria-hidden="true">
    <canvas bind:this={snapEl} width="1" height="1" style="left:{snapRect.x}px;top:{snapRect.y - box.y}px;width:{snapRect.w}px;height:{snapRect.h}px"></canvas>
  </div>

  <!-- The body. Pure black, slightly translucent, no shadow. -->
  <svg class="shape" class:voice={fadeOn} width={WIN_W} height={WIN_H} viewBox="0 0 {WIN_W} {WIN_H}" aria-hidden="true" style="transform:translateY({box.y}px)">
    <defs>
      <!-- userSpaceOnUse so the fade always spans the notch's current height: solid black at the top edge, clearing toward the bottom -->
      <linearGradient id="voiceFade" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="0" y2={Math.max(box.h, box.h2, 40)}>
        <stop offset="0" stop-color="#000" stop-opacity="1" />
        <stop offset="0.75" stop-color="#000" stop-opacity="1" />
        <stop class="fadeEnd" class:on={fadeOn} offset="1" stop-color="#000" />
      </linearGradient>
    </defs>
    <!-- One layer: the gradient's last stop animates from solid black to see-through, so there is never a half-transparent crossfade dip -->
    <path {d} fill="url(#voiceFade)" />
  </svg>

  <!-- Everything interactive is clipped to the exact same path, so content can never poke out of the liquid edge. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="hit"
    class:open={mode === "expanded"}
    style="clip-path:path('{d}');transform:translateY({box.y}px)"
    role={mode === "idle" ? "button" : "dialog"}
    tabindex={mode === "idle" ? 0 : -1}
    aria-label="OpenNotch"
    onmouseenter={() => {
      hovered = true;
      sync();
    }}
    onmouseleave={() => {
      hovered = pressed = false;
      sync();
    }}
    onpointerdown={() => {
      if (mode !== "idle") return;
      pressed = true;
      sync();
    }}
    onpointerup={() => {
      pressed = false;
      sync();
    }}
    onwheel={(e) => mode === "idle" && idleSwipe(e)}
    onclick={openFromIdle}
    onkeydown={(e) => mode === "idle" && (e.key === "Enter" || e.key === " ") && openFromIdle()}
  >
    <!-- very faint gray liquid sheen that drifts inside the body (the reference is otherwise pure black) -->
    <div class="surface" style="left:{CX - box.w / 2}px;width:{box.w}px;height:{box.h}px">
      <div class="sheen" class:on={mode === "expanded"}><i></i><i></i><i></i></div>
    </div>

    {#if mode === "idle"}
      <div class="slot idle" style="left:{CX - box.w / 2}px;width:{box.w}px;height:{box.h}px" in:morph={{ delay: 120, duration: 260 }} out:morph={{ duration: 120 }}>
        {#if cur}
          {#key cur.id}
            <div class="glance" class:wide={hovered} class:music={cur.id === "music"} class:pomo={cur.id === "pomodoro"} style="--c:{cur.color};--m:{Math.max(2, (box.h - LIFT - 22) / 2).toFixed(2)}px;height:{box.h}px;padding-bottom:{LIFT}px;padding-left:{SIDE}px;padding-right:{SIDE}px" aria-hidden="true" in:morph={{ duration: 220 }}>
              <span class="ico">
                {#if cur.id === "music"}
                  <span class="cover">{#if media.thumb}<img src={media.thumb} alt="" draggable="false" />{:else}♪{/if}</span>
                {:else if cur.id === "chat"}
                  <span class="typing"><i></i><i></i><i></i></span>
                {:else if cur.id === "pomodoro"}
                  <svg class="pring" viewBox="0 0 24 24" aria-hidden="true">
                    <circle cx="12" cy="12" r="9" class="pt" />
                    <circle cx="12" cy="12" r="9" class="pf" stroke-dasharray="56.55" stroke-dashoffset={56.55 * (1 - pomoProgress)} />
                  </svg>
                {:else}
                  <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M6 17V11a6 6 0 1 1 12 0v6l1.5 2h-15zM10 21h4" /></svg>
                {/if}
              </span>
              <span class="lbl">{cur.label}</span>
              {#if slots.length > 1}
                <span class="adots">
                  {#each slots as id, i}<i class:cur={i === curIdx} style="--c:{acts.find((a) => a.id === id)?.color ?? '#ffffff'}"></i>{/each}
                </span>
              {/if}
              <span class="val">
                {#if cur.id === "music"}<Meter levels={meter} alive={meterAlive} bg={blurUrl ? `url(${blurUrl})` : ""} fx={bars} />{:else}{cur.value}{/if}
              </span>
            </div>
          {/key}
        {/if}
        {#if !cur && slots.length > 1}
          <div class="adots solo" class:on={hovered} aria-hidden="true">
            {#each slots as id, i}<i class:cur={i === curIdx} style="--c:{acts.find((a) => a.id === id)?.color ?? '#ffffff'}"></i>{/each}
          </div>
        {/if}
      </div>
    {:else}
      <div class="slot panelslot" style="left:{CX - SIZES.expanded.w / 2}px;width:{SIZES.expanded.w}px;height:{expandedH}px" in:morph={{ delay: 140, duration: 320 }} out:morph={{ duration: 110 }}>
        <div class="panel" class:voice={fadeOn}>
          <div class="top">
            <div class="titles">
              {#each PAGES as p, i}<span style="opacity:{weight(i)}">{p.title}</span>{/each}
            </div>
            <div class="status">
              <svg class="ring" viewBox="0 0 24 24" aria-hidden="true">
                <circle cx="12" cy="12" r="9" class="rt" />
                <circle cx="12" cy="12" r="9" class="rf" stroke-dasharray="56.55" stroke-dashoffset={56.55 * (1 - ((now.getSeconds() + 1) / 60))} />
              </svg>
              <span class="date">{dateStr}</span>
              <b>{timeStr}</b>
            </div>
            {#if banner}<div class="banner">{banner}</div>{/if}
          </div>

          {#if ring}
            <div class="ringcard" role="alertdialog" aria-label="Recordatorio" in:morph={{ duration: 260 }}>
              <div class="bell"><span class="pulse"></span><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3.5a5.5 5.5 0 0 0-5.5 5.5v3.2c0 .9-.3 1.8-.9 2.5L4.8 16.5h14.4l-.8-1.3a4.5 4.5 0 0 1-.9-2.5V9A5.5 5.5 0 0 0 12 3.5zM10 19a2 2 0 0 0 4 0" /></svg></div>
              <div class="rlabel">Recordatorio{ringing.length > 1 ? ` · ${ringing.length} pendientes` : ""}</div>
              <div class="rtext">{ring.text}</div>
              <div class="ractions">
                <button class="rbtn ghost" onclick={ringSnooze}>Posponer 5 min</button>
                <button class="rbtn" onclick={ringDone}>Listo</button>
              </div>
            </div>
          {/if}

          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="pages"
            bind:clientWidth={pageW}
            onpointerdown={onPointerDown}
            onpointermove={onPointerMove}
            onpointerup={onPointerUp}
            onpointercancel={onPointerUp}
          >
            <div class="track" style="transform:translate3d({tx}px,0,0)">
              <section class="page" style="--accent:{PAGES[0].color}" aria-hidden={index !== 0}><Chat bind:this={chatRef} listening={voice} bind:needed={chatNeeded} /></section>
              <section class="page" style="--accent:{PAGES[1].color}" aria-hidden={index !== 1}><Music levels={meter} alive={meterAlive} bg={blurUrl ? `url(${blurUrl})` : ""} fx={bars} /></section>
              <section class="page" style="--accent:{PAGES[2].color}" aria-hidden={index !== 2}><Pomodoro /></section>
              <section class="page" style="--accent:{PAGES[3].color}" aria-hidden={index !== 3}><Reminders /></section>
            </div>
          </div>

          <div class="dots" role="tablist" aria-label="Módulos">
            {#each PAGES as p, i}
              <button role="tab" aria-selected={index === i} aria-label={p.label} class:on={index === i} onclick={() => goTo(i)}></button>
            {/each}
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  :global(html, body) {
    margin: 0;
    background: transparent;
    overflow: hidden;
    user-select: none;
    -webkit-font-smoothing: antialiased;
    font-family: "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  }
  :global(.field) {
    all: unset;
    box-sizing: border-box;
    flex: none;
    width: 100%;
    padding: 11px 16px;
    border-radius: 14px;
    background: rgba(255, 255, 255, 0.12);
    color: #fff;
    font-size: 15px;
    transition: background 120ms, box-shadow 120ms;
  }
  :global(.field:focus) {
    background: rgba(255, 255, 255, 0.17);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent, #0a84ff) 80%, transparent);
  }
  :global(.field::placeholder) {
    color: #9a9aa3;
  }
  .stage {
    position: fixed;
    inset: 0;
  }
  .shape .fadeEnd {
    stop-opacity: 1;
    transition: stop-opacity 420ms ease;
  }
  .shape .fadeEnd.on {
    stop-opacity: 0.03;
  }
  .shape {
    position: absolute;
    left: 0;
    top: 0;
    overflow: visible;
    pointer-events: none;
    will-change: transform;
  }
  .hit {
    position: absolute;
    inset: 0;
    color: #fff;
    cursor: pointer;
    outline: none;
    will-change: clip-path, transform;
  }
  /* Open notch behaves like a native panel: plain arrow everywhere (no hand over buttons, no grab hand while
     paging); only text fields keep the I-beam. */
  .hit.open,
  .hit.open :global(*:not(input)),
  .hit.open :global(*:not(input):active) {
    cursor: default !important;
  }
  .hit.open :global(input) {
    cursor: text;
  }

  /* Glow + gray liquid sheen, sized to the notch body. */
  .surface {
    position: absolute;
    top: 0;
    pointer-events: none;
    /* Not clipped to this rectangle: the body's belly lags the rectangle while resizing, and a clipped sheen left a visible step
       (a thin line) along the edge. The .hit clip-path already trims everything to the exact body shape. */
    overflow: visible;
  }
  .sheen {
    position: absolute;
    inset: -90px;
    opacity: 0;
    transition: opacity 600ms ease-out;
    mix-blend-mode: screen;
  }
  .sheen.on {
    opacity: 1;
  }
  .sheen i {
    position: absolute;
    width: 360px;
    height: 150px;
    border-radius: 50%;
    background: radial-gradient(closest-side, rgba(170, 170, 182, 0.1), rgba(170, 170, 182, 0));
    filter: blur(14px);
    animation: drift 9s ease-in-out infinite alternate;
    will-change: transform;
  }
  .sheen i:nth-child(1) {
    left: 30px;
    top: 210px;
  }
  .sheen i:nth-child(2) {
    left: 350px;
    top: 240px;
    animation-duration: 12s;
    animation-delay: -4s;
  }
  .sheen i:nth-child(3) {
    left: 590px;
    top: 200px;
    animation-duration: 10s;
    animation-delay: -7s;
  }
  @keyframes drift {
    from {
      transform: translate3d(-40px, 10px, 0) scale(1);
    }
    to {
      transform: translate3d(60px, -26px, 0) scale(1.18);
    }
  }

  .slot {
    position: absolute;
    top: 0;
    transform-origin: 50% 0;
    will-change: transform, filter, opacity;
  }
  .slot.idle {
    height: 36px;
  }

  /* Live Activity on the closed notch: icon left, value right; hovering widens it and reveals the label */
  .glance {
    position: absolute;
    inset: 0 0 auto 0;
    height: 36px;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 20px;
    font-size: 12.5px;
    font-weight: 700;
    letter-spacing: 0.01em;
    font-variant-numeric: tabular-nums;
    color: var(--c);
  }
  .ico {
    flex: none;
    display: grid;
    place-items: center;
    min-width: 16px;
    min-height: 16px; /* not a fixed height: a taller child (the cover) would overflow downwards instead of being centred */
  }
  .lbl {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: rgba(255, 255, 255, 0.86);
    font-weight: 600;
    opacity: 0;
    transition: opacity 200ms ease-out;
  }
  .glance.wide .lbl {
    opacity: 1;
    transition-delay: 90ms;
  }
  .val {
    flex: none;
    margin-left: auto;
  }
  .cover {
    width: 24px;
    height: 24px;
    border-radius: 7px;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: #1c1c1e;
    color: rgba(255, 255, 255, 0.6);
    font-size: 13px;
    box-shadow: 0 0 0 0.5px rgba(255, 255, 255, 0.12);
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  /* Music activity: artwork and level meter sit on the same optical inset, both centred on the notch's vertical middle. */
  .glance.music {
    gap: 12px; /* side insets come from SIDE (inline style) */
  }
  .glance.music .cover {
    width: 22px;
    height: 22px;
    border-radius: 6px;
  }
  /* Pomodoro activity: a bigger timer icon and countdown; red while focusing, green on a break (colour comes from --c) */
  .glance.pomo {
    font-size: 16px;
    gap: 11px;
  }
  /* filling ring, same look as the seconds ring in the header */
  .glance.pomo .pring {
    width: 22px;
    height: 22px;
    transform: rotate(-90deg);
  }
  .pring circle {
    fill: none;
    stroke-width: 3;
  }
  .pt {
    stroke: color-mix(in srgb, var(--c) 24%, transparent);
  }
  .pf {
    stroke: var(--c);
    stroke-linecap: round;
    transition: stroke-dashoffset 1s linear;
  }
  .glance.pomo .val {
    font-weight: 800;
    letter-spacing: 0.02em;
  }
  .typing {
    display: flex;
    gap: 3px;
  }
  .typing i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    animation: typing 1000ms ease-in-out infinite;
  }
  .typing i:nth-child(2) {
    animation-delay: 150ms;
  }
  .typing i:nth-child(3) {
    animation-delay: 300ms;
  }
  @keyframes typing {
    0%,
    60%,
    100% {
      opacity: 0.3;
      transform: translateY(0);
    }
    30% {
      opacity: 1;
      transform: translateY(-2px);
    }
  }
  .adots {
    flex: none;
    display: flex;
    align-items: center;
    gap: 5px;
    opacity: 0;
    transition: opacity 180ms;
  }
  .glance.wide .adots,
  .adots.on {
    opacity: 1;
  }
  /* empty slot (no activity drawn): dots sit in the vertical middle of the pill */
  .adots.solo {
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    transform: translateY(-50%);
    justify-content: center;
  }
  .adots i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.28);
    transition: background 160ms, transform 160ms;
  }
  .adots i.cur {
    background: var(--c);
    transform: scale(1.25);
  }

  .panel {
    position: relative;
    height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    padding: 0 46px 8px;
    text-align: left;
  }
  .panel.voice {
    text-shadow: 0 1px 8px rgba(0, 0, 0, 0.55);
  }
  .top {
    flex: none;
    height: 46px;
    position: relative;
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding-top: 6px;
  }
  .titles {
    position: relative;
    height: 100%;
  }
  .titles span {
    position: absolute;
    left: 0;
    top: 50%;
    transform: translateY(-40%);
    font-size: 21px;
    font-weight: 700;
    letter-spacing: -0.02em;
    white-space: nowrap;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 17px;
    color: #f2f2f4;
    font-variant-numeric: tabular-nums;
  }
  .status b {
    font-weight: 700;
  }
  .ring {
    width: 24px;
    height: 24px;
    transform: rotate(-90deg);
  }
  .ring circle {
    fill: none;
    stroke-width: 3;
  }
  .rt {
    stroke: rgba(48, 209, 88, 0.22);
  }
  .rf {
    stroke: #30d158;
    stroke-linecap: round;
    transition: stroke-dashoffset 1s linear;
  }
  .ringcard {
    position: absolute;
    z-index: 5;
    left: 46px;
    right: 46px;
    top: 50px;
    bottom: 10px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    text-align: center;
    background: #000;
    border-radius: 18px;
  }
  .bell {
    position: relative;
    width: 46px;
    height: 46px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: #ffd60a;
    color: #000;
    animation: ringshake 1.6s ease-in-out infinite;
    transform-origin: 50% 20%;
  }
  .bell svg {
    width: 24px;
    height: 24px;
    fill: currentColor;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .bell .pulse {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    border: 2px solid #ffd60a;
    animation: ringpulse 1.6s ease-out infinite;
  }
  .rlabel {
    margin-top: 4px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: #ffd60a;
  }
  .rtext {
    max-width: 100%;
    font-size: 19px;
    font-weight: 650;
    line-height: 1.25;
    color: #fff;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .ractions {
    display: flex;
    gap: 10px;
    margin-top: 6px;
  }
  .rbtn {
    all: unset;
    box-sizing: border-box;
    padding: 8px 20px;
    border-radius: 999px;
    font-size: 14px;
    font-weight: 650;
    background: #ffd60a;
    color: #000;
    transition: transform 120ms, filter 120ms;
  }
  .rbtn.ghost {
    background: #2a2a2d;
    color: #fff;
  }
  .rbtn:hover {
    filter: brightness(1.12);
  }
  .rbtn:active {
    transform: scale(0.95);
  }
  .rbtn:focus-visible {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.7);
  }
  @keyframes ringshake {
    0%, 60%, 100% { transform: rotate(0); }
    10% { transform: rotate(14deg); }
    20% { transform: rotate(-12deg); }
    30% { transform: rotate(9deg); }
    40% { transform: rotate(-6deg); }
    50% { transform: rotate(3deg); }
  }
  @keyframes ringpulse {
    from { transform: scale(1); opacity: 0.7; }
    to { transform: scale(1.7); opacity: 0; }
  }
  .banner {
    position: absolute;
    left: 25%;
    right: 25%;
    top: 8px;
    height: 30px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 20px;
    font-size: 13px;
    font-weight: 700;
    color: #000;
    background: #ffd60a;
    border-radius: 15px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    animation: pop 260ms cubic-bezier(0.23, 1, 0.32, 1) both;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.94);
      filter: blur(4px);
    }
  }
  .pages {
    flex: 1;
    min-height: 0;
    overflow: hidden;
    touch-action: pan-y;
  }
  .track {
    height: 100%;
    display: flex;
    will-change: transform;
  }
  .page {
    flex: 0 0 100%;
    min-width: 0;
    height: 100%;
    box-sizing: border-box;
    padding: 2px 4px;
    overflow: hidden; /* keep neighbouring pages from leaking in while swiping */
  }
  .dots {
    flex: none;
    height: 18px;
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 2px;
  }
  .dots button {
    all: unset;
    cursor: pointer;
    width: 16px;
    height: 18px;
    display: grid;
    place-items: center;
    transition: width 280ms cubic-bezier(0.3, 1.3, 0.5, 1);
  }
  .dots button.on {
    width: 32px;
  }
  .dots button::after {
    content: "";
    width: 100%;
    height: 6px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.28);
    transition: background 150ms;
    transform: scaleX(0.4);
  }
  .dots button.on::after {
    background: #fff;
    transform: none;
  }
  .dots button:focus-visible::after {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.5);
  }
  @media (prefers-reduced-motion: reduce) {
    .bell,
    .bell .pulse {
      animation: none;
    }
    .banner {
      animation: none;
    }
    .sheen i {
      animation: none;
    }
  }
</style>
