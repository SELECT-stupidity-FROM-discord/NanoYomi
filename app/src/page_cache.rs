//! Chapter pages kept on disk up to a size cap. Past it the least recently read pages go first.
//! Which pages there are, how big, and when each was last read is kept in the cache database, not asked of the folder.

use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static CAP_BYTES: AtomicU64 = AtomicU64::new(1024 * 1024 * 1024);

pub const CAPS_MB: [u32; 4] = [256, 512, 1024, 2048];
pub const DEFAULT_MB: u32 = 1024;

fn now() -> i64 {
    crate::store::now_ms()
}

/// Opens the cache folder. Pages already in it from before the database are indexed by when they were last read.
pub fn set_dir(dir: PathBuf) {
    let _ = crate::cache_db::with(|c| adopt(c, &dir));
    let _ = DIR.set(dir);
}

fn adopt(conn: &Connection, dir: &Path) -> rusqlite::Result<()> {
    if conn.query_row("SELECT count(*) FROM pages", [], |r| r.get::<_, i64>(0))? > 0 {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
    let mut put = conn.prepare("INSERT OR IGNORE INTO pages(id, size, used) VALUES(?1, ?2, ?3)")?;
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().and_then(|n| u64::from_str_radix(n, 16).ok()) else { continue };
        let Ok(m) = e.metadata() else { continue };
        let used = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
        put.execute(params![id as i64, m.len() as i64, used])?;
    }
    Ok(())
}

pub fn set_cap_mb(mb: u32) {
    CAP_BYTES.store(mb as u64 * 1024 * 1024, Ordering::Relaxed);
    if let Some(dir) = DIR.get() {
        crate::cache_db::with(|c| evict(c, dir, mb as u64 * 1024 * 1024));
    }
}

pub fn clear() {
    if let Some(dir) = DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
        crate::cache_db::with(|c| c.execute("DELETE FROM pages", []));
    }
}

/// Named by a hash of the page's JSON.
fn id(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

fn path(key: &str) -> Option<PathBuf> {
    DIR.get().map(|d| d.join(format!("{:016x}", id(key))))
}

/// Notes a page as read now, adding it if the database hadn't heard of it.
fn touch(conn: &Connection, id: u64, size: u64) -> rusqlite::Result<usize> {
    conn.execute("INSERT INTO pages(id, size, used) VALUES(?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET size = excluded.size, used = excluded.used", params![id as i64, size as i64, now()])
}

pub fn get(key: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path(key)?).ok()?;
    crate::cache_db::with(|c| touch(c, id(key), bytes.len() as u64));
    Some(bytes)
}

/// Moves a download into the cache and trims it to the cap. Returns the original path if it couldn't be kept.
pub fn put(key: &str, downloaded: PathBuf) -> PathBuf {
    let Some(target) = path(key) else { return downloaded };
    let dir = target.parent().unwrap();
    let moved = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::rename(&downloaded, &target).or_else(|_| std::fs::copy(&downloaded, &target).map(|_| ())));
    if moved.is_err() {
        return downloaded;
    }
    let _ = std::fs::remove_file(&downloaded);
    let size = std::fs::metadata(&target).map_or(0, |m| m.len());
    crate::cache_db::with(|c| touch(c, id(key), size).and_then(|_| evict(c, dir, CAP_BYTES.load(Ordering::Relaxed))));
    target
}

/// For entries that turned out not to be images.
pub fn remove(key: &str) {
    if let Some(p) = path(key) {
        let _ = std::fs::remove_file(p);
        crate::cache_db::with(|c| c.execute("DELETE FROM pages WHERE id = ?1", params![id(key) as i64]));
    }
}

/// Deletes the least recently read pages until the rest fit under the cap.
fn evict(conn: &Connection, dir: &Path, cap: u64) -> rusqlite::Result<()> {
    let mut total: u64 = conn.query_row("SELECT COALESCE(SUM(size), 0) FROM pages", [], |r| r.get::<_, i64>(0))? as u64;
    if total <= cap {
        return Ok(());
    }
    let mut stale = vec![];
    let mut oldest = conn.prepare("SELECT id, size FROM pages ORDER BY used")?;
    for row in oldest.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64)))? {
        let (id, size) = row?;
        if total <= cap {
            break;
        }
        total -= size;
        stale.push(id);
    }
    for id in stale {
        let _ = std::fs::remove_file(dir.join(format!("{:016x}", id as u64)));
        conn.execute("DELETE FROM pages WHERE id = ?1", params![id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(dir: &Path, conn: &Connection, id: u64, size: usize, used: i64) {
        std::fs::write(dir.join(format!("{id:016x}")), vec![0u8; size]).unwrap();
        conn.execute("INSERT INTO pages(id, size, used) VALUES(?1, ?2, ?3)", params![id as i64, size as i64, used]).unwrap();
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nano-page-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn evicts_least_recently_used() {
        let (dir, conn) = (dir("evict"), crate::cache_db::memory());
        for (id, used) in [(1, 10), (2, 20), (3, 30)] {
            page(&dir, &conn, id, 100, used);
        }
        evict(&conn, &dir, 200).unwrap();
        assert!(!dir.join(format!("{:016x}", 1)).exists() && dir.join(format!("{:016x}", 2)).exists() && dir.join(format!("{:016x}", 3)).exists());
        let ids: Vec<i64> = conn.prepare("SELECT id FROM pages ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(ids, vec![2, 3]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reading_a_page_makes_it_the_newest_even_if_it_was_never_noted() {
        let (dir, conn) = (dir("touch"), crate::cache_db::memory());
        page(&dir, &conn, 1, 100, 10);
        page(&dir, &conn, 2, 100, 20);
        touch(&conn, 1, 100).unwrap();
        touch(&conn, 3, 100).unwrap();
        std::fs::write(dir.join(format!("{:016x}", 3)), vec![0u8; 100]).unwrap();
        evict(&conn, &dir, 200).unwrap();
        assert!(!dir.join(format!("{:016x}", 2)).exists(), "the one nobody read goes first");
        assert!(dir.join(format!("{:016x}", 1)).exists() && dir.join(format!("{:016x}", 3)).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pages_cached_before_the_database_are_adopted_once() {
        let (dir, conn) = (dir("adopt"), crate::cache_db::memory());
        std::fs::write(dir.join(format!("{:016x}", 7)), [0u8; 50]).unwrap();
        std::fs::write(dir.join("not-a-page.txt"), [0u8; 5]).unwrap();
        adopt(&conn, &dir).unwrap();
        let rows: Vec<(i64, i64)> = conn.prepare("SELECT id, size FROM pages").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(rows, vec![(7, 50)]);
        std::fs::write(dir.join(format!("{:016x}", 8)), [0u8; 50]).unwrap();
        adopt(&conn, &dir).unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM pages", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "only an empty database adopts");
        let _ = std::fs::remove_dir_all(dir);
    }
}
