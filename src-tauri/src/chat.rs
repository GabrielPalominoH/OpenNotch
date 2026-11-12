//! Gemini chat: API key in the Windows Credential Manager, streaming over SSE, tool calling.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

const SERVICE: &str = "OpenNotch";
const ACCOUNT: &str = "gemini_api_key";
// Fastest current Flash-Lite (docs: "fastest 3.5 model"). The big Flash/Pro models spent 30-60 s thinking per answer.
const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
/// Tried in order when the chosen model is overloaded / rate-limited / unavailable.
const FALLBACK_MODELS: [&str; 2] = ["gemini-3.1-flash-lite", "gemini-3.8-flash"];
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

const SYSTEM_BASE: &str = "You are OpenNotch, a fast assistant that lives in a notch at the top of the user's Windows screen. \
ALWAYS reply in Spanish (español), whatever language the user writes in or the tools return; only use another language when the user explicitly asks you to translate or to answer in it. Be concise and direct. You may use light Markdown (bold, short lists, `code`, fenced code blocks) when it helps; no headings, tables or images.";
const SYSTEM_VOICE: &str = " Your reply will be spoken aloud: use short plain sentences, no markdown, no lists, no code blocks.";

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize, Deserialize)]
struct Settings {
    model: String,
}

#[derive(Serialize, Clone)]
struct Delta {
    id: u64,
    text: String,
}
#[derive(Serialize, Clone)]
struct Done {
    id: u64,
}
#[derive(Serialize, Clone)]
struct Failure {
    id: u64,
    message: String,
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
}

/// Credential Manager first, `GEMINI_API_KEY` env var as a dev fallback.
fn api_key() -> Option<String> {
    let stored = entry().ok().and_then(|e| e.get_password().ok());
    stored
        .or_else(|| std::env::var("GEMINI_API_KEY").ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn settings_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

fn load_model(app: &AppHandle) -> String {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .map(|s| s.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

#[tauri::command]
pub fn has_api_key() -> bool {
    api_key().is_some()
}

#[tauri::command]
pub fn set_api_key(key: String) -> Result<(), String> {
    let key = key.trim();
    // Google has changed key formats before, so don't pin a prefix: the API itself rejects bad keys.
    if key.len() < 20 || key.chars().any(char::is_whitespace) {
        return Err("La clave parece incompleta o tiene espacios.".into());
    }
    entry()?.set_password(key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_api_key() -> Result<(), String> {
    entry()?.delete_credential().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_model(app: AppHandle) -> String {
    load_model(&app)
}

#[tauri::command]
pub fn set_model(app: AppHandle, model: String) -> Result<(), String> {
    let path = settings_path(&app).ok_or("no config dir")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string(&Settings { model }).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Parse the JSON payload of one Gemini SSE `data:` chunk into the model's content parts
/// (thought summaries are dropped). Parts are kept raw so `thoughtSignature` can be echoed back
/// verbatim when a function call is answered, as Gemini 3 requires.
pub fn parse_chunk(data: &str) -> Result<Vec<serde_json::Value>, String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return Ok(vec![]);
    };
    if let Some(msg) = v["error"]["message"].as_str() {
        return Err(msg.to_string());
    }
    if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
        return Err(format!("Google bloqueó el mensaje ({reason})."));
    }
    let parts = v["candidates"][0]["content"]["parts"].as_array().cloned().unwrap_or_default();
    Ok(parts.into_iter().filter(|p| p["thought"] != true).collect())
}

fn friendly_http_error(status: u16, body: &str) -> String {
    let api_msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
    let key_problem = api_msg.as_deref().is_some_and(|m| m.to_lowercase().contains("api key"));
    match status {
        401 | 403 => "API key inválida o sin permisos.".into(),
        400 if key_problem => "API key inválida.".into(),
        429 => "Gemini está saturado o alcanzaste el límite gratuito. Espera un momento.".into(),
        _ => api_msg.unwrap_or_else(|| format!("Error HTTP {status}")),
    }
}

fn tools_schema() -> serde_json::Value {
    serde_json::json!([{ "functionDeclarations": [
        {
            "name": "remember",
            "description": "Save a lasting fact about the user (preference, habit, name, routine, how they like answers) so it is remembered in future chats. Use when the user says 'recuerda que...' or clearly states a lasting preference. One short sentence in Spanish.",
            "parameters": {"type": "object", "properties": {"fact": {"type": "string", "description": "The fact, e.g. 'Prefiere respuestas cortas'."}}, "required": ["fact"]}
        },
        {
            "name": "forget_memory",
            "description": "Delete remembered facts about the user that contain the given words. Use when the user says 'olvida que...' or asks to forget something.",
            "parameters": {"type": "object", "properties": {"query": {"type": "string", "description": "Words that identify the fact to delete."}}, "required": ["query"]}
        },
        {
            "name": "create_reminder",
            "description": "Create a reminder that fires a notification at a given time. Give EITHER minutes_from_now OR at.",
            "parameters": {"type": "object", "properties": {
                "text": {"type": "string", "description": "What to remind the user about."},
                "minutes_from_now": {"type": "number", "description": "Relative delay in minutes."},
                "at": {"type": "string", "description": "Absolute LOCAL time, format YYYY-MM-DDTHH:MM."}
            }, "required": ["text"]}
        },
        {
            "name": "list_reminders",
            "description": "List the user's pending reminders."
        },
        {
            "name": "delete_reminder",
            "description": "Delete a pending reminder. Give its id (from list_reminders) or a piece of its text. If several match, ask the user which one.",
            "parameters": {"type": "object", "properties": {
                "id": {"type": "number", "description": "Reminder id from list_reminders."},
                "text": {"type": "string", "description": "Part of the reminder text, when the id is unknown."}
            }}
        },
        {
            "name": "look_at_screen",
            "description": "Take a screenshot of the user's screen right now and answer a question about what is visible (where something is, what a sentence or error means, read text, describe the window). Use whenever the user refers to something on their screen.",
            "parameters": {"type": "object", "properties": {
                "question": {"type": "string", "description": "What to find out about the screen, in the user's words."}
            }, "required": ["question"]}
        },
        {
            "name": "pomodoro",
            "description": "Control the pomodoro timer (25 min focus / 5 min break).",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["start", "pause", "reset", "skip", "status"]}
            }, "required": ["action"]}
        },
        {
            "name": "web_search",
            "description": "Search the web for current or factual information: news, prices, weather, sports, release dates, people, anything you are not sure about or that may have changed. Returns the top results (title, link, snippet) and the text of the first pages.",
            "parameters": {"type": "object", "properties": {
                "query": {"type": "string", "description": "What to look up, phrased as a good search query."}
            }, "required": ["query"]}
        },
        {
            "name": "media_control",
            "description": "Control the music/media currently playing on the PC.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["play_pause", "next", "previous", "status"]}
            }, "required": ["action"]}
        }
    ]}])
}

fn fmt_local(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// Runs one tool call and returns the text handed back to the model (errors included).
async fn run_tool(app: &AppHandle, name: &str, input: &serde_json::Value) -> Result<String, String> {
    match name {
        "remember" => {
            let fact = input["fact"].as_str().ok_or("falta fact")?;
            Ok(if crate::memory::add(app, fact)? { "Guardado.".to_string() } else { "Ya lo sabía.".to_string() })
        }
        "forget_memory" => {
            let n = crate::memory::forget(app, input["query"].as_str().ok_or("falta query")?)?;
            Ok(if n > 0 { format!("Olvidado ({n}).") } else { "No tenía nada así guardado.".to_string() })
        }
        "create_reminder" => {
            let text = input["text"].as_str().ok_or("falta text")?;
            let due = if let Some(m) = input["minutes_from_now"].as_f64() {
                chrono::Utc::now().timestamp_millis() + (m * 60_000.0) as i64
            } else if let Some(at) = input["at"].as_str() {
                crate::reminders::parse_local(at)?
            } else {
                return Err("indica minutes_from_now o at".into());
            };
            if due < chrono::Utc::now().timestamp_millis() {
                return Err("Esa hora ya pasó.".into());
            }
            let r = crate::reminders::add(app, text, due)?;
            Ok(format!("Recordatorio creado para {}: {}", fmt_local(r.due_at_ms), r.text))
        }
        "list_reminders" => {
            let rows: Vec<_> = crate::reminders::list(app)?.into_iter().filter(|r| !r.fired && !r.done).collect();
            if rows.is_empty() {
                return Ok("No hay recordatorios pendientes.".into());
            }
            Ok(rows.iter().map(|r| format!("- [id {}] {} — {}", r.id, fmt_local(r.due_at_ms), r.text)).collect::<Vec<_>>().join("\n"))
        }
        "delete_reminder" => {
            let rows = crate::reminders::list(app)?;
            let target: Vec<_> = if let Some(id) = input["id"].as_i64() {
                rows.into_iter().filter(|r| r.id == id).collect()
            } else if let Some(t) = input["text"].as_str() {
                let t = t.to_lowercase();
                rows.into_iter().filter(|r| !r.done && r.text.to_lowercase().contains(&t)).collect()
            } else {
                return Err("indica id o text".into());
            };
            match target.as_slice() {
                [] => Err("No encontré ese recordatorio.".into()),
                [r] => {
                    crate::reminders::reminder_delete(app.clone(), r.id)?;
                    Ok(format!("Recordatorio eliminado: {}", r.text))
                }
                many => Err(format!("Varios coinciden, pregunta cuál: {}", many.iter().map(|r| format!("[id {}] {}", r.id, r.text)).collect::<Vec<_>>().join("; "))),
            }
        }
        "look_at_screen" => {
            let question = input["question"].as_str().unwrap_or("Describe lo que se ve en la pantalla.").to_string();
            let jpeg = tauri::async_runtime::spawn_blocking(|| crate::screen::capture_jpeg(1600)).await.map_err(|e| e.to_string())??;
            ask_about_image(app, &question, &jpeg).await
        }
        "pomodoro" => {
            let action = input["action"].as_str().ok_or("falta action")?;
            let s = crate::pomodoro::control(app, action)?;
            Ok(format!(
                "Pomodoro: fase {:?}, {}, quedan {} s, ciclos {}/4",
                s.phase,
                if s.running { "corriendo" } else { "detenido" },
                s.remaining_ms / 1000,
                s.completed
            ))
        }
        "web_search" => {
            let query = input["query"].as_str().ok_or("falta query")?;
            let t_s = std::time::Instant::now();
            let r = search_web(query).await;
            eprintln!("[web_search] {:?} en {:?}", query, t_s.elapsed());
            eprintln!("[web_search] {query:?} -> {}", match &r { Ok(t) => format!("ok {} chars: {}", t.len(), t.chars().take(160).collect::<String>().replace('\n', " ")), Err(e) => format!("ERR {e}") });
            r
        }
        "media_control" => {
            let action = input["action"].as_str().ok_or("falta action")?.to_string();
            tauri::async_runtime::spawn_blocking(move || crate::media::control(&action))
                .await
                .map_err(|e| e.to_string())?
        }
        other => Err(format!("herramienta desconocida: {other}")),
    }
}

/// Web search without any paid quota or API key. Search engines throttle scrapers (DuckDuckGo answered with an
/// "anomaly" challenge after a few queries), so several are tried in order: Bing, then DuckDuckGo. The text of the top
/// pages is then read so the model has real content to answer from (snippets alone rarely contain the number asked for).
async fn search_web(query: &str) -> Result<String, String> {
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::ACCEPT_LANGUAGE, "es-PE,es;q=0.9,en;q=0.6".parse().unwrap());
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|e| e.to_string())?;

    let mut results: Vec<SearchHit> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    // Bing web results and Bing News (recent headlines: portals' front pages rarely hold readable news text) in parallel.
    let web_req = client.get("https://www.bing.com/search").query(&[("q", query), ("setlang", "es")]).send();
    let news_req = client.get("https://www.bing.com/news/search").query(&[("q", query), ("format", "rss"), ("mkt", "es-PE")]).send();
    let (web, news_resp) = futures_util::future::join(web_req, news_req).await;
    match web {
        Ok(r) => match r.text().await {
            Ok(html) => results = parse_bing(&html),
            Err(e) => errors.push(format!("bing: {e}")),
        },
        Err(e) => errors.push(format!("bing: {e}")),
    }
    let news = match news_resp {
        Ok(r) => r.text().await.map(|x| parse_news_rss(&x)).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    // DuckDuckGo only as a fallback
    if results.is_empty() {
        match client.post("https://html.duckduckgo.com/html/").form(&[("q", query), ("kl", "wt-wt")]).send().await {
            Ok(r) => match r.text().await {
                Ok(html) => results = parse_ddg(&html),
                Err(e) => errors.push(format!("duckduckgo: {e}")),
            },
            Err(e) => errors.push(format!("duckduckgo: {e}")),
        }
    }
    if results.is_empty() && news.is_empty() {
        eprintln!("[web_search] sin resultados para {query:?}: {errors:?}");
        return Err("Ningún buscador devolvió resultados (puede ser un bloqueo temporal o falta de conexión).".into());
    }
    // Read the first two pages in parallel (best effort; many sites block or are script-only).
    let pages = futures_util::future::join_all(results.iter().take(2).map(|r| {
        let c = client.clone();
        let url = r.url.clone();
        async move {
            let resp = c.get(&url).timeout(std::time::Duration::from_secs(3)).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let body = resp.text().await.ok()?;
            let text = html_to_text(&body);
            (text.chars().count() > 200).then(|| text.chars().take(1800).collect::<String>())
        }
    }))
    .await;
    let mut out = String::new();
    if !news.is_empty() {
        out.push_str("Noticias recientes:\n");
        for n in news.iter().take(6) {
            out.push_str(&format!("- {n}\n"));
        }
        out.push('\n');
    }
    for (i, r) in results.iter().take(6).enumerate() {
        out.push_str(&format!("{}. {} — {}\n   {}\n", i + 1, r.title, r.url, r.snippet));
    }
    for (r, text) in results.iter().zip(pages) {
        if let Some(t) = text {
            out.push_str(&format!("\nContenido de {}:\n{t}\n", r.url));
        }
    }
    Ok(out)
}

struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| if let Some(h) = n.strip_prefix(['x', 'X']) { u32::from_str_radix(h, 16).ok() } else { n.parse::<u32>().ok() })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Items of a Bing News RSS feed as "title (date): summary".
fn parse_news_rss(xml: &str) -> Vec<String> {
    fn tag(block: &str, name: &str) -> String {
        let (o, c) = (format!("<{name}>"), format!("</{name}>"));
        block.find(&o).and_then(|a| block[a + o.len()..].find(&c).map(|b| strip_tags(&block[a + o.len()..a + o.len() + b]))).unwrap_or_default()
    }
    xml.split("<item>")
        .skip(1)
        .filter_map(|it| {
            let title = tag(it, "title");
            if title.is_empty() {
                return None;
            }
            let date = tag(it, "pubDate");
            let date = date.split(" 20").next().unwrap_or("").to_string() + date.split_once(" 20").map(|(_, r)| format!(" 20{}", r.split(' ').next().unwrap_or(""))).unwrap_or_default().as_str();
            let desc = tag(it, "description");
            Some(format!("{title} ({}){}", date.trim(), if desc.is_empty() { String::new() } else { format!(": {desc}") }))
        })
        .collect()
}

/// Bing wraps result links in a click tracker (`/ck/a?...&u=a1<base64url of the real URL>`): unwrap it.
fn unwrap_bing(href: &str) -> String {
    use base64::Engine;
    let href = decode_entities(href);
    if href.contains("bing.com/ck/a") {
        if let Some(u) = href.split(['?', '&']).find_map(|kv| kv.strip_prefix("u=a1")) {
            if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(u.trim_end_matches('=')) {
                if let Ok(url) = String::from_utf8(bytes) {
                    return url;
                }
            }
        }
    }
    href
}

/// Organic results of a Bing results page (`<li class="b_algo">`): (title, url, snippet).
fn parse_bing(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for block in html.split("<li class=\"b_algo\"").skip(1) {
        let block = block.split("</li>").next().unwrap_or(block);
        let Some(h2) = block.find("<h2") else { continue };
        let h = &block[h2..];
        let Some(hr) = h.find("href=\"") else { continue };
        let after = &h[hr + 6..];
        let Some(q) = after.find('"') else { continue };
        let url = unwrap_bing(&after[..q]);
        let Some(gt) = after.find('>') else { continue };
        let Some(end) = after[gt..].find("</a>") else { continue };
        let title = strip_tags(&after[gt + 1..gt + end]);
        let snippet = block
            .find("<p class=\"b_lineclamp")
            .or_else(|| block.find("<p"))
            .and_then(|k| {
                let seg = &block[k..];
                let s0 = seg.find('>')? + 1;
                let e0 = seg.find("</p>")?;
                Some(strip_tags(&seg[s0..e0]))
            })
            .unwrap_or_default();
        if url.starts_with("http") && !title.is_empty() {
            hits.push(SearchHit { title, url, snippet });
        }
        if hits.len() >= 8 {
            break;
        }
    }
    hits
}

/// Results page of html.duckduckgo.com -> (title, url, snippet). Redirect links (`/l/?uddg=`) are unwrapped.
fn parse_ddg(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("class=\"result__a\"") {
        rest = &rest[i..];
        let Some(h) = rest.find("href=\"") else { break };
        let after = &rest[h + 6..];
        let Some(q) = after.find('"') else { break };
        let mut url = decode_entities(&after[..q]);
        if let Some(u) = url.split("uddg=").nth(1) {
            url = percent_decode(u.split('&').next().unwrap_or(u));
        }
        if url.starts_with("//") {
            url = format!("https:{url}");
        }
        let Some(gt) = after.find('>') else { break };
        let Some(end) = after[gt..].find("</a>") else { break };
        let title = strip_tags(&after[gt + 1..gt + end]);
        let block_end = after.find("class=\"result__a\"").unwrap_or(after.len());
        let block = &after[..block_end];
        let snippet = block
            .find("result__snippet")
            .and_then(|k| {
                let seg = &block[k..];
                let s0 = seg.find('>')? + 1;
                let e0 = seg.find("</a>").or_else(|| seg.find("</td>"))?;
                Some(strip_tags(&seg[s0..e0]))
            })
            .unwrap_or_default();
        if url.starts_with("http") && !title.is_empty() && !url.contains("duckduckgo.com/y.js") {
            hits.push(SearchHit { title, url, snippet });
        }
        rest = &after[block_end.min(after.len())..];
        if hits.len() >= 8 {
            break;
        }
    }
    hits
}

/// Visible text of an HTML page (scripts/styles dropped, whitespace collapsed).
fn html_to_text(html: &str) -> String {
    let mut cleaned = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "head"] {
        let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
        while let Some(a) = cleaned.to_lowercase().find(&open) {
            let lower = cleaned.to_lowercase();
            let Some(b) = lower[a..].find(&close) else {
                cleaned.truncate(a);
                break;
            };
            cleaned.replace_range(a..a + b + close.len(), " ");
        }
    }
    strip_tags(&cleaned).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One-off multimodal request: the screenshot plus the user's question; returns Gemini's description/answer.
async fn ask_about_image(app: &AppHandle, question: &str, jpeg: &[u8]) -> Result<String, String> {
    use base64::Engine;
    let key = api_key().ok_or("Falta la API key de Gemini.")?;
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [
            {"inline_data": {"mime_type": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(jpeg)}},
            {"text": format!("This is a screenshot of the user's screen. Answer in Spanish, concisely and precisely, about what is visible. If asked where something is, say where on the screen (top/bottom/left/right/center) and in which window. Question: {question}")}
        ]}],
        "generationConfig": {"temperature": 0.2, "maxOutputTokens": 1024},
    });
    let client = reqwest::Client::new();
    let models = candidate_models(&load_model(app));
    let mut last_err = String::from("No pude analizar la pantalla.");
    for (i, model) in models.iter().enumerate() {
        let r = client
            .post(format!("{API_BASE}/{model}:generateContent"))
            .header("x-goog-api-key", &key)
            .timeout(std::time::Duration::from_secs(40))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Sin conexión: {e}"))?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            last_err = friendly_http_error(status.as_u16(), &text);
            if is_fallback_status(status.as_u16()) && i + 1 < models.len() {
                continue;
            }
            return Err(last_err);
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let out = extract_text(&v);
        return if out.is_empty() { Err("No vi nada útil en la pantalla.".into()) } else { Ok(out) };
    }
    Err(last_err)
}

/// Speech -> text with Gemini (audio input is part of the normal free API; no extra service or key).
pub async fn transcribe_wav(app: &AppHandle, wav: &[u8]) -> Result<String, String> {
    use base64::Engine;
    let key = api_key().ok_or("Falta la API key de Gemini.")?;
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [
            {"text": "Transcribe this audio exactly as spoken, in the language spoken (usually Spanish). Output ONLY the transcription: no quotes, no comments, no translation. If there is no intelligible speech, output nothing. The speaker is talking to a desktop assistant named \"Notch\" (OpenNotch); words you may hear: notch, pomodoro, recordatorio, temporizador, música, volumen, pausa, siguiente, anterior, Spotify, Opera, Gemini."},
            {"inline_data": {"mime_type": "audio/wav", "data": base64::engine::general_purpose::STANDARD.encode(wav)}}
        ]}],
        "generationConfig": {"temperature": 0, "maxOutputTokens": 512},
    });
    let client = reqwest::Client::new();
    let models = candidate_models(&load_model(app));
    let mut last_err = String::from("No se pudo transcribir el audio.");
    for (i, model) in models.iter().enumerate() {
        let r = client
            .post(format!("{API_BASE}/{model}:generateContent"))
            .header("x-goog-api-key", &key)
            .timeout(std::time::Duration::from_secs(25))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Sin conexión: {e}"))?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            eprintln!("[transcribe] {model} -> HTTP {status}");
            last_err = friendly_http_error(status.as_u16(), &text);
            if is_fallback_status(status.as_u16()) && i + 1 < models.len() {
                continue;
            }
            return Err(last_err);
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        return Ok(extract_text(&v));
    }
    Err(last_err)
}

fn extract_text(v: &serde_json::Value) -> String {
    v["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|ps| ps.iter().filter(|p| p["thought"].as_bool() != Some(true)).filter_map(|p| p["text"].as_str()).collect::<String>())
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .trim()
        .to_string()
}

/// Female neural voice (Gemini TTS). Fast "lite" model first, regular one as fallback.
const TTS_MODELS: [&str; 2] = ["gemini-3.8-flash-lite-tts", "gemini-3.8-flash-tts"];
const TTS_VOICE: &str = "Aoede";

/// Audio data (base64 WAV) from an Interactions API response.
fn extract_tts_audio(v: &serde_json::Value) -> Option<String> {
    v["steps"]
        .as_array()?
        .iter()
        .filter(|s| s["type"] == "model_output")
        .flat_map(|s| s["content"].as_array().cloned().unwrap_or_default())
        .filter(|c| c["type"] == "audio")
        .filter_map(|c| c["data"].as_str().map(str::to_string))
        .last()
}

/// Text -> speech. Returns the audio as base64 (WAV).
#[tauri::command]
pub async fn tts_synthesize(text: String) -> Result<String, String> {
    let key = api_key().ok_or("Falta la API key de Gemini.")?;
    let client = reqwest::Client::new();
    let mut last_err = String::from("No se pudo generar la voz.");
    for model in TTS_MODELS {
        let body = serde_json::json!({
            "model": model,
            "input": [{"type": "user_input", "content": [{"type": "text", "text": text}]}],
            "response_format": {"type": "audio"},
            "generation_config": {"speech_config": [{"voice": TTS_VOICE}]},
        });
        let r = client
            .post("https://generativelanguage.googleapis.com/v1beta/interactions")
            .header("x-goog-api-key", &key)
            .timeout(std::time::Duration::from_secs(20))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Sin conexión: {e}"))?;
        let status = r.status();
        let txt = r.text().await.unwrap_or_default();
        if !status.is_success() {
            eprintln!("[tts] {model} -> HTTP {status}: {}", txt.chars().take(300).collect::<String>());
            last_err = friendly_http_error(status.as_u16(), &txt);
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
        if let Some(a) = extract_tts_audio(&v) {
            return Ok(a);
        }
        eprintln!("[tts] {model}: respuesta sin audio: {}", txt.chars().take(300).collect::<String>());
    }
    Err(last_err)
}

fn candidate_models(primary: &str) -> Vec<String> {
    let mut v = vec![primary.to_string()];
    for m in FALLBACK_MODELS {
        if !v.iter().any(|x| x == m) {
            v.push(m.to_string());
        }
    }
    v
}

/// Worth retrying on another model: overloaded, rate-limited, upstream error, or model not found.
fn is_fallback_status(status: u16) -> bool {
    matches!(status, 404 | 429 | 500 | 502 | 503 | 504)
}

/// Heuristic: does this message clearly need live information from the internet?
fn needs_web(q: &str) -> bool {
    let q = q.to_lowercase();
    const SKIP: [&str; 7] = ["recordatorio", "recuérdame", "recuerdame", "pomodoro", "pantalla", "música", "musica"];
    if SKIP.iter().any(|w| q.contains(w)) {
        return false;
    }
    const KEYS: [&str; 46] = [
        "hoy", "ahora mismo", "actualmente", "actual ", "última", "ultima", "último", "ultimo", "ayer", "esta semana", "noticia", "clima",
        "temperatura", "pronóstico", "pronostico", "precio", "cuesta", "cuánto vale", "cuanto vale", "dólar", "dolar", "tipo de cambio",
        "bitcoin", "cotización", "cotizacion", "resultado", "partido", "marcador", "quién ganó", "quien gano", "horario", "estreno",
        "lanzamiento", "versión", "version", "busca", "investiga", "averigua", "en internet", "en la web", "presidente", "ceo de",
        "2025", "2026", "vuelo", "tráfico",
    ];
    KEYS.iter().any(|k| q.contains(k))
}

/// One pooled HTTP client for the whole app: keeps the TLS/HTTP2 connection to Gemini warm between questions.
fn shared_client() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(300))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

/// How long the first model may stay silent before the next one is raced against it (Gemini's free tier sometimes queues a
/// request for 8+ s while the next model answers in ~1 s).
const HEDGE_AFTER: std::time::Duration = std::time::Duration::from_millis(2500);

/// POST one streamed request. Returns the response whatever its status; drops `thinkingConfig` if the model rejects it.
async fn request_model(client: reqwest::Client, model: String, key: String, mut body: serde_json::Value) -> Result<reqwest::Response, String> {
    let url = format!("{API_BASE}/{model}:streamGenerateContent?alt=sse");
    let send = |b: &serde_json::Value| client.post(&url).header("x-goog-api-key", &key).json(b).send();
    let mut r = send(&body).await.map_err(|e| format!("Sin conexión: {e}"))?;
    if r.status().as_u16() == 400 && body["generationConfig"].get("thinkingConfig").is_some() {
        eprintln!("[chat] {model} rechazó thinkingConfig; reintento sin él");
        if let Some(o) = body["generationConfig"].as_object_mut() {
            o.remove("thinkingConfig");
        }
        r = send(&body).await.map_err(|e| format!("Sin conexión: {e}"))?;
    }
    Ok(r)
}

/// First successful response among the candidate models: tries them in order, and if one is slow, races the next.
async fn first_response(
    client: &reqwest::Client,
    key: &str,
    body: &serde_json::Value,
    models: &[String],
    t0: std::time::Instant,
) -> Result<reqwest::Response, String> {
    type Rq = tauri::async_runtime::JoinHandle<Result<reqwest::Response, String>>;
    let launch = |m: &String| -> Rq {
        let (c, m2, k, b) = (client.clone(), m.clone(), key.to_string(), body.clone());
        tauri::async_runtime::spawn(async move { request_model(c, m2, k, b).await })
    };
    let mut last_err = String::from("Ningún modelo respondió.");
    let mut handles: Vec<Rq> = Vec::new();
    let mut next = 0usize;
    let mut hedged = false;
    loop {
        if handles.is_empty() {
            let Some(m) = models.get(next) else { return Err(last_err) };
            handles.push(launch(m));
            next += 1;
        }
        let mut sel = futures_util::future::select_all(std::mem::take(&mut handles));
        let done = if !hedged && next < models.len() {
            match tokio::time::timeout(HEDGE_AFTER, &mut sel).await {
                Ok(x) => x,
                Err(_) => {
                    handles = sel.into_inner();
                    eprintln!("[chat] {} sin respuesta a los {:?}: pruebo {} en paralelo", models[next - 1], t0.elapsed(), models[next]);
                    handles.push(launch(&models[next]));
                    next += 1;
                    hedged = true;
                    continue;
                }
            }
        } else {
            sel.await
        };
        let (res, _, rest) = done;
        handles = rest;
        match res.map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(r) if r.status().is_success() => {
                eprintln!("[chat] respuesta OK tras {:?}", t0.elapsed());
                for h in handles {
                    h.abort();
                }
                return Ok(r);
            }
            Ok(r) => {
                let status = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                last_err = friendly_http_error(status, &text);
                eprintln!("[chat] HTTP {status} tras {:?}", t0.elapsed());
                if !is_fallback_status(status) {
                    for h in handles {
                        h.abort();
                    }
                    return Err(last_err);
                }
            }
            Err(e) => {
                for h in handles {
                    h.abort();
                }
                return Err(e);
            }
        }
    }
}

/// One model turn, streamed. Returns the raw content parts the model produced.
async fn stream_turn(
    app: &AppHandle,
    id: u64,
    key: &str,
    system: &str,
    contents: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    // Minimal thinking: answers start sooner (thinking tokens are generated before the first visible word).
    let body = serde_json::json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": contents,
        "tools": tools_schema(),
        "generationConfig": {"maxOutputTokens": 8192, "thinkingConfig": {"thinkingLevel": "minimal"}},
    });

    let client = shared_client();
    let t_turn = std::time::Instant::now();
    let models = candidate_models(&load_model(app));
    let resp = first_response(&client, key, &body, &models, t_turn).await?;

    let mut first_text = true;
    let mut parts: Vec<serde_json::Value> = Vec::new();
    // Buffer raw bytes: a chunk boundary can split a multi-byte UTF-8 char.
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            for part in parse_chunk(data.trim())? {
                if let Some(text) = part["text"].as_str() {
                    if first_text {
                        first_text = false;
                        eprintln!("[chat] primer texto a los {:?}", t_turn.elapsed());
                    }
                    let _ = app.emit("chat-delta", Delta { id, text: text.to_string() });
                }
                parts.push(part);
            }
        }
    }
    Ok(parts)
}

async fn stream_chat(app: &AppHandle, id: u64, messages: &[ChatMessage], voice: bool) -> Result<String, String> {
    let key = api_key().ok_or("Falta la API key de Gemini.")?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M (%A), UTC%:z");
    let mut system = format!("{SYSTEM_BASE} Current local time: {now}. Use your tools to act on the user's PC when asked (reminders, pomodoro, music); after a tool runs, confirm briefly. Use web_search whenever the answer depends on the real world right now or on specifics you could get wrong: news, weather, prices, exchange rates, scores and sports results, schedules and opening hours, current office holders, latest versions/releases, products, companies, apps, people, places, events, laws, anything dated 2024 or later, anything you are not sure about, and whenever the user says search/busca/investiga/averigua or asks for the latest/current/today. Do not say you cannot browse and do not guess: search first, then answer from the results and mention the main source names. Skip the search only for timeless knowledge (definitions, how things work, math, grammar, writing, code, advice) and chit-chat; then answer from the results and mention the main source names. When the user asks about anything on their screen (where is X, what does this text mean, what is this error, what is open, read this) call look_at_screen IMMEDIATELY. Never ask for permission or confirmation to take the screenshot, never announce it, never say you are going to look: just call the tool and reply only with the answer to what was asked. You CAN delete reminders with delete_reminder (use list_reminders first if you need the id). Call web_search at most twice per question; if the results are thin, answer with what they contain and say what is missing instead of searching again.");
    // Questions that obviously need the internet: search right away (saves a whole model round trip) and hand the results over.
    if let Some(q) = messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()).filter(|q| needs_web(q)) {
        let _ = app.emit("chat-tool", serde_json::json!({ "id": id, "name": "web_search" }));
        let t_s = std::time::Instant::now();
        match tokio::time::timeout(std::time::Duration::from_secs(7), search_web(&q)).await {
            Ok(Ok(res)) => {
                eprintln!("[chat] búsqueda previa de {q:?} en {:?} ({} chars)", t_s.elapsed(), res.len());
                let res: String = res.chars().take(3200).collect();
                system.push_str(&format!(" Web search results already fetched for the user's latest message (today is {now}); answer from them, mention the main source names, and only call web_search again if they clearly do not contain the answer:\n{res}\n"));
            }
            other => eprintln!("[chat] búsqueda previa falló o tardó demasiado: {:?}", other.err().map(|_| "timeout")),
        }
    }
    system.push_str(&crate::memory::prompt_block(app));
    system.push_str(" When the user asks you to remember something call remember; when they ask you to forget something call forget_memory. Preferences you notice on your own are saved automatically in the background, so do not call remember for those.");
    if voice {
        system.push_str(SYSTEM_VOICE);
    }
    let mut contents: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            let role = if m.role == "assistant" { "model" } else { "user" };
            serde_json::json!({"role": role, "parts": [{"text": m.content}]})
        })
        .collect();

    // Tool loop: model asks for tools -> we run them -> model continues. Capped to stay bounded.
    for _ in 0..5 {
        let parts = stream_turn(app, id, &key, &system, &contents).await?;
        let calls: Vec<&serde_json::Value> = parts.iter().filter(|p| p.get("functionCall").is_some()).collect();
        if calls.is_empty() {
            return Ok(parts.iter().filter(|p| p["thought"].as_bool() != Some(true)).filter_map(|p| p["text"].as_str()).collect());
        }
        let mut responses = Vec::new();
        for p in &calls {
            let call = &p["functionCall"];
            let name = call["name"].as_str().unwrap_or_default();
            let _ = app.emit("chat-tool", serde_json::json!({ "id": id, "name": name }));
            eprintln!("[chat] herramienta: {name} {}", call["args"]);
            let result = match run_tool(app, name, &call["args"]).await {
                Ok(s) => serde_json::json!({ "result": s }),
                Err(e) => serde_json::json!({ "error": e }),
            };
            let mut fr = serde_json::json!({ "name": name, "response": result });
            if let Some(cid) = call.get("id") {
                fr["id"] = cid.clone();
            }
            responses.push(serde_json::json!({ "functionResponse": fr }));
        }
        contents.push(serde_json::json!({"role": "model", "parts": parts}));
        contents.push(serde_json::json!({"role": "user", "parts": responses}));
    }
    Err("Demasiados pasos con herramientas.".into())
}

/// Starts a streamed reply. Returns immediately; progress arrives as
/// `chat-delta` / `chat-done` / `chat-error` events tagged with `id`.
#[tauri::command]
pub fn chat_send(app: AppHandle, id: u64, messages: Vec<ChatMessage>, source: String) {
    tauri::async_runtime::spawn(async move {
        match stream_chat(&app, id, &messages, source == "voice").await {
            Ok(answer) => {
                let _ = app.emit("chat-done", Done { id });
                // Learn from this exchange in the background: the user never waits for it.
                let (app2, convo) = (app.clone(), messages.clone());
                tauri::async_runtime::spawn(async move { learn(&app2, &convo, &answer).await });
            }
            Err(message) => {
                let _ = app.emit("chat-error", Failure { id, message });
            }
        }
    });
}

/// Sends a memory-module prompt and returns the JSON `{"add": [...], "remove": [...]}` it answers with.
async fn memory_json(app: &AppHandle, prompt: String) -> Option<serde_json::Value> {
    let key = api_key()?;
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [{"text": prompt}]}],
        "generationConfig": {"temperature": 0, "maxOutputTokens": 400, "responseMimeType": "application/json", "thinkingConfig": {"thinkingLevel": "minimal"}},
    });
    let client = shared_client();
    let model = load_model(app);
    let send = |b: &serde_json::Value| client.post(format!("{API_BASE}/{model}:generateContent")).header("x-goog-api-key", &key).timeout(std::time::Duration::from_secs(20)).json(b).send();
    let mut r = send(&body).await.ok()?;
    if r.status().as_u16() == 400 {
        let mut b2 = body.clone();
        if let Some(o) = b2["generationConfig"].as_object_mut() {
            o.remove("thinkingConfig");
        }
        r = send(&b2).await.ok()?;
    }
    if !r.status().is_success() {
        eprintln!("[memoria] HTTP {} al aprender", r.status());
        return None;
    }
    let v = r.json::<serde_json::Value>().await.ok()?;
    serde_json::from_str(extract_text(&v).trim()).ok()
}

fn apply_memory_ops(app: &AppHandle, ops: &serde_json::Value) {
    for q in ops["remove"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
        if let Ok(n) = crate::memory::forget(app, q) {
            if n > 0 {
                eprintln!("[memoria] olvidado ({n}): {q}");
            }
        }
    }
    for f in ops["add"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).take(3) {
        if let Ok(true) = crate::memory::add(app, f) {
            eprintln!("[memoria] aprendido: {f}");
        }
    }
}

fn known_facts_text(app: &AppHandle) -> String {
    let known = crate::memory::load(app);
    if known.is_empty() { "(nada todavía)".to_string() } else { known.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n") }
}

const MEMORY_RULES: &str = "Cada hecho debe ser UNA sola idea; si hay dos cosas, son DOS elementos separados. Formato: frases cortas en español. NO guardes peticiones puntuales, cultura general, el contenido de las respuestas ni datos sensibles (contraseñas, claves, tarjetas, documentos). Si algo nuevo contradice un hecho guardado, quita el viejo.";

/// After each exchange, asks the model whether the user revealed something lasting (a preference, a habit, a personal fact) or
/// CORRECTED the assistant (then it writes a general rule for next time). Runs after the answer is shown: no waiting.
async fn learn(app: &AppHandle, convo: &[ChatMessage], answer: &str) {
    let Some(user) = convo.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()) else { return };
    if user.trim().chars().count() < 3 {
        return;
    }
    // Habits: keep a short log of what the user asks for, and every few requests look for patterns in it.
    let analyse_habits = user.trim().chars().count() >= 8 && crate::memory::log_request(app, &user);

    // The last exchanges, so a correction ("no, así no…") can be understood against the answer it refers to.
    let tail: Vec<&ChatMessage> = convo.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    let transcript: String = tail
        .iter()
        .map(|m| format!("{}: {}", if m.role == "user" { "Usuario" } else { "Asistente" }, m.content.chars().take(600).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "Eres el módulo de memoria de un asistente personal. Lee el final de la conversación y decide qué APRENDER del último mensaje del usuario.\n\
1) PREFERENCIAS y DATOS duraderos que él mismo revela: formato, longitud, tono, idioma, gustos, herramientas, nombre, ciudad, trabajo, estudios, horarios, rutinas, proyectos.\n\
2) CORRECCIONES: si el usuario dice que el asistente lo hizo mal o le indica cómo quiere la respuesta (\"no, así no\", \"mejor hazlo…\", \"más corto\", \"sin explicar\", \"siempre/nunca…\", \"dame solo…\"), escribe una REGLA general y reutilizable que empiece con \"Cuando…\" o \"Al responder…\", describiendo el comportamiento correcto para la próxima vez (no el tema puntual). Ejemplo: usuario pide la hora y se queja de que explicó de más → \"Cuando pregunte la hora, responde solo la hora, sin explicaciones\".\n\
{MEMORY_RULES}\n\n\
Lo ya guardado:\n{known}\n\n\
Conversación (lo último es lo más reciente):\n{transcript}\nAsistente (respuesta nueva): {answer}\n\n\
Responde SOLO con JSON: {{\"add\": [\"...\"], \"remove\": [\"palabras clave de un hecho guardado que ya no aplica\"]}}. Listas vacías si no hay nada que aprender (lo más habitual).",
        known = known_facts_text(app),
        answer = answer.chars().take(500).collect::<String>(),
    );
    if let Some(ops) = memory_json(app, prompt).await {
        apply_memory_ops(app, &ops);
    }

    if analyse_habits {
        let recent = crate::memory::recent(app).iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n");
        let prompt = format!(
            "Eres el módulo de memoria de un asistente personal. Abajo están las últimas peticiones del usuario (la hora local va entre corchetes). Busca HÁBITOS y patrones que se repitan (temas que consulta con frecuencia, horarios en que usa el asistente, tareas que repite, cómo suele pedir las cosas) y guárdalos como hechos breves, p. ej. \"Suele consultar el clima de Lima por las mañanas\". Solo si el patrón aparece al menos 3 veces; no inventes.\n\
{MEMORY_RULES}\n\n\
Lo ya guardado:\n{known}\n\nPeticiones recientes:\n{recent}\n\n\
Responde SOLO con JSON: {{\"add\": [\"...\"], \"remove\": []}}. Listas vacías si no hay un patrón claro.",
            known = known_facts_text(app),
        );
        if let Some(ops) = memory_json(app, prompt).await {
            apply_memory_ops(app, &ops);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_part() {
        let d = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hola ñ"}]}}]}"#;
        let parts = parse_chunk(d).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hola ñ");
    }
    #[test]
    fn keeps_function_call_with_signature_and_drops_thoughts() {
        let d = r#"{"candidates":[{"content":{"role":"model","parts":[
            {"text":"pensando","thought":true},
            {"functionCall":{"name":"create_reminder","args":{"text":"x","minutes_from_now":10}},"thoughtSignature":"SIG"}]}}]}"#;
        let parts = parse_chunk(d).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionCall"]["name"], "create_reminder");
        assert_eq!(parts[0]["thoughtSignature"], "SIG");
    }
    #[test]
    fn handles_errors_blocks_and_noise() {
        assert!(parse_chunk("not json").unwrap().is_empty());
        assert!(parse_chunk(r#"{"usageMetadata":{}}"#).unwrap().is_empty());
        assert_eq!(parse_chunk(r#"{"error":{"code":429,"message":"quota"}}"#), Err("quota".into()));
        assert!(parse_chunk(r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#).unwrap_err().contains("SAFETY"));
    }
    #[test]
    fn maps_http_errors() {
        assert!(friendly_http_error(403, "{}").contains("inválida"));
        assert!(friendly_http_error(400, r#"{"error":{"message":"API key not valid. Please pass a valid API key."}}"#).contains("inválida"));
        assert!(friendly_http_error(429, "{}").contains("saturado"));
        assert_eq!(friendly_http_error(400, r#"{"error":{"message":"bad"}}"#), "bad");
    }
    #[test]
    fn fallback_chain_dedups_and_orders() {
        assert_eq!(candidate_models("gemini-2.5-pro"), ["gemini-2.5-pro", "gemini-3.1-flash-lite", "gemini-3.8-flash"]);
        assert_eq!(candidate_models("gemini-3.8-flash"), ["gemini-3.8-flash", "gemini-3.1-flash-lite"]);
        assert!(is_fallback_status(503) && is_fallback_status(429) && !is_fallback_status(400) && !is_fallback_status(403));
    }
    #[test]
    fn needs_web_detects_live_questions() {
        assert!(needs_web("cómo está el clima hoy en Lima"));
        assert!(needs_web("busca el precio del dólar"));
        assert!(!needs_web("qué es la fotosintesis"));
        assert!(!needs_web("ponme un recordatorio para hoy a las 5"));
    }
    #[test]
    fn tool_schema_is_gemini_shaped() {
        let s = tools_schema();
        let decls = s[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 9);
        assert!(decls.iter().all(|d| d["name"].is_string()));
        assert!(decls.iter().any(|d| d["name"] == "web_search"));
        assert!(decls.iter().any(|d| d["name"] == "remember"));
        assert!(decls.iter().any(|d| d["name"] == "forget_memory"));
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn parses_results_and_unwraps_redirects() {
        let html = r#"<div class="result"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fx%3D1&amp;rut=abc">Clima <b>Lima</b> &amp; hoy</a>
            <a class="result__snippet" href="x">22 &deg;C, <b>nublado</b></a></div>
            <div class="result"><a class="result__a" href="https://b.example/y">Segundo</a><a class="result__snippet" href="x">Otro texto</a></div>"#;
        let r = parse_ddg(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://example.com/a?x=1");
        assert_eq!(r[0].title, "Clima Lima & hoy");
        assert!(r[0].snippet.contains("nublado"));
        assert_eq!(r[1].url, "https://b.example/y");
        assert_eq!(r[1].snippet, "Otro texto");
    }

    #[test]
    fn parses_bing_and_unwraps_tracking_links() {
        // u=a1 + base64url("https://www.plazavea.com.pe/")
        let html = r#"<ol><li class="b_algo" data-id><h2><a target="_blank" href="https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;ver=2&amp;u=a1aHR0cHM6Ly93d3cucGxhemF2ZWEuY29tLnBlLw&amp;ntb=1" h="ID">plazaVea | &#161;Precios Bajos!</a></h2><div class="b_caption"><p class="b_lineclamp2">Compra online los mejores <strong>precios</strong> &#8230;</p></div></li>
            <li class="b_algo"><h2><a href="https://example.com/dolar">D&#243;lar hoy</a></h2><div class="b_caption"><p>S/ 3.45</p></div></li></ol>"#;
        let r = parse_bing(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://www.plazavea.com.pe/");
        assert_eq!(r[0].title, "plazaVea | ¡Precios Bajos!");
        assert!(r[0].snippet.starts_with("Compra online los mejores precios"));
        assert_eq!(r[1].url, "https://example.com/dolar");
        assert_eq!(r[1].title, "Dólar hoy");
        assert_eq!(r[1].snippet, "S/ 3.45");
    }

    #[test]
    fn extracts_transcript_text() {
        let v = serde_json::json!({"candidates": [{"content": {"parts": [{"thought": true, "text": "hmm"}, {"text": " \"Pon música\" "}]}}]});
        assert_eq!(extract_text(&v), "Pon música");
        assert_eq!(extract_text(&serde_json::json!({})), "");
    }

    #[test]
    fn finds_tts_audio() {
        let v = serde_json::json!({"steps": [{"type": "user_input"}, {"type": "model_output", "content": [{"type": "audio", "data": "QUJD"}]}]});
        assert_eq!(extract_tts_audio(&v).as_deref(), Some("QUJD"));
        assert!(extract_tts_audio(&serde_json::json!({"steps": []})).is_none());
    }

    #[test]
    fn parses_news_rss() {
        let x = "<rss><channel><title>q</title><item><title>Sube el d&#243;lar</title><description>El tipo de cambio &#8230; hoy</description><pubDate>Wed, 04 Nov 2026 15:59:00 GMT</pubDate></item></channel></rss>";
        let n = parse_news_rss(x);
        assert_eq!(n.len(), 1);
        assert!(n[0].starts_with("Sube el dólar (Wed, 04 Nov 2026)"), "{}", n[0]);
        assert!(n[0].contains("tipo de cambio"));
    }

    #[test]
    fn page_text_drops_scripts() {
        let t = html_to_text("<html><head><title>x</title></head><body><script>var a=1;</script><p>Hola   <b>mundo</b></p><style>p{}</style></body></html>");
        assert_eq!(t, "Hola mundo");
    }
}
