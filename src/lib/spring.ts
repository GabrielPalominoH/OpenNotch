/** Spring on a set of numeric channels, driven by rAF. Re-targeting keeps the current velocity (interruptible). */
export type Channels = Record<string, number>;

export interface SpringOpts {
  stiffness: number; // higher = snappier
  damping: number; // higher = less overshoot
}

export function createSpring<T extends Channels>(
  initial: T,
  onFrame: (v: T) => void,
  opts: SpringOpts = { stiffness: 260, damping: 24 },
) {
  const value = { ...initial } as Channels;
  const velocity: Channels = Object.fromEntries(Object.keys(initial).map((k) => [k, 0]));
  let target = { ...initial } as Channels;
  let raf = 0;
  let last = 0;

  function step(now: number) {
    const dt = Math.min((now - last) / 1000, 1 / 30);
    last = now;
    let moving = false;
    for (const k in value) {
      const force = -opts.stiffness * (value[k] - target[k]) - opts.damping * velocity[k];
      velocity[k] += force * dt;
      value[k] += velocity[k] * dt;
      if (Math.abs(velocity[k]) > 0.02 || Math.abs(value[k] - target[k]) > 0.02) moving = true;
      else {
        value[k] = target[k];
        velocity[k] = 0;
      }
    }
    onFrame({ ...value } as T);
    raf = moving ? requestAnimationFrame(step) : 0;
  }

  function run() {
    if (!raf) {
      last = performance.now();
      raf = requestAnimationFrame(step);
    }
  }

  return {
    /** Animate toward `next`; optional release velocity (units/s) per channel for a seamless hand-off. */
    to(next: Partial<T>, vel?: Partial<T>) {
      target = { ...target, ...next };
      if (vel) for (const k in vel) velocity[k] = vel[k] as number;
      run();
    },
    /** Jump (no animation) and stop — for 1:1 tracking while the user drags. */
    set(next: Partial<T>) {
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      for (const k in next) {
        value[k] = next[k] as number;
        target[k] = next[k] as number;
        velocity[k] = 0;
      }
      onFrame({ ...value } as T);
    },
    destroy() {
      if (raf) cancelAnimationFrame(raf);
    },
  };
}
