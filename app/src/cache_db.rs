//! A second database next to the library for what can be rebuilt: which pages are cached, and each repo's extension list.
//! Losing it costs a refetch and nothing else, so it isn't backed up.

use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

static CONN: Mutex<Option<Connection>> = Mutex::new(None);

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pages (id INTEGER PRIMARY KEY, size INTEGER NOT NULL, used INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS pages_used ON pages(used);
CREATE TABLE IF NOT EXISTS repo_exts (
    repo TEXT NOT NULL, pos INTEGER NOT NULL, pkg TEXT NOT NULL, name TEXT NOT NULL, version TEXT NOT NULL, langs TEXT NOT NULL, nsfw INTEGER NOT NULL,
    mixed INTEGER NOT NULL, source_ids TEXT NOT NULL, sites TEXT NOT NULL, site TEXT, icon_url TEXT NOT NULL, jar_url TEXT NOT NULL, key TEXT NOT NULL,
    PRIMARY KEY (repo, pos)) WITHOUT ROWID;
";

fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Opens the cache database, starting over if the file is damaged, since everything in it can be rebuilt.
pub fn open(path: &Path) {
    let conn = connect(path).or_else(|_| {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        connect(path)
    });
    *CONN.lock().unwrap() = conn.ok();
}

/// Runs `f` on the cache database, or answers None when it couldn't be opened.
pub fn with<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    f(CONN.lock().unwrap().as_ref()?).ok()
}

/// An empty cache database in memory, for tests.
#[cfg(test)]
pub fn memory() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn
}
