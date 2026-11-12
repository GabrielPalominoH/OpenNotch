//! Long-term memory: short facts about the user (preferences, habits, corrections) that persist across chats and are fed back
//! into every conversation. Stored as plain JSON in the app config dir; the user can read it with `/memoria` and wipe it.
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const MAX_FACTS: usize = 60;
const MAX_FACT_CHARS: usize = 200;

#[derive(Serialize, Deserialize, Default)]
struct Store {
    facts: Vec<String>,
    /// Recent user requests with their local time, to spot habits.
    #[serde(default)]
    recent: Vec<String>,
    /// Requests logged since habits were last analysed.
    #[serde(default)]
    since_habits: u32,
}

const MAX_RECENT: usize = 40;
const HABIT_EVERY: u32 = 8;

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("memory.json"))
}

fn load_store(app: &AppHandle) -> Store {
    path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Store>(&s).ok())
        .unwrap_or_default()
}

pub fn load(app: &AppHandle) -> Vec<String> {
    load_store(app).facts
}

fn save(app: &AppHandle, facts: &[String]) -> Result<(), String> {
    let mut st = load_store(app);
    st.facts = facts.to_vec();
    save_store(app, &st)
}

/// Logs a user request; returns true every `HABIT_EVERY` requests, when it is time to look for habits.
pub fn log_request(app: &AppHandle, text: &str) -> bool {
    let mut st = load_store(app);
    let stamp = chrono::Local::now().format("%a %H:%M");
    st.recent.push(format!("[{stamp}] {}", text.trim().chars().take(160).collect::<String>()));
    if st.recent.len() > MAX_RECENT {
        let extra = st.recent.len() - MAX_RECENT;
        st.recent.drain(..extra);
    }
    st.since_habits += 1;
    let due = st.since_habits >= HABIT_EVERY;
    if due {
        st.since_habits = 0;
    }
    let _ = save_store(app, &st);
    due
}

pub fn recent(app: &AppHandle) -> Vec<String> {
    load_store(app).recent
}

fn save_store(app: &AppHandle, store: &Store) -> Result<(), String> {
    let p = path(app).ok_or("no config dir")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    // atomic: write a temp file, then rename over the real one
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Adds a fact (skipping duplicates); the oldest ones are dropped past the cap. Returns whether anything changed.
pub fn add(app: &AppHandle, fact: &str) -> Result<bool, String> {
    let fact: String = fact.trim().chars().take(MAX_FACT_CHARS).collect();
    if fact.chars().count() < 4 {
        return Ok(false);
    }
    let mut facts = load(app);
    let n = norm(&fact);
    if facts.iter().any(|f| norm(f) == n) {
        return Ok(false);
    }
    facts.push(fact);
    if facts.len() > MAX_FACTS {
        let extra = facts.len() - MAX_FACTS;
        facts.drain(..extra);
    }
    save(app, &facts)?;
    Ok(true)
}

/// Removes every fact that contains `query` (case/punctuation-insensitive). Returns how many went away.
pub fn forget(app: &AppHandle, query: &str) -> Result<usize, String> {
    let q = norm(query);
    if q.is_empty() {
        return Ok(0);
    }
    let facts = load(app);
    let kept: Vec<String> = facts.iter().filter(|f| !norm(f).contains(&q)).cloned().collect();
    let removed = facts.len() - kept.len();
    if removed > 0 {
        save(app, &kept)?;
    }
    Ok(removed)
}

/// The block that goes into the system prompt (empty when nothing is known yet).
pub fn prompt_block(app: &AppHandle) -> String {
    let facts = load(app);
    if facts.is_empty() {
        return String::new();
    }
    let list = facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
    format!(
        " What you have learned about the user from earlier chats (these are binding preferences and rules (including corrections the user gave you before): obey them in every answer (length, tone, format) and personalise naturally; never recite them or mention that you remember unless asked):\n{list}\n"
    )
}

#[tauri::command]
pub fn memory_list(app: AppHandle) -> Vec<String> {
    load(&app)
}

#[tauri::command]
pub fn memory_clear(app: AppHandle) -> Result<(), String> {
    save_store(&app, &Store::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_ignores_case_and_punctuation() {
        assert_eq!(norm("  Le gusta el  CAFÉ, sin azúcar. "), norm("le gusta el café sin azúcar"));
    }
}
