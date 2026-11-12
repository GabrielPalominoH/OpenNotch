import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

// Model output is untrusted HTML once rendered: sanitize, and drop anything that could load or navigate.
const CLEAN = {
  FORBID_TAGS: ["img", "iframe", "style", "script", "form", "input", "button", "svg", "math"],
  FORBID_ATTR: ["style", "onerror", "onclick", "src", "srcset"],
  ALLOWED_URI_REGEXP: /^(?:https?:)/i,
};

/** Markdown -> sanitized HTML. `caret` appends the streaming cursor inside the last text block. */
export function renderMarkdown(src: string, caret = false): string {
  const html = DOMPurify.sanitize(marked.parse(src, { async: false }) as string, CLEAN) as unknown as string;
  if (!caret) return html;
  const tail = /(<\/(?:p|li|h[1-6]|pre)>)((?:\s*<\/(?:ul|ol|blockquote)>)*\s*)$/;
  return tail.test(html) ? html.replace(tail, '<span class="caret"></span>$1$2') : html + '<span class="caret"></span>';
}
