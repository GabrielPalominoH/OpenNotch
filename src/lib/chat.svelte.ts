import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Speaker } from "$lib/speech";

export type Msg = { role: "user" | "assistant"; content: string };
export type Source = "text" | "voice";
/** idle -> listening (mic open) -> thinking (transcribing / waiting for the answer) -> speaking -> idle */
export type VoiceState = "idle" | "listening" | "thinking" | "speaking";


class Chat {
  messages = $state<Msg[]>([]);
  busy = $state(false);
  error = $state("");
  hasKey = $state<boolean | null>(null);
  voice = $state<VoiceState>("idle");
  /** microphone level 0..1 while listening */
  level = $state(0);
  /** false while the microphone is still opening (speech before this point would be lost) */
  micReady = $state(false);
  #reqId = 0;
  #voiceReq = -1;
  /** whole answer of the current voice turn; shown once the voice starts */
  #voiceFull = "";
  #voiceShown = false;
  #speaker = new Speaker();

  async init() {
    this.hasKey = await invoke<boolean>("has_api_key");
    await listen("key-cleared", () => (this.hasKey = false));
    // Events carry the request id so a stale stream can never write into a newer reply.
    this.#speaker.onStart = () => {
      if (this.voice === "thinking") this.voice = "speaking";
    };
    // Voice answers: the text block stays hidden until the voice starts, then it appears whole (and keeps growing if
    // the model is still writing).
    this.#speaker.onSentence = () => {
      if (this.#voiceShown) return;
      this.#voiceShown = true;
      this.#showFullVoiceText();
    };
    this.#speaker.onIdle = () => {
      this.#showFullVoiceText();
      this.#endVoice();
    };
    await listen<number>("mic-level", (e) => (this.level = e.payload));
    await listen("mic-ready", () => (this.micReady = true));
    await listen("mic-transcribing", () => {
      if (this.voice === "listening") this.voice = "thinking";
    });
    await listen<{ text: string; error: string }>("mic-result", (e) => {
      if (this.voice === "idle") return; // cancelled meanwhile
      const { text, error } = e.payload;
      if (error) this.error = error;
      else if (!text.trim()) this.error = "No te escuché. Pulsa el micrófono y vuelve a intentarlo.";
      if (error || !text.trim()) return this.#endVoice();
      this.send(text, "voice");
    });
    await listen<{ id: number; text: string }>("chat-delta", (e) => {
      if (e.payload.id !== this.#reqId) return;
      if (e.payload.id === this.#voiceReq) {
        this.#voiceFull += e.payload.text;
        this.#speaker.feed(e.payload.text);
        if (!this.#voiceShown) return;
      }
      const last = this.messages[this.messages.length - 1];
      if (last?.role === "assistant") last.content += e.payload.text;
    });
    await listen<{ id: number }>("chat-done", (e) => {
      if (e.payload.id !== this.#reqId) return;
      this.busy = false;
      if (e.payload.id === this.#voiceReq) this.#speaker.end();
    });
    await listen<{ id: number; message: string }>("chat-error", (e) => {
      if (e.payload.id !== this.#reqId) return;
      this.busy = false;
      if (e.payload.id === this.#voiceReq) this.cancelVoice();
      this.error = e.payload.message;
      const last = this.messages[this.messages.length - 1];
      if (last?.role === "assistant" && !last.content) this.messages.pop();
      if (/API key/i.test(e.payload.message)) this.hasKey = false;
    });
  }

  async saveKey(key: string) {
    this.error = "";
    try {
      await invoke("set_api_key", { key });
      this.hasKey = true;
    } catch (e) {
      this.error = String(e);
    }
  }

  /** `source` decides the reply style: "text" => text only, "voice" => short, speakable (voice phase). */
  async send(text: string, source: Source = "text") {
    const content = text.trim();
    if (!content || this.busy) return;
    if (source === "text" && this.voice !== "idle") this.cancelVoice();
    this.error = "";
    this.messages.push({ role: "user", content });
    const history = this.messages.map((m) => ({ ...m }));
    this.messages.push({ role: "assistant", content: "" });
    this.busy = true;
    this.#reqId += 1;
    if (source === "voice") {
      this.#voiceReq = this.#reqId;
      this.#voiceFull = "";
      this.#voiceShown = false;
      this.#speaker.reset();
      this.voice = "thinking";
    }
    invoke("chat_send", { id: this.#reqId, messages: history, source });
  }

  /** The mic button: start listening / finish now / interrupt whatever the assistant is doing by voice. */
  async toggleVoice() {
    if (this.voice === "idle") {
      this.error = "";
      try {
        this.micReady = false;
        await invoke("mic_start");
        this.voice = "listening";
        invoke("voice_activate", { active: true });
      } catch (e) {
        this.error = String(e);
      }
    } else if (this.voice === "listening") {
      invoke("mic_stop");
    } else {
      this.cancelVoice();
    }
  }

  /** Stop recording / speaking and leave the voice look. */
  cancelVoice() {
    if (this.voice === "idle") return;
    if (this.voice === "listening") invoke("mic_cancel");
    this.#speaker.stop();
    this.#showFullVoiceText(); // interrupted: show the rest of the answer as text
    this.#voiceReq = -1;
    this.#endVoice();
  }

  #showFullVoiceText() {
    const last = this.messages[this.messages.length - 1];
    if (this.#voiceReq >= 0 && last?.role === "assistant" && this.#voiceFull) last.content = this.#voiceFull;
  }

  #endVoice() {
    if (this.voice === "idle") return;
    this.voice = "idle";
    this.level = 0;
    invoke("voice_activate", { active: false });
  }

  clear() {
    if (this.busy) return;
    this.messages = [];
    this.error = "";
  }
}

export const chat = new Chat();
