<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { media, clock } from "$lib/stores.svelte";
  import Meter from "./Meter.svelte";

  let {
    levels = [0, 0, 0, 0, 0, 0],
    alive = false,
    bg = "",
    fx,
  }: { levels?: number[]; alive?: boolean; bg?: string; fx?: { brightness: number; saturation: number; veil: number } } = $props();

  // System volume (the media session has no per-app volume). Read on mount and every few seconds, so keys/other apps stay in sync.
  let vol = $state(0.5);
  let adjusting = false;
  const readVol = () => !adjusting && invoke<number>("volume_get").then((v) => (vol = v)).catch(() => {});
  onMount(() => {
    readVol();
    const id = setInterval(readVol, 3000);
    return () => clearInterval(id);
  });
  function setVol(v: number) {
    vol = Math.min(1, Math.max(0, v));
    invoke("volume_set", { level: vol }).catch(() => {});
  }
  function volDown(e: PointerEvent) {
    e.stopPropagation(); // never let the page carousel treat a volume drag as a swipe
    const el = e.currentTarget as HTMLElement;
    try {
      el.setPointerCapture(e.pointerId);
    } catch {}
    adjusting = true;
    const at = (ev: PointerEvent) => {
      const r = el.getBoundingClientRect();
      setVol((ev.clientX - r.left) / r.width);
    };
    at(e);
    const move = (ev: PointerEvent) => at(ev);
    const up = () => {
      adjusting = false;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }
  function volKey(e: KeyboardEvent) {
    if (e.key === "ArrowRight" || e.key === "ArrowUp") setVol(vol + 0.05);
    else if (e.key === "ArrowLeft" || e.key === "ArrowDown") setVol(vol - 0.05);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  const fmt = (ms: number) => {
    const s = Math.max(0, Math.floor(ms / 1000));
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };
  const pos = $derived(media.position(clock.perf));
  const dur = $derived(media.s.duration_ms);
  // while dragging the bar, show (and later commit) the scrubbed position instead of the live one
  let scrub = $state<number | null>(null);
  const shown = $derived(scrub ?? pos);
  const shownPct = $derived(dur > 0 ? Math.min(100, Math.max(0, (shown / dur) * 100)) : 0);

  function seekDown(e: PointerEvent) {
    if (dur <= 0) return;
    e.stopPropagation(); // the page carousel must not take this for a swipe
    const el = e.currentTarget as HTMLElement;
    try {
      el.setPointerCapture(e.pointerId);
    } catch {}
    const at = (ev: PointerEvent) => {
      const r = el.getBoundingClientRect();
      scrub = Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width)) * dur;
    };
    at(e);
    const move = (ev: PointerEvent) => at(ev);
    const up = () => {
      if (scrub !== null) media.seek(scrub);
      scrub = null;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  function seekKey(e: KeyboardEvent) {
    if (dur <= 0) return;
    const step = e.shiftKey ? 15_000 : 5_000;
    if (e.key === "ArrowRight") media.seek(Math.min(dur, pos + step));
    else if (e.key === "ArrowLeft") media.seek(Math.max(0, pos - step));
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<div class="music">
  {#if !media.s.has_session}
    <p class="none">Nada sonando</p>
  {:else}
    <div class="top">
      <div class="art">
        {#if media.thumb}<img src={media.thumb} alt="" draggable="false" />{:else}<span>♪</span>{/if}
      </div>
      <div class="meta">
        <div class="title">{media.s.title || "Sin título"}</div>
        <div class="artist">{media.s.artist}</div>
      </div>
      {#if media.s.playing}<div class="lvl"><Meter {levels} {alive} {bg} {fx} size="lg" /></div>{/if}
    </div>

    <div class="bar">
      <span>{fmt(shown)}</span>
      <div
        class="seek"
        class:active={scrub !== null}
        class:off={dur <= 0}
        role="slider"
        tabindex="0"
        aria-label="Posición"
        aria-valuemin="0"
        aria-valuemax={Math.round(dur / 1000)}
        aria-valuenow={Math.round(shown / 1000)}
        onpointerdown={seekDown}
        onkeydown={seekKey}
      >
        <div class="track"><div class="fill" style="width:{shownPct}%"></div></div>
        <i class="knob" style="left:{shownPct}%"></i>
      </div>
      <span>{dur > 0 ? `-${fmt(dur - shown)}` : "--:--"}</span>
    </div>

    <div class="controls">
      <button aria-label="Anterior" onclick={() => media.control("previous")}>
        <svg viewBox="0 0 24 24"><path d="M11 6v12L2.5 12zM21 6v12l-8.5-6z" /></svg>
      </button>
      <button class="main" aria-label={media.s.playing ? "Pausar" : "Reproducir"} onclick={() => media.control("play_pause")}>
        {#if media.s.playing}
          <svg viewBox="0 0 24 24"><path d="M6.5 4.5h4v15h-4zM13.5 4.5h4v15h-4z" /></svg>
        {:else}
          <svg viewBox="0 0 24 24"><path d="M7 4.5v15l12.5-7.5z" /></svg>
        {/if}
      </button>
      <button aria-label="Siguiente" onclick={() => media.control("next")}>
        <svg viewBox="0 0 24 24"><path d="M13 6v12l8.5-6zM3 6v12l8.5-6z" /></svg>
      </button>
    </div>

    <div class="vol">
      <button aria-label={vol === 0 ? "Activar sonido" : "Silenciar"} onclick={() => setVol(vol === 0 ? 0.5 : 0)}>
        <svg viewBox="0 0 24 24" class="sp"><path d="M3 9.5v5h3.5l5 4v-13l-5 4z" />{#if vol > 0}<path d="M14.5 9a4 4 0 0 1 0 6" class="wv" />{/if}{#if vol > 0.5}<path d="M17 6.5a8 8 0 0 1 0 11" class="wv" />{/if}</svg>
      </button>
      <div class="vtrack" role="slider" tabindex="0" aria-label="Volumen" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(vol * 100)} onpointerdown={volDown} onkeydown={volKey}>
        <div class="vfill" style="width:{vol * 100}%"></div>
      </div>
    </div>
  {/if}
</div>

<style>
  .music {
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
  }
  .none {
    margin: auto;
    color: #8e8e93;
    font-size: 15px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .art {
    flex: none;
    width: 68px;
    height: 68px;
    border-radius: 13px;
    background: #1c1c1e;
    display: grid;
    place-items: center;
    overflow: hidden;
    font-size: 36px;
    color: #8e8e93;
  }
  .art img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .meta {
    min-width: 0;
    flex: 1;
  }
  .lvl {
    flex: none;
    margin-left: 4px;
    padding-right: 2px;
  }
  .title {
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.025em;
    line-height: 1.1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .artist {
    margin-top: 3px;
    font-size: 14.5px;
    color: #8e8e93;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 14px;
    font-size: 14.5px;
    font-weight: 500;
    color: #f2f2f4;
    font-variant-numeric: tabular-nums;
  }
  .bar span {
    min-width: 42px;
  }
  .bar span:last-child {
    text-align: right;
  }
  .seek {
    flex: 1;
    position: relative;
    height: 22px;
    display: flex;
    align-items: center;
    touch-action: none;
    cursor: pointer;
  }
  .seek.off {
    cursor: default;
  }
  .seek:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.4);
    border-radius: 6px;
  }
  .track {
    flex: 1;
    height: 5px;
    transition: height 120ms ease-out;
    border-radius: 3px;
    background: #3a3a3e;
    overflow: hidden;
  }
  .seek:hover .track,
  .seek.active .track {
    height: 8px;
  }
  .knob {
    position: absolute;
    top: 50%;
    width: 14px;
    height: 14px;
    margin: -7px 0 0 -7px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
    opacity: 0;
    transform: scale(0.6);
    transition: opacity 120ms, transform 120ms;
    pointer-events: none;
  }
  .seek.off .knob {
    display: none;
  }
  .seek:hover .knob,
  .seek.active .knob {
    opacity: 1;
    transform: scale(1);
  }
  .fill {
    height: 100%;
    background: #fff;
    border-radius: 3px;
  }
  .controls {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 38px;
  }
  button {
    all: unset;
    cursor: pointer;
    width: 40px;
    height: 32px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    transition: transform 100ms ease-out, opacity 120ms;
  }
  button:hover {
    opacity: 0.75;
  }
  button:active {
    transform: scale(0.9);
  }
  button:focus-visible {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.5);
  }
  .vol {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
  }
  .vol button {
    width: 30px;
    height: 24px;
  }
  .vol .sp {
    width: 19px;
    height: 19px;
  }
  .wv {
    fill: none;
    stroke: #fff;
    stroke-width: 2;
    stroke-linecap: round;
  }
  .vtrack {
    width: 190px;
    height: 20px;
    display: flex;
    align-items: center;
    touch-action: none;
    position: relative;
  }
  .vtrack::before {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    height: 5px;
    border-radius: 3px;
    background: #3a3a3e;
  }
  .vfill {
    position: relative;
    height: 5px;
    border-radius: 3px;
    background: #fff;
  }
  .vtrack:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.4);
    border-radius: 6px;
  }
  svg {
    width: 23px;
    height: 23px;
    fill: #fff;
  }
  .main svg {
    width: 27px;
    height: 27px;
  }
</style>
