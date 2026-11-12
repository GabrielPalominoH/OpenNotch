<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { chat } from "$lib/chat.svelte";
  import { renderMarkdown } from "$lib/markdown";

  /**
   * `needed` = the notch height (px of the chat page itself) that would show the whole conversation without
   * scrolling. The shell uses it to stretch the notch while a long answer streams in.
   */
  let { listening = false, needed = $bindable(0) }: { listening?: boolean; needed?: number } = $props();

  let inputEl: HTMLInputElement | undefined = $state();
  let keyEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();
  let stackEl: HTMLDivElement | undefined = $state();
  let chatEl: HTMLDivElement | undefined = $state();
  let draft = $state("");
  let copied = $state(false);
  // Footer (Copiar / Nuevo chat) appears once an answer is complete
  const canAct = $derived(
    !chat.busy && chat.voice === "idle" && chat.messages.length > 0 && chat.messages.at(-1)?.role === "assistant" && !!chat.messages.at(-1)?.content,
  );
  async function copyAnswer() {
    const t = chat.messages.at(-1)?.content ?? "";
    try {
      await navigator.clipboard.writeText(t);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  }
  // "Escuchando…" while the mic is open (or when the voice look is simulated from the tray)
  const showListen = $derived(chat.voice === "listening" || (listening && chat.voice === "idle"));

  export function focus() {
    (keyEl ?? inputEl)?.focus();
  }

  /** `/memoria` lists what the assistant has learned about the user; `/olvidar todo` wipes it. */
  async function showMemory(wipe: boolean) {
    if (wipe) await invoke("memory_clear");
    const facts = (await invoke("memory_list")) as string[];
    chat.messages.push({ role: "assistant", content: wipe ? "Listo, olvidé todo lo que sabía de ti." : facts.length ? "Esto es lo que sé de ti:\n\n" + facts.map((f) => `- ${f}`).join("\n") + "\n\nPuedes decirme «olvida que…» o escribir `/olvidar todo`." : "Todavía no sé nada de ti. Voy aprendiendo de lo que me preguntes." });
  }

  function submit() {
    const text = draft;
    draft = "";
    if (text.trim().toLowerCase() === "/nuevo") {
      chat.clear();
      return;
    }
    const cmd = text.trim().toLowerCase();
    if (cmd === "/memoria" || cmd === "/olvidar todo") {
      void showMemory(cmd === "/olvidar todo");
      return;
    }
    chat.send(text, "text");
  }

  // Focus whichever field is on screen (e.g. the prompt appears right after the key is saved).
  $effect(() => {
    (inputEl ?? keyEl)?.focus();
  });

  // content height + everything in the chat page that isn't the message list (input, gaps, error line)
  $effect(() => {
    if (!stackEl || !listEl || !chatEl) {
      needed = 0;
      return;
    }
    const measure = () => {
      needed = Math.ceil(stackEl!.offsetHeight + (chatEl!.offsetHeight - listEl!.clientHeight));
      listEl!.scrollTop = listEl!.scrollHeight; // the newest message stays in view when the list is resized
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(stackEl);
    ro.observe(chatEl);
    ro.observe(listEl); // the footer appearing shrinks the list without touching the other two
    return () => ro.disconnect();
  });

  // Links must never navigate the notch's webview.
  function blockLinks(e: MouseEvent) {
    if ((e.target as HTMLElement).closest("a")) e.preventDefault();
  }

  // Keep the newest text in view while a reply streams in.
  $effect(() => {
    chat.messages.at(-1)?.content;
    chat.messages.length;
    if (listEl) {
      listEl.scrollTop = listEl.scrollHeight;
      // layout settles over the next frames (panel grows, footer appears): follow it
      for (const ms of [50, 150, 350]) setTimeout(() => listEl && (listEl.scrollTop = listEl.scrollHeight), ms);
    }
  });
</script>

<div class="chat" bind:this={chatEl}>
  {#if chat.hasKey === false}
    <form
      class="keyform"
      onsubmit={(e) => {
        e.preventDefault();
        chat.saveKey(keyEl?.value ?? "");
      }}
    >
      <p>Pega tu API key de Gemini (Google AI Studio). Se guarda en el Administrador de credenciales de Windows.</p>
      <input bind:this={keyEl} class="field" type="password" placeholder="Tu API key de Gemini" autocomplete="off" spellcheck="false" />
    </form>
  {:else}
    <div class="inputrow">
      <input
        bind:this={inputEl}
        class="field pill"
        bind:value={draft}
        placeholder={chat.voice === "listening" ? "Te escucho…" : "Busca o pregunta"}
        autocomplete="off"
        spellcheck="false"
        onkeydown={(e) => e.key === "Enter" && !e.isComposing && submit()}
      />
      <button
        type="button"
        class="mic"
        class:on={chat.voice !== "idle"}
        class:thinking={chat.voice === "thinking"}
        style="--lv:{chat.level}"
        aria-label={chat.voice === "idle" ? "Hablar con el asistente" : chat.voice === "listening" ? "Terminar de hablar" : "Detener"}
        title={chat.voice === "idle" ? "Hablar" : chat.voice === "listening" ? "Terminar" : "Detener"}
        onclick={() => chat.toggleVoice()}
      >
        <span class="ring"></span>
        {#if chat.voice === "idle"}
          <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="3" width="6" height="12" rx="3" fill="currentColor" stroke="none" /><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3" /></svg>
        {:else if chat.voice === "thinking"}
          <span class="spin"></span>
        {:else}
          <svg viewBox="0 0 24 24" width="13" height="13" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="3" fill="currentColor" /></svg>
        {/if}
      </button>
    </div>
    <div class="messages" bind:this={listEl}>
      <div class="stack" class:center={showListen || chat.messages.length === 0} bind:this={stackEl}>
        {#if showListen}
          <div class="listen" aria-live="polite"><span class="bars"><i></i><i></i><i></i><i></i><i></i></span>{chat.voice === "listening" && !chat.micReady ? "Preparando…" : "Escuchando…"}</div>
        {:else if chat.messages.length === 0}
          <p class="empty">¿En qué te ayudo?</p>
        {/if}
        {#each chat.messages as m}
          {#if m.role === "assistant"}
            {#if !m.content.trim() && (chat.busy || chat.voice === "thinking") && m === chat.messages.at(-1)}
              <div class="msg assistant think" aria-live="polite"><span class="shimmer">Pensando…</span></div>
            {:else}
              <div class="msg assistant md" role="presentation" onclick={blockLinks}>{@html renderMarkdown(m.content, (chat.busy || chat.voice === "thinking" || chat.voice === "speaking") && m === chat.messages.at(-1))}</div>
            {/if}
          {:else}
            <div class="msg user">{m.content}</div>
          {/if}
        {/each}
      </div>
    </div>
    {#if canAct}
      <div class="foot">
        <button class="fbtn" onclick={copyAnswer}>{copied ? "Copiado" : "Copiar"}</button>
        <button class="fbtn" onclick={() => chat.clear()}>Nuevo chat</button>
      </div>
    {/if}
  {/if}

  {#if chat.error}<p class="err">{chat.error}</p>{/if}

</div>

<style>
  .chat {
    height: 100%;
    display: flex;
    flex-direction: column;
    gap: 12px;
    user-select: text;
  }
  .messages {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-right: 4px;
    /* still scrollable (wheel / trackpad), just without the visible bar */
    scrollbar-width: none;
  }
  /* when the conversation is longer than the box, text fades out softly at the edges instead of being cut flat */
  .messages {
    -webkit-mask-image: linear-gradient(to bottom, transparent 0, #000 14px, #000 calc(100% - 16px), transparent 100%);
    mask-image: linear-gradient(to bottom, transparent 0, #000 14px, #000 calc(100% - 16px), transparent 100%);
  }
  .messages::-webkit-scrollbar {
    display: none;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 16px; /* question <-> answer */
    /* breathing room above the first and below the last message, so the answer never touches the edges of the list */
    padding-block: 16px 20px;
  }
  .msg.assistant + .msg.user {
    margin-top: 10px; /* a bit more air between one exchange and the next */
  }
  .stack.center {
    margin-block: auto;
    padding-block: 0;
  }
  .empty,
  .keyform p {
    margin: auto;
    color: #9a9aa3;
    font-size: 14px;
    text-align: center;
  }
  .msg {
    max-width: 92%;
    font-size: 14.5px;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .msg.user {
    align-self: flex-end;
    background: rgba(255, 255, 255, 0.16);
    color: #fff;
    font-weight: 500;
    padding: 7px 13px;
    border-radius: 16px 16px 5px 16px;
  }
  .msg.assistant {
    align-self: flex-start;
    color: #fff;
  }
  .err {
    margin: 0;
    color: #ff6b63;
    font-size: 13px;
  }
  /* No pill: just a text line with a caret (and the mic at the end), like a spotlight bar */
  :global(.field.pill),
  :global(.field.pill:focus) {
    border-radius: 0;
    padding: 8px 2px;
    background: transparent;
    box-shadow: none;
    outline: none;
    caret-color: #fff;
    font-size: 16px;
  }
  .inputrow {
    position: relative;
  }
  .foot {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }
  .fbtn {
    all: unset;
    box-sizing: border-box;
    padding: 5px 14px;
    border-radius: 999px;
    font-size: 12.5px;
    font-weight: 600;
    color: #fff;
    background: rgba(255, 255, 255, 0.14);
    transition: background 120ms, transform 120ms;
  }
  .fbtn:hover {
    background: rgba(255, 255, 255, 0.22);
  }
  .fbtn:active {
    transform: scale(0.95);
  }
  .fbtn:focus-visible {
    box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.6);
  }
  .inputrow :global(.field.pill) {
    width: 100%;
    box-sizing: border-box;
    padding-right: 40px;
  }
  .mic {
    position: absolute;
    right: 0;
    top: 50%;
    width: 30px;
    height: 30px;
    margin-top: -15px;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #c9c9d1;
    background: transparent;
    transition: background-color 160ms ease, color 160ms ease, transform 120ms ease;
  }
  .mic:hover {
    background: rgba(255, 255, 255, 0.1);
    color: #fff;
  }
  .mic:active {
    transform: scale(0.92);
  }
  .mic.on {
    background: rgba(255, 255, 255, 0.24);
    color: #fff;
  }
  .mic .ring {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: var(--accent);
    opacity: 0;
    transform: scale(1);
    pointer-events: none;
    transition: transform 90ms linear, opacity 90ms linear;
  }
  .mic.on:not(.thinking) .ring {
    opacity: calc(0.18 + var(--lv) * 0.4);
    transform: scale(calc(1 + var(--lv) * 0.55));
  }
  .mic > :global(svg),
  .spin {
    position: relative;
  }
  .spin {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.35);
    border-top-color: #fff;
    animation: spin 700ms linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  .listen {
    margin: auto;
    display: flex;
    align-items: center;
    gap: 14px;
    font-size: 17px;
    font-weight: 600;
    color: #fff;
  }
  .bars {
    display: flex;
    align-items: center;
    gap: 3px;
    height: 22px;
  }
  .bars i {
    width: 3.5px;
    height: 100%;
    border-radius: 2px;
    background: #fff;
    animation: voice 900ms ease-in-out infinite alternate;
  }
  .bars i:nth-child(2) { animation-delay: -150ms; animation-duration: 700ms; }
  .bars i:nth-child(3) { animation-delay: -400ms; animation-duration: 1100ms; }
  .bars i:nth-child(4) { animation-delay: -250ms; animation-duration: 800ms; }
  .bars i:nth-child(5) { animation-delay: -550ms; animation-duration: 1000ms; }
  @keyframes voice {
    from { transform: scaleY(0.2); }
    to { transform: scaleY(1); }
  }
  /* Markdown (rendered from the model's answer) */
  .md {
    white-space: normal;
  }
  .md :global(> :first-child) {
    margin-top: 0;
  }
  .md :global(> :last-child) {
    margin-bottom: 0;
  }
  .md :global(p) {
    margin: 0 0 0.55em;
  }
  .md :global(strong) {
    font-weight: 700;
    color: #fff;
  }
  .md :global(em) {
    font-style: italic;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    margin: 0.7em 0 0.35em;
    font-size: 1.05em;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 0.55em;
    padding-left: 1.3em;
  }
  .md :global(li) {
    margin: 0.15em 0;
  }
  .md :global(li::marker) {
    color: var(--accent);
  }
  .md :global(code) {
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.88em;
    padding: 0.1em 0.4em;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.12);
  }
  .md :global(pre) {
    margin: 0.4em 0 0.6em;
    padding: 10px 12px;
    border-radius: 12px;
    background: #121214;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  .md :global(pre code) {
    padding: 0;
    background: none;
    font-size: 0.85em;
  }
  .md :global(blockquote) {
    margin: 0.4em 0 0.6em;
    padding-left: 12px;
    border-left: 3px solid var(--accent);
    color: #c7c7cc;
  }
  .md :global(a) {
    color: var(--accent);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: default;
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid rgba(255, 255, 255, 0.16);
    margin: 0.7em 0;
  }
  .md :global(table) {
    border-collapse: collapse;
    margin: 0.4em 0 0.6em;
    font-size: 0.92em;
  }
  .md :global(th),
  .md :global(td) {
    padding: 4px 10px;
    border: 1px solid rgba(255, 255, 255, 0.16);
  }
  /* Streaming marker: a soft white dot that breathes, like iMessage / Siri, instead of a spinner */
  .md :global(.caret) {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-left: 5px;
    vertical-align: middle;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 0 10px rgba(255, 255, 255, 0.55);
    animation: breathe 1100ms ease-in-out infinite;
  }
  @keyframes breathe {
    0%, 100% { transform: scale(0.6); opacity: 0.45; }
    50% { transform: scale(1); opacity: 1; }
  }
  /* "Pensando…": text with a light sweep passing through it (Apple Intelligence style) */
  .think {
    font-size: 15px;
    font-weight: 500;
  }
  .shimmer {
    background: linear-gradient(100deg, rgba(255, 255, 255, 0.38) 35%, #fff 50%, rgba(255, 255, 255, 0.38) 65%) 200% 0 / 250% 100% no-repeat;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: sweep 1500ms linear infinite;
  }
  @keyframes sweep {
    to { background-position: -50% 0; }
  }
  /* a new message fades in once (no blur: streamed text is re-rendered on every chunk, a blur here would never settle) */
  .msg {
    animation: settle 260ms cubic-bezier(0.22, 1, 0.36, 1) both;
  }
  .msg.assistant.md {
    animation: none;
  }
  @keyframes settle {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: none; }
  }
  .keyform {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 12px;
  }
  @media (prefers-reduced-motion: reduce) {
    .md :global(.caret),
    .shimmer,
    .msg {
      animation: none;
    }
    .shimmer {
      color: rgba(255, 255, 255, 0.7);
    }
    .bars i {
      animation: none;
    }
  }
</style>
