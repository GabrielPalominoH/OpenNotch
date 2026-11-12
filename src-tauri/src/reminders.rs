//! Reminders: SQLite storage plus a scheduler thread that fires notifications.
use chrono::{Local, NaiveDateTime, TimeZone};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub struct Db(pub Mutex<Connection>);

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Reminder {
    pub id: i64,
    pub text: String,
    pub due_at_ms: i64,
    pub fired: bool,
    pub done: bool,
}

pub fn open(app: &AppHandle) -> Result<Db, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(dir.join("opennotch.db")).map_err(|e| e.to_string())?;
    init(&conn).map_err(|e| e.to_string())?;
    Ok(Db(Mutex::new(conn)))
}

fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reminders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            due_at_ms INTEGER NOT NULL,
            fired INTEGER NOT NULL DEFAULT 0,
            done_at_ms INTEGER
        );",
    )?;
    // Databases created before the to-do style UI don't have the column; ignore "duplicate column".
    let _ = conn.execute("ALTER TABLE reminders ADD COLUMN done_at_ms INTEGER", []);
    Ok(())
}

fn insert(conn: &Connection, text: &str, due_at_ms: i64) -> rusqlite::Result<i64> {
    conn.execute("INSERT INTO reminders (text, due_at_ms) VALUES (?1, ?2)", params![text, due_at_ms])?;
    Ok(conn.last_insert_rowid())
}

/// Pending reminders, then the ones completed in the last 24h ("done today"). Each group ordered by due time.
fn list_rows(conn: &Connection, now_ms: i64) -> rusqlite::Result<Vec<Reminder>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, due_at_ms, fired, done_at_ms FROM reminders
         WHERE done_at_ms IS NULL OR done_at_ms > ?1
         ORDER BY (done_at_ms IS NOT NULL), due_at_ms ASC",
    )?;
    let rows = stmt.query_map(params![now_ms - 86_400_000], |r| {
        Ok(Reminder {
            id: r.get(0)?,
            text: r.get(1)?,
            due_at_ms: r.get(2)?,
            fired: r.get::<_, i64>(3)? != 0,
            done: r.get::<_, Option<i64>>(4)?.is_some(),
        })
    })?;
    rows.collect()
}

fn due_rows(conn: &Connection, now_ms: i64) -> rusqlite::Result<Vec<Reminder>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, due_at_ms FROM reminders WHERE fired = 0 AND done_at_ms IS NULL AND due_at_ms <= ?1",
    )?;
    let rows = stmt.query_map(params![now_ms], |r| {
        Ok(Reminder { id: r.get(0)?, text: r.get(1)?, due_at_ms: r.get(2)?, fired: false, done: false })
    })?;
    rows.collect()
}

fn set_done_row(conn: &Connection, id: i64, done: bool, now_ms: i64) -> rusqlite::Result<usize> {
    conn.execute("UPDATE reminders SET done_at_ms = ?2 WHERE id = ?1", params![id, if done { Some(now_ms) } else { None }])
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn add(app: &AppHandle, text: &str, due_at_ms: i64) -> Result<Reminder, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("El recordatorio está vacío.".into());
    }
    let db = app.state::<Db>();
    let id = insert(&db.0.lock().unwrap(), text, due_at_ms).map_err(|e| e.to_string())?;
    let _ = app.emit("reminders-changed", ());
    Ok(Reminder { id, text: text.into(), due_at_ms, fired: false, done: false })
}

pub fn list(app: &AppHandle) -> Result<Vec<Reminder>, String> {
    let db = app.state::<Db>();
    let rows = list_rows(&db.0.lock().unwrap(), now_ms()).map_err(|e| e.to_string());
    rows
}

/// "YYYY-MM-DDTHH:MM[:SS]" in the user's local time zone -> epoch ms.
pub fn parse_local(at: &str) -> Result<i64, String> {
    let naive = NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M"))
        .map_err(|_| format!("Fecha inválida: {at} (usa YYYY-MM-DDTHH:MM)"))?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|d| d.timestamp_millis())
        .ok_or_else(|| "Hora local inexistente.".to_string())
}

#[tauri::command]
pub fn reminder_add(app: AppHandle, text: String, due_at_ms: i64) -> Result<Reminder, String> {
    add(&app, &text, due_at_ms)
}

#[tauri::command]
pub fn reminder_list(app: AppHandle) -> Result<Vec<Reminder>, String> {
    list(&app)
}

#[tauri::command]
pub fn reminder_set_done(app: AppHandle, id: i64, done: bool) -> Result<(), String> {
    let db = app.state::<Db>();
    set_done_row(&db.0.lock().unwrap(), id, done, now_ms()).map_err(|e| e.to_string())?;
    let _ = app.emit("reminders-changed", ());
    Ok(())
}

#[tauri::command]
pub fn reminder_delete(app: AppHandle, id: i64) -> Result<(), String> {
    let db = app.state::<Db>();
    db.0.lock().unwrap().execute("DELETE FROM reminders WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    let _ = app.emit("reminders-changed", ());
    Ok(())
}

pub fn spawn_scheduler(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(1000));
        let due = {
            let db = app.state::<Db>();
            let conn = db.0.lock().unwrap();
            let due = due_rows(&conn, now_ms()).unwrap_or_default();
            for r in &due {
                let _ = conn.execute("UPDATE reminders SET fired = 1 WHERE id = ?1", params![r.id]);
            }
            due
        };
        for r in due {
            let _ = app.emit("alert", serde_json::json!({ "kind": "reminder", "id": r.id, "title": "Recordatorio", "body": r.text }));
            let _ = app.emit("reminders-changed", ());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }

    #[test]
    fn due_only_returns_unfired_past_items() {
        let c = mem();
        insert(&c, "past", 100).unwrap();
        insert(&c, "future", 10_000).unwrap();
        let due = due_rows(&c, 500).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "past");
        c.execute("UPDATE reminders SET fired = 1 WHERE id = ?1", params![due[0].id]).unwrap();
        assert!(due_rows(&c, 500).unwrap().is_empty());
    }

    #[test]
    fn list_orders_by_due_time() {
        let c = mem();
        insert(&c, "b", 2_000).unwrap();
        insert(&c, "a", 1_000).unwrap();
        let texts: Vec<_> = list_rows(&c, 0).unwrap().into_iter().map(|r| r.text).collect();
        assert_eq!(texts, ["a", "b"]);
    }

    #[test]
    fn done_items_move_to_the_end_and_expire_after_a_day() {
        let c = mem();
        let a = insert(&c, "a", 1_000).unwrap();
        insert(&c, "b", 2_000).unwrap();
        set_done_row(&c, a, true, 10_000).unwrap();
        let rows = list_rows(&c, 20_000).unwrap();
        assert_eq!(rows.iter().map(|r| (r.text.as_str(), r.done)).collect::<Vec<_>>(), [("b", false), ("a", true)]);
        // 25h later the completed one is gone, and a done item never fires.
        assert_eq!(list_rows(&c, 10_000 + 25 * 3_600_000).unwrap().len(), 1);
        assert!(due_rows(&c, 5_000).unwrap().iter().all(|r| r.text != "a"));
        set_done_row(&c, a, false, 0).unwrap();
        assert!(list_rows(&c, 20_000).unwrap().iter().all(|r| !r.done));
    }

    #[test]
    fn parses_local_datetime() {
        assert!(parse_local("2026-11-03T09:00").is_ok());
        assert!(parse_local("2026-11-03T09:00:30").is_ok());
        assert!(parse_local("mañana").is_err());
    }
}
