<script lang="ts">
  /**
   * 6-bar level meter: bass on the left -> treble on the right. Each bar is a rounded pill that grows from the centre line,
   * painted with a heavily blurred copy of the cover (`bg`, a CSS url(...) value). `alive=false` -> canned animation.
   */
  let {
    levels,
    alive,
    bg = "",
    size = "sm",
    fx = { brightness: 135, saturation: 170, veil: 18 },
  }: {
    levels: number[];
    alive: boolean;
    bg?: string;
    size?: "sm" | "lg";
    fx?: { brightness: number; saturation: number; veil: number };
  } = $props();
  const dim = $derived(size === "lg" ? { bw: 4, gap: 3, h: 30 } : { bw: 3, gap: 2, h: 20 });
  const vars = $derived(
    `--bw:${dim.bw}px;--gap:${dim.gap}px;--bh:${dim.h}px;--pitch:${dim.bw + dim.gap}px;--bgw:${6 * dim.bw + 5 * dim.gap}px` +
      `;--fx:saturate(${fx.saturation / 100}) brightness(${fx.brightness / 100});--veil:${fx.veil / 100}` +
      (bg ? `;--bg:${bg}` : ""),
  );
</script>

<span class="meter" class:fallback={!alive} style={vars} aria-hidden="true">
  {#each levels as lv, i}<i style="--i:{i};--s:{(0.14 + 0.86 * lv).toFixed(3)}"></i>{/each}
</span>

<style>
  .meter {
    flex: none;
    position: relative;
    isolation: isolate;
    filter: var(--fx, saturate(1.7) brightness(1.35));
    display: flex;
    align-items: center;
    gap: var(--gap);
    height: var(--bh);
  }
  i {
    width: var(--bw);
    /* height (not scaleY) so the rounded tips stay perfectly round at every level */
    height: calc(var(--s, 0.14) * 100%);
    border-radius: 999px;
    /* painted with the blurred cover (each bar shows the slice of it under its own position); white when there is no cover */
    background-color: #fff;
    background-image: linear-gradient(rgba(255, 255, 255, var(--veil, 0.18)), rgba(255, 255, 255, var(--veil, 0.18))), var(--bg, linear-gradient(#fff, #fff));
    background-size: 100% 100%, var(--bgw) var(--bh);
    background-position: 0 0, calc(var(--i) * var(--pitch) * -1) 50%;
    background-repeat: no-repeat;
    transition: height 50ms linear;
  }
  .fallback i {
    transition: none;
    animation: lvl 880ms ease-in-out infinite alternate;
  }
  .fallback i:nth-child(1) { animation-duration: 760ms; animation-delay: -200ms; }
  .fallback i:nth-child(2) { animation-duration: 640ms; animation-delay: -420ms; }
  .fallback i:nth-child(3) { animation-duration: 900ms; animation-delay: -90ms; }
  .fallback i:nth-child(4) { animation-duration: 700ms; animation-delay: -330ms; }
  .fallback i:nth-child(5) { animation-duration: 840ms; animation-delay: -560ms; }
  .fallback i:nth-child(6) { animation-duration: 600ms; animation-delay: -150ms; }
  @keyframes lvl {
    from {
      height: 30%;
    }
    to {
      height: 100%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    i {
      animation: none;
      height: 70%;
    }
  }
</style>
