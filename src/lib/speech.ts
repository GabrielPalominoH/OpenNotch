import { invoke } from "@tauri-apps/api/core";

/**
 * Text-to-speech: Gemini's neural female voice (falls back to the Windows voices of `speechSynthesis` if it fails).
 * The answer is spoken sentence by sentence while it streams in, so the voice starts before the text is complete.
 */

/** Markdown / symbols that must not be read aloud. */
export function speakable(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, "")
    .replace(/^\s*[-*•]\s+/gm, "")
    .replace(/[*_#>~|]/g, "")
    .replace(/\p{Extended_Pictographic}/gu, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** Splits off complete sentences; returns them plus the unfinished remainder. */
export function splitSentences(buf: string, final = false): { done: string[]; rest: string } {
  const done: string[] = [];
  const re = /[\s\S]*?(?:[.!?…]+["')\]]?(?=\s)|\n)/y;
  let pos = 0;
  let acc = "";
  for (;;) {
    re.lastIndex = pos;
    const m = re.exec(buf);
    if (!m) break;
    pos += m[0].length;
    acc += m[0];
    // very short pieces ("Sí.", "Ok.") are merged with the next sentence: one utterance sounds more natural
    if (acc.trim().length >= 18) {
      done.push(acc);
      acc = "";
    }
  }
  let rest = acc + buf.slice(pos);
  if (final && rest.trim()) {
    done.push(rest);
    rest = "";
  }
  return { done, rest };
}

function pickVoice(): SpeechSynthesisVoice | undefined {
  const es = speechSynthesis.getVoices().filter((v) => v.lang.toLowerCase().startsWith("es"));
  return es.find((v) => /natural|online/i.test(v.name)) ?? es.find((v) => /helena|sabina|elvira|laura/i.test(v.name)) ?? es[0];
}

/** Neural voice (Gemini TTS, else local Kokoro) as a playable blob URL; rejects when unavailable (quota, offline...). */
async function neuralClip(text: string): Promise<string> {
  let b64: string;
  try {
    b64 = await invoke<string>("tts_synthesize", { text }); // Gemini TTS (female neural voice)
  } catch {
    b64 = await invoke<string>("tts_local", { text }); // offline Kokoro if Gemini is saturated / offline
  }
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
}

export class Speaker {
  #buf = "";
  #pending = 0;
  #ended = false;
  #gen = 0;
  #chain: Promise<void> = Promise.resolve();
  #audio: HTMLAudioElement | null = null;
  onStart: () => void = () => {};
  /** called with the raw sentence text right when its audio starts */
  onSentence: (raw: string) => void = () => {};
  onIdle: () => void = () => {};

  reset() {
    this.stop();
    this.#ended = false;
  }

  feed(delta: string) {
    this.#buf += delta;
    const { done, rest } = splitSentences(this.#buf);
    this.#buf = rest;
    done.forEach((s) => this.#say(s));
  }

  /** The answer is complete: speak whatever is left, then report idle once the queue drains. */
  end() {
    const { done } = splitSentences(this.#buf, true);
    this.#buf = "";
    done.forEach((s) => this.#say(s));
    this.#ended = true;
    this.#check();
  }

  stop() {
    this.#gen++;
    this.#buf = "";
    this.#pending = 0;
    this.#ended = true;
    this.#chain = Promise.resolve();
    if (this.#audio) {
      this.#audio.pause();
      this.#audio = null;
    }
    speechSynthesis.cancel();
  }

  /** Sentences are synthesized right away (in parallel) but played strictly in order. */
  #say(raw: string) {
    const text = speakable(raw);
    if (!text) return;
    const gen = this.#gen;
    this.#pending++;
    const clip = neuralClip(text).catch(() => null);
    this.#chain = this.#chain.then(async () => {
      if (gen !== this.#gen) return;
      const url = await clip;
      if (gen !== this.#gen) return;
      if (url) await this.#play(url, gen, raw);
      else await this.#fallback(text, gen, raw);
      if (gen !== this.#gen) return;
      this.#pending = Math.max(0, this.#pending - 1);
      this.#check();
    });
  }

  #play(url: string, gen: number, raw: string): Promise<void> {
    return new Promise((resolve) => {
      const a = new Audio(url);
      a.playbackRate = 1.1; // a touch faster: less "read-aloud" feel
      this.#audio = a;
      const done = () => {
        URL.revokeObjectURL(url);
        resolve();
      };
      a.onended = done;
      a.onerror = done;
      a.onpause = () => gen !== this.#gen && done();
      a.play().then(
        () => {
          if (gen !== this.#gen) return;
          this.onStart();
          this.onSentence(raw);
        },
        done,
      );
    });
  }

  /** Windows voice, only when the neural one failed. */
  #fallback(text: string, gen: number, raw: string): Promise<void> {
    return new Promise((resolve) => {
      const u = new SpeechSynthesisUtterance(text);
      const v = pickVoice();
      if (v) u.voice = v;
      u.lang = v?.lang ?? "es-ES";
      u.rate = 1.05;
      u.onstart = () => {
        if (gen !== this.#gen) return;
        this.onStart();
        this.onSentence(raw);
      };
      u.onend = () => resolve();
      u.onerror = () => resolve();
      speechSynthesis.speak(u);
    });
  }

  #check() {
    if (this.#ended && this.#pending === 0) this.onIdle();
  }
}
