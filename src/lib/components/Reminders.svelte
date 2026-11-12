<script lang="ts">
  import { reminders, clock } from "$lib/stores.svelte";

  const CHIPS = [
    { label: "10 min", at: () => Date.now() + 10 * 60_000 },
    { label: "30 min", at: () => Date.now() + 30 * 60_000 },
    { label: "1 h", at: () => Date.now() + 60 * 60_000 },
    {
      label: "Mañana 9:00",
      at: () => {
        const d = new Date();
        d.setDate(d.getDate() + 1);
        d.setHours(9, 0, 0, 0);
        return d.getTime();
      },
    },
  ];

  let text = $state("");
  let chip = $state(1);

  const done = $derived(reminders.list.filter((r) => r.done).length);
  const total = $derived(reminders.list.length);

  async function add() {
    const t = text.trim();
    if (!t) return;
    text = "";
    await reminders.add(t, CHIPS[chip].at());
  }

  function when(ms: number, now: number) {
    const d = new Date(ms);
    const hm = d.toLocaleTimeString("es", { hour: "2-digit", minute: "2-digit", hour12: false });
    return d.toDateString() === new Date(now).toDateString()
      ? hm
      : `${d.toLocaleDateString("es", { weekday: "short", day: "numeric" }).replace(".", "")} ${hm}`;
  }
</script>

<div class="rem">
  <div class="stat">
    <div class="num">{done}/{total}</div>
    <div class="cap">Hechos hoy</div>
  </div>

  <div class="col">
    <div class="list">
      {#each reminders.list as r (r.id)}
        <div class="item" class:done={r.done} class:late={r.fired && !r.done}>
          <button class="check" aria-label={r.done ? "Marcar pendiente" : "Marcar hecho"} aria-pressed={r.done} onclick={() => reminders.setDone(r.id, !r.done)}>
            {#if r.done}<svg viewBox="0 0 24 24"><path d="M6.5 12.5l4 4 7.5-8.5" /></svg>{/if}
          </button>
          <span class="txt">{r.text}</span>
          <span class="when">{when(r.due_at_ms, clock.now)}</span>
          <button class="del" aria-label="Borrar" onclick={() => reminders.remove(r.id)}>
            <svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17" /></svg>
          </button>
        </div>
      {/each}

      <div class="item add">
        <span class="plus"><svg viewBox="0 0 24 24"><path d="M12 6v12M6 12h12" /></svg></span>
        <input
          bind:value={text}
          placeholder="Recordarme…"
          autocomplete="off"
          spellcheck="false"
          onkeydown={(e) => e.key === "Enter" && !e.isComposing && add()}
        />
        <button class="chip" aria-label="Cuándo recordar" onclick={() => (chip = (chip + 1) % CHIPS.length)}>{CHIPS[chip].label}</button>
      </div>
    </div>
  </div>
</div>

<style>
  .rem {
    height: 100%;
    display: flex;
    align-items: center;
    gap: 24px;
  }
  .stat {
    flex: none;
    width: 130px;
    padding-left: 4px;
  }
  .num {
    font-size: 50px;
    font-weight: 700;
    letter-spacing: -0.04em;
    line-height: 1;
    font-variant-numeric: tabular-nums;
  }
  .cap {
    margin-top: 6px;
    font-size: 16px;
    color: #8e8e93;
  }
  .col {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: center;
  }
  .list {
    width: 100%;
    max-height: 100%;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 2px 4px 2px 0;
    scrollbar-width: none;
  }
  .item {
    flex: none;
    display: flex;
    align-items: center;
    gap: 14px;
    height: 36px;
    padding: 0 10px 0 10px;
    border-radius: 13px;
    background: #121214;
    box-shadow: 0 0 22px rgba(255, 255, 255, 0.035);
    font-size: 16px;
    transition: background 200ms, box-shadow 200ms;
  }
  .item.done {
    background: #0e2216;
    box-shadow: 0 0 26px rgba(48, 209, 88, 0.12);
  }
  .item.done .txt {
    text-decoration: line-through;
    color: #a6b3aa;
  }
  .item.late .when {
    color: #ff453a;
  }
  .check {
    all: unset;
    flex: none;
    box-sizing: border-box;
    width: 23px;
    height: 23px;
    border-radius: 50%;
    border: 2px solid #8e8e93;
    display: grid;
    place-items: center;
    cursor: pointer;
    transition: background 160ms, border-color 160ms, transform 100ms;
  }
  .check:hover {
    border-color: #fff;
  }
  .check:active {
    transform: scale(0.9);
  }
  .item.done .check {
    background: #30d158;
    border-color: #30d158;
  }
  .check svg {
    width: 18px;
    height: 18px;
    fill: none;
    stroke: #000;
    stroke-width: 3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .check:focus-visible,
  .del:focus-visible,
  .chip:focus-visible {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.6);
  }
  .txt {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .when {
    flex: none;
    font-size: 14px;
    color: #8e8e93;
    font-variant-numeric: tabular-nums;
    text-transform: capitalize;
  }
  .del {
    all: unset;
    flex: none;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    cursor: pointer;
    opacity: 0;
    transition: opacity 120ms, background 120ms;
  }
  .item:hover .del,
  .del:focus-visible {
    opacity: 1;
  }
  .del:hover {
    background: rgba(255, 255, 255, 0.14);
  }
  .del svg {
    width: 13px;
    height: 13px;
    fill: none;
    stroke: #d6d6dc;
    stroke-width: 2.4;
    stroke-linecap: round;
  }
  .item.add {
    background: transparent;
    box-shadow: none;
    border: 1.5px dashed #3a3a3e;
  }
  .plus {
    flex: none;
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
  }
  .plus svg {
    width: 18px;
    height: 18px;
    fill: none;
    stroke: #8e8e93;
    stroke-width: 2.4;
    stroke-linecap: round;
  }
  input {
    all: unset;
    flex: 1;
    min-width: 0;
    font-size: 16px;
    color: #fff;
  }
  input::placeholder {
    color: #6e6e73;
  }
  .chip {
    all: unset;
    flex: none;
    cursor: pointer;
    padding: 5px 12px;
    border-radius: 999px;
    background: #2a2a2d;
    font-size: 13px;
    font-weight: 600;
    color: #e5e5ea;
    white-space: nowrap;
    transition: background 120ms, transform 100ms;
  }
  .chip:hover {
    background: #3a3a3e;
  }
  .chip:active {
    transform: scale(0.95);
  }
</style>
