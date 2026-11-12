<script lang="ts">
  import { pomo, clock } from "$lib/stores.svelte";

  const LABEL = { focus: "Tiempo de foco", short_break: "Descanso corto", long_break: "Descanso largo" } as const;
  const R = 55;
  const C = 2 * Math.PI * R;

  const left = $derived(pomo.remaining(clock.now));
  const frac = $derived(pomo.s.total_ms > 0 ? left / pomo.s.total_ms : 0);
  const mm = $derived(String(Math.floor(Math.ceil(left / 1000) / 60)).padStart(2, "0"));
  const ss = $derived(String(Math.ceil(left / 1000) % 60).padStart(2, "0"));
  const fresh = $derived(!pomo.s.running && left === pomo.s.total_ms);
  const isFocus = $derived(pomo.s.phase === "focus");
  // Focus is the vivid red of the reference; breaks switch to green.
  const color = $derived(isFocus ? "#ff3b4f" : "#30d158");
</script>

<div class="pomo" style="--c:{color}">
  <div class="ring">
    <svg viewBox="0 0 140 140" aria-hidden="true">
      <circle cx="70" cy="70" r={R} class="bg" />
      <circle cx="70" cy="70" r={R} class="fg" stroke-dasharray={C} stroke-dashoffset={C * (1 - frac)} />
    </svg>
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9.5 2.5h5M12 2.5V5" />
      <circle cx="12" cy="14" r="7.5" />
      <path d="M12 9.8V14M18.3 7.7l1.3-1.3" />
    </svg>
  </div>

  <div class="right">
    <div class="timerow">
      <div class="time">{mm}:{ss}</div>
      <button class="round" aria-label="Reiniciar" onclick={() => pomo.control("reset")}>
        <svg viewBox="0 0 24 24"><path d="M20 12a8 8 0 1 1-2.6-5.9M20 4v4.5h-4.5" /></svg>
      </button>
    </div>
    <div class="label">{LABEL[pomo.s.phase]}</div>

    <div class="actions">
      <button class="pill" class:sel={pomo.s.phase === "short_break"} onclick={() => pomo.control("short_break")}>
        <svg viewBox="0 0 24 24"><path d="M5 9h11v5a5 5 0 0 1-5 5H10a5 5 0 0 1-5-5zM16 10h1.5a2.5 2.5 0 0 1 0 5H16M8 3.5v2M11.5 3.5v2" /></svg>
        Descanso
      </button>
      <button class="pill main" aria-label={pomo.s.running ? "Pausar" : fresh ? "Iniciar" : "Continuar"} onclick={() => pomo.control(pomo.s.running ? "pause" : "start")}>
        {#if pomo.s.running}
          <svg class="solid" viewBox="0 0 24 24"><path d="M6.5 4.5h4v15h-4zM13.5 4.5h4v15h-4z" /></svg>
        {:else}
          <svg class="solid" viewBox="0 0 24 24"><path d="M7 4.5v15l12.5-7.5z" /></svg>
        {/if}
      </button>
      <button class="pill" class:sel={isFocus} onclick={() => pomo.control("focus")}>
        <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8.5" /><circle cx="12" cy="12" r="4.5" /><circle cx="12" cy="12" r="1" /></svg>
        Foco
      </button>
    </div>
  </div>
</div>

<style>
  .pomo {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 30px;
  }
  .ring {
    position: relative;
    width: 104px;
    height: 104px;
    flex: none;
  }
  .ring > svg:first-child {
    width: 100%;
    height: 100%;
    transform: rotate(-90deg);
  }
  circle {
    fill: none;
  }
  .bg {
    stroke: color-mix(in srgb, var(--c) 28%, #000);
    stroke-width: 14;
    fill: color-mix(in srgb, var(--c) 14%, #000);
  }
  .fg {
    stroke: var(--c);
    stroke-width: 14;
    stroke-linecap: round;
    transition: stroke 300ms;
  }
  .icon {
    position: absolute;
    inset: 28px;
    width: 48px;
    height: 48px;
    fill: none;
    stroke: #fff;
    stroke-width: 1.9;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .right {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }
  .timerow {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .time {
    font-size: 50px;
    font-weight: 700;
    letter-spacing: -0.035em;
    line-height: 1;
    font-variant-numeric: tabular-nums;
  }
  .label {
    font-size: 16.5px;
    color: #8e8e93;
    margin-bottom: 10px;
  }
  .actions {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  button {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    transition: transform 100ms ease-out, background 120ms, filter 120ms;
  }
  button:active {
    transform: scale(0.95);
  }
  button:focus-visible {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.55);
  }
  .round {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: #2a2a2d;
    display: grid;
    place-items: center;
  }
  .round:hover,
  .pill:hover {
    background: #3a3a3e;
  }
  .round svg {
    width: 20px;
    height: 20px;
    fill: none;
    stroke: #fff;
    stroke-width: 2.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .pill {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    padding: 0 14px;
    border-radius: 20px;
    background: #2a2a2d;
    font-size: 15.5px;
    font-weight: 600;
    white-space: nowrap;
  }
  .pill.sel {
    box-shadow: inset 0 0 0 2px color-mix(in srgb, var(--c) 70%, transparent);
  }
  .pill svg {
    width: 19px;
    height: 19px;
    fill: none;
    stroke: #fff;
    stroke-width: 2.2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .pill.main {
    width: 92px;
    justify-content: center;
    background: var(--c);
  }
  .pill.main:hover {
    background: var(--c);
    filter: brightness(1.1);
  }
  .pill.main svg.solid {
    width: 24px;
    height: 24px;
    fill: #fff;
    stroke: none;
  }
</style>
