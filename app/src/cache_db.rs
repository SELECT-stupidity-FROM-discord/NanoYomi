//! A second database next to the library for what can be rebuilt: which pages are cached, and each repo's extension list.
//! Losing it costs a refetch and nothing else, so it isn't backed up.

use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

static CONN: Mutex<Option<Connection>> = Mutex::new(None);

/// Bump this when a table changes. A cache from another layout is dropped and made again, since everything in it can be rebuilt.
const VERSION: i64 = 1;

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
    // 0 is a file made before layouts were numbered, which is layout 1.
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 0 && version != VERSION {
        let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for t in tables {
            conn.execute_batch(&format!("DROP TABLE \"{t}\""))?;
        }
    }
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", VERSION)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_from_another_layout_is_made_again_instead_of_failing_quietly() {
        let path = std::env::temp_dir().join(format!("nano-cache-layout-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch("CREATE TABLE pages (id INTEGER PRIMARY KEY); INSERT INTO pages VALUES (1)").unwrap();
            old.pragma_update(None, "user_version", 99).unwrap();
        }
        let conn = connect(&path).unwrap();
        conn.execute("INSERT INTO pages(id, size, used) VALUES(1, 2, 3)", []).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM pages", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "the old rows went with the old table");
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, VERSION);
        drop(conn);
        assert!(connect(&path).is_ok(), "a file already at this layout opens as it is");
    }
}
