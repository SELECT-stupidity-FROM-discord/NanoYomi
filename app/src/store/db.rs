//! The library on disk as SQLite, in typed tables. Only what extensions hand us stays opaque, as text: a series' manga details and chapter list.
//!
//! A field added to `Data`, `Series` or anything inside them has to be added here as well, or it is lost on restart.
//! The round trip test builds every struct without `..Default::default()`, so a new field stops it compiling until it is filled in, and then it fails until it is saved.

use super::{Category, Data, Progress, Series, Track};
use crate::downloads::Format;
use crate::extensions::Repo;
use crate::sync::{Meta, Snapshot};
use crate::track::Login;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, Transaction, params};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Res<T> = Result<T, Box<dyn std::error::Error>>;
type Chapters = Arc<Vec<Value>>;

const VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value);
CREATE TABLE IF NOT EXISTS setting_items (key TEXT NOT NULL, sub TEXT NOT NULL, pos INTEGER NOT NULL, value, PRIMARY KEY (key, sub, pos)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS fonts (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS repos (pos INTEGER PRIMARY KEY, url TEXT NOT NULL, name TEXT NOT NULL, key TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS categories (pos INTEGER PRIMARY KEY, name TEXT NOT NULL, sort INTEGER NOT NULL, flags INTEGER NOT NULL, uid INTEGER NOT NULL, version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS trackers (tracker INTEGER PRIMARY KEY, access TEXT NOT NULL, refresh TEXT NOT NULL, expires INTEGER NOT NULL, user TEXT NOT NULL, user_id INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS series (
    key TEXT PRIMARY KEY, pos INTEGER NOT NULL, source TEXT NOT NULL, source_name TEXT NOT NULL, manga TEXT NOT NULL,
    in_library INTEGER NOT NULL, added_at INTEGER NOT NULL, scanlators_chosen INTEGER NOT NULL, independent_chapters INTEGER NOT NULL,
    history_cleared_at INTEGER NOT NULL, read_url TEXT, read_name TEXT, read_page INTEGER, read_pages INTEGER, read_at INTEGER);
CREATE INDEX IF NOT EXISTS series_pos ON series(pos);
CREATE TABLE IF NOT EXISTS chapters (key TEXT PRIMARY KEY REFERENCES series(key) ON DELETE CASCADE, list TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS hidden_scanlators (key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, pos INTEGER NOT NULL, name TEXT NOT NULL, PRIMARY KEY (key, pos)) WITHOUT ROWID;
-- kind 0 is the series itself, 1 and 2 what the two sync files last saw, so the tables below hold all three.
CREATE TABLE IF NOT EXISTS sent (key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, kind INTEGER NOT NULL, in_library INTEGER NOT NULL, read_url TEXT, read_page INTEGER, read_at INTEGER, PRIMARY KEY (key, kind)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS marks (key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, kind INTEGER NOT NULL, chapter TEXT NOT NULL, read INTEGER NOT NULL, PRIMARY KEY (key, kind, chapter)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS bookmarks (key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, kind INTEGER NOT NULL, chapter TEXT NOT NULL, PRIMARY KEY (key, kind, chapter)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS series_categories (key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, kind INTEGER NOT NULL, pos INTEGER NOT NULL, category INTEGER NOT NULL, PRIMARY KEY (key, kind, pos)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS tracks (
    key TEXT NOT NULL REFERENCES series(key) ON DELETE CASCADE, kind INTEGER NOT NULL, pos INTEGER NOT NULL, tracker INTEGER NOT NULL, library_id INTEGER NOT NULL,
    media_id INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, last_read REAL NOT NULL, total INTEGER NOT NULL, score REAL NOT NULL, status INTEGER NOT NULL,
    started INTEGER NOT NULL, finished INTEGER NOT NULL, private INTEGER NOT NULL, PRIMARY KEY (key, kind, pos)) WITHOUT ROWID;
";

/// What a series row looks like on disk, so a save can skip what hasn't changed.
struct Saved {
    pos: usize,
    body: u64,
    chapters: Chapters,
}

pub struct Db {
    path: PathBuf,
    conn: Option<Connection>,
    series: HashMap<String, Saved>,
    groups: HashMap<&'static str, u64>,
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// A row key per series, counting repeats so two with the same source and url, or none, both survive.
fn keys(series: &[Series]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    series
        .iter()
        .map(|s| {
            let base = format!("{}\u{1}{}", s.source, s.url());
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 { base } else { format!("{base}\u{1}{n}") }
        })
        .collect()
}

/// Copies the library to `to` as a consistent snapshot, replacing the last one only once the new one checks out.
pub fn backup(from: &Path, to: &Path) -> Res<()> {
    let tmp = to.with_extension("bak.new");
    let _ = std::fs::remove_file(&tmp);
    Connection::open(from)?.execute("VACUUM INTO ?1", params![tmp.to_string_lossy()])?;
    let verdict: String = Connection::open(&tmp)?.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if verdict != "ok" {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the copy failed its check: {verdict}").into());
    }
    let _ = std::fs::remove_file(to);
    std::fs::rename(&tmp, to)?;
    Ok(())
}

fn flag(v: bool) -> Sql {
    Sql::Integer(v as i64)
}

fn int(v: i64) -> Sql {
    Sql::Integer(v)
}

fn text(v: &str) -> Sql {
    Sql::Text(v.into())
}

/// What `Data` keeps outside the series, as rows. The lists and maps are rows of `setting_items`, a map's empty entries marked by position -1.
#[derive(Debug, Default)]
struct Settings {
    scalars: Vec<(&'static str, Sql)>,
    items: Vec<(&'static str, String, i64, Sql)>,
    fonts: Vec<(String, String)>,
}

fn format_name(f: Format) -> &'static str {
    match f {
        Format::Pages => "pages",
        Format::Cbz => "cbz",
    }
}

fn settings(d: &Data) -> Settings {
    let (p, n, s) = (&d.prefs, &d.prefs.nsfw, &d.sync);
    let opt = |v: Option<u32>| v.map_or(Sql::Null, |v| int(v as i64));
    let mut out = Settings {
        scalars: vec![
            ("host_heap_mb", opt(p.host_heap_mb)),
            ("vertical_reader", flag(p.vertical_reader)),
            ("right_to_left", flag(p.right_to_left)),
            ("sidebar_collapsed", flag(p.sidebar_collapsed)),
            ("auto_download", flag(p.auto_download)),
            ("reader_fullscreen", flag(p.reader_fullscreen)),
            ("chapters_oldest_first", flag(p.chapters_oldest_first)),
            ("updates_off", flag(p.updates_off)),
            ("last_update_check", int(p.last_update_check)),
            ("dismissed_update", text(&p.dismissed_update)),
            ("download_dir", text(&p.download_dir)),
            ("download_format", text(format_name(p.download_format))),
            ("nsfw.all", flag(n.all)),
            ("nsfw.mixed", flag(n.mixed)),
            ("nsfw.library", flag(n.library)),
            ("nsfw.updates", flag(n.updates)),
            ("nsfw.history", flag(n.history)),
            ("nsfw.downloaded", flag(n.downloaded)),
            ("nsfw.sources", flag(n.sources)),
            ("nsfw.extensions", flag(n.extensions)),
            ("zoom", d.zoom.map_or(Sql::Null, |z| Sql::Real(z as f64))),
            ("solver_url", text(&d.solver_url)),
            ("page_cache_mb", opt(d.page_cache_mb)),
            ("categories_changed", flag(d.categories_changed)),
            ("sync.host", text(&s.host)),
            ("sync.key", text(&s.key)),
            ("sync.device_id", text(&s.device_id)),
            ("sync.cursor", int(s.cursor)),
            ("sync.v1_etag", text(&s.v1_etag)),
        ],
        ..Default::default()
    };
    let mut list = |key: &'static str, sub: &str, values: Vec<Sql>| {
        if values.is_empty() {
            out.items.push((key, sub.to_string(), -1, Sql::Null));
        }
        out.items.extend(values.into_iter().enumerate().map(|(i, v)| (key, sub.to_string(), i as i64, v)));
    };
    let strings = |v: &[String]| v.iter().map(|s| text(s)).collect::<Vec<_>>();
    // A plain list that is empty needs no marker, only the entries of a map do.
    for (key, values) in [("source_order", strings(&p.source_order)), ("nsfw_sources", strings(&p.nsfw_sources)), ("mixed_sources", strings(&p.mixed_sources)), ("deleted_categories", strings(&d.deleted_categories))] {
        if !values.is_empty() {
            list(key, "", values);
        }
    }
    if !p.nsfw_categories.is_empty() {
        list("nsfw_categories", "", p.nsfw_categories.iter().map(|c| int(*c)).collect());
    }
    for (key, map) in [("languages", &d.languages), ("sites", &d.sites)] {
        for (name, values) in map {
            list(key, name, strings(values));
        }
    }
    out.fonts = p.fonts.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    out.items.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
    out.fonts.sort();
    out
}

/// Reads settings back over the defaults, so a key this version doesn't know about yet keeps its default.
fn apply_settings(d: &mut Data, scalars: HashMap<String, Sql>, items: Vec<(String, String, Sql)>, fonts: Vec<(String, String)>) {
    let on = |k: &str, d: bool| match scalars.get(k) {
        Some(Sql::Integer(v)) => *v != 0,
        _ => d,
    };
    let num = |k: &str, d: i64| match scalars.get(k) {
        Some(Sql::Integer(v)) => *v,
        _ => d,
    };
    let opt = |k: &str, d: Option<u32>| match scalars.get(k) {
        Some(Sql::Integer(v)) => Some(*v as u32),
        Some(Sql::Null) => None,
        _ => d,
    };
    let txt = |k: &str, d: &str| match scalars.get(k) {
        Some(Sql::Text(v)) => v.clone(),
        _ => d.to_string(),
    };
    let p = &mut d.prefs;
    p.host_heap_mb = opt("host_heap_mb", p.host_heap_mb);
    p.vertical_reader = on("vertical_reader", p.vertical_reader);
    p.right_to_left = on("right_to_left", p.right_to_left);
    p.sidebar_collapsed = on("sidebar_collapsed", p.sidebar_collapsed);
    p.auto_download = on("auto_download", p.auto_download);
    p.reader_fullscreen = on("reader_fullscreen", p.reader_fullscreen);
    p.chapters_oldest_first = on("chapters_oldest_first", p.chapters_oldest_first);
    p.updates_off = on("updates_off", p.updates_off);
    p.last_update_check = num("last_update_check", p.last_update_check);
    p.dismissed_update = txt("dismissed_update", &p.dismissed_update);
    p.download_dir = txt("download_dir", &p.download_dir);
    p.download_format = match scalars.get("download_format") {
        Some(Sql::Text(v)) if v == "cbz" => Format::Cbz,
        Some(Sql::Text(_)) => Format::Pages,
        _ => p.download_format,
    };
    let n = &mut p.nsfw;
    n.all = on("nsfw.all", n.all);
    n.mixed = on("nsfw.mixed", n.mixed);
    n.library = on("nsfw.library", n.library);
    n.updates = on("nsfw.updates", n.updates);
    n.history = on("nsfw.history", n.history);
    n.downloaded = on("nsfw.downloaded", n.downloaded);
    n.sources = on("nsfw.sources", n.sources);
    n.extensions = on("nsfw.extensions", n.extensions);
    d.zoom = match scalars.get("zoom") {
        Some(Sql::Real(z)) => Some(*z as f32),
        Some(Sql::Null) => None,
        _ => d.zoom,
    };
    d.solver_url = txt("solver_url", &d.solver_url);
    d.page_cache_mb = opt("page_cache_mb", d.page_cache_mb);
    d.categories_changed = on("categories_changed", d.categories_changed);
    d.sync.host = txt("sync.host", &d.sync.host);
    d.sync.key = txt("sync.key", &d.sync.key);
    d.sync.device_id = txt("sync.device_id", &d.sync.device_id);
    d.sync.cursor = num("sync.cursor", d.sync.cursor);
    d.sync.v1_etag = txt("sync.v1_etag", &d.sync.v1_etag);
    d.prefs.fonts = fonts.into_iter().collect();
    for (key, sub, value) in items {
        let word = || if let Sql::Text(t) = &value { Some(t.clone()) } else { None };
        match key.as_str() {
            "source_order" => d.prefs.source_order.extend(word()),
            "nsfw_sources" => d.prefs.nsfw_sources.extend(word()),
            "mixed_sources" => d.prefs.mixed_sources.extend(word()),
            "deleted_categories" => d.deleted_categories.extend(word()),
            "nsfw_categories" => {
                if let Sql::Integer(c) = value {
                    d.prefs.nsfw_categories.push(c);
                }
            }
            "languages" | "sites" => {
                let map = if key == "languages" { &mut d.languages } else { &mut d.sites };
                let entry = map.entry(sub).or_default();
                entry.extend(word());
            }
            _ => {}
        }
    }
}

/// The marks, bookmarks, categories and tracks of the series itself or of one of its sync snapshots.
struct View<'a> {
    kind: i64,
    marks: &'a BTreeMap<String, bool>,
    bookmarks: &'a BTreeSet<String>,
    categories: &'a [i64],
    tracks: &'a [Track],
    sent: Option<&'a Snapshot>,
}

fn views(s: &Series) -> Vec<View<'_>> {
    let mut v = vec![View { kind: 0, marks: &s.marks, bookmarks: &s.bookmarks, categories: &s.categories, tracks: &s.tracks, sent: None }];
    for (kind, meta) in [(1, &s.sync), (2, &s.sync1)] {
        if let Some(m) = meta {
            let x = &m.sent;
            v.push(View { kind, marks: &x.marks, bookmarks: &x.bookmarks, categories: &x.categories, tracks: &x.tracks, sent: Some(x) });
        }
    }
    v
}

struct Part<'a> {
    marks: &'a mut BTreeMap<String, bool>,
    bookmarks: &'a mut BTreeSet<String>,
    categories: &'a mut Vec<i64>,
    tracks: &'a mut Vec<Track>,
}

fn part(s: &mut Series, kind: i64) -> Option<Part<'_>> {
    match kind {
        0 => Some(Part { marks: &mut s.marks, bookmarks: &mut s.bookmarks, categories: &mut s.categories, tracks: &mut s.tracks }),
        1 | 2 => {
            let m = if kind == 1 { s.sync.as_mut() } else { s.sync1.as_mut() }?;
            Some(Part { marks: &mut m.sent.marks, bookmarks: &mut m.sent.bookmarks, categories: &mut m.sent.categories, tracks: &mut m.sent.tracks })
        }
        _ => None,
    }
}

/// The series as one text, minus its chapters, which only exists to tell whether anything changed.
fn body(s: &mut Series) -> Res<String> {
    let chapters = std::mem::take(&mut s.chapters);
    let body = serde_json::to_string(&*s);
    s.chapters = chapters;
    Ok(body?)
}

fn write_series(tx: &Transaction, key: &str, pos: usize, s: &Series) -> Res<()> {
    let read = s.progress.as_ref();
    tx.prepare_cached(
        "INSERT INTO series(key, pos, source, source_name, manga, in_library, added_at, scanlators_chosen, independent_chapters, history_cleared_at, read_url, read_name, read_page, read_pages, read_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(key) DO UPDATE SET pos = excluded.pos, source = excluded.source, source_name = excluded.source_name, manga = excluded.manga, in_library = excluded.in_library,
            added_at = excluded.added_at, scanlators_chosen = excluded.scanlators_chosen, independent_chapters = excluded.independent_chapters, history_cleared_at = excluded.history_cleared_at,
            read_url = excluded.read_url, read_name = excluded.read_name, read_page = excluded.read_page, read_pages = excluded.read_pages, read_at = excluded.read_at",
    )?
    .execute(params![
        key,
        pos as i64,
        s.source,
        s.source_name,
        s.manga.to_string(),
        s.in_library,
        s.added_at,
        s.scanlators_chosen,
        s.independent_chapters,
        s.history_cleared_at,
        read.map(|p| p.chapter_url.as_str()),
        read.map(|p| p.chapter_name.as_str()),
        read.map(|p| p.page as i64),
        read.map(|p| p.pages as i64),
        read.map(|p| p.at),
    ])?;
    for table in ["hidden_scanlators", "sent", "marks", "bookmarks", "series_categories", "tracks"] {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE key = ?1"))?.execute([key])?;
    }
    let mut hidden = tx.prepare_cached("INSERT INTO hidden_scanlators(key, pos, name) VALUES(?1, ?2, ?3)")?;
    for (i, name) in s.hidden_scanlators.iter().enumerate() {
        hidden.execute(params![key, i as i64, name])?;
    }
    let mut sent = tx.prepare_cached("INSERT INTO sent(key, kind, in_library, read_url, read_page, read_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
    let mut mark = tx.prepare_cached("INSERT INTO marks(key, kind, chapter, read) VALUES(?1, ?2, ?3, ?4)")?;
    let mut bookmark = tx.prepare_cached("INSERT INTO bookmarks(key, kind, chapter) VALUES(?1, ?2, ?3)")?;
    let mut category = tx.prepare_cached("INSERT INTO series_categories(key, kind, pos, category) VALUES(?1, ?2, ?3, ?4)")?;
    let mut track = tx.prepare_cached(
        "INSERT INTO tracks(key, kind, pos, tracker, library_id, media_id, url, title, last_read, total, score, status, started, finished, private)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    for v in views(s) {
        if let Some(x) = v.sent {
            let p = x.progress.as_ref();
            sent.execute(params![key, v.kind, x.in_library, p.map(|p| p.0.as_str()), p.map(|p| p.1 as i64), p.map(|p| p.2)])?;
        }
        for (chapter, read) in v.marks {
            mark.execute(params![key, v.kind, chapter, read])?;
        }
        for chapter in v.bookmarks {
            bookmark.execute(params![key, v.kind, chapter])?;
        }
        for (i, c) in v.categories.iter().enumerate() {
            category.execute(params![key, v.kind, i as i64, c])?;
        }
        for (i, t) in v.tracks.iter().enumerate() {
            track.execute(params![key, v.kind, i as i64, t.tracker, t.library_id, t.media_id, t.url, t.title, t.last_read as f64, t.total, t.score as f64, t.status, t.started, t.finished, t.private])?;
        }
    }
    Ok(())
}

/// Replaces the rows of the small tables, which are cheap enough to rewrite whole when any of them changes.
fn write_group(tx: &Transaction, group: &str, d: &Data) -> Res<()> {
    match group {
        "settings" => {
            let s = settings(d);
            for t in ["settings", "setting_items", "fonts"] {
                tx.execute(&format!("DELETE FROM {t}"), [])?;
            }
            let mut put = tx.prepare_cached("INSERT INTO settings(key, value) VALUES(?1, ?2)")?;
            for (k, v) in &s.scalars {
                put.execute(params![k, v])?;
            }
            let mut put = tx.prepare_cached("INSERT INTO setting_items(key, sub, pos, value) VALUES(?1, ?2, ?3, ?4)")?;
            for (k, sub, pos, v) in &s.items {
                put.execute(params![k, sub, pos, v])?;
            }
            let mut put = tx.prepare_cached("INSERT INTO fonts(key, value) VALUES(?1, ?2)")?;
            for (k, v) in &s.fonts {
                put.execute(params![k, v])?;
            }
        }
        "repos" => {
            tx.execute("DELETE FROM repos", [])?;
            let mut put = tx.prepare_cached("INSERT INTO repos(pos, url, name, key) VALUES(?1, ?2, ?3, ?4)")?;
            for (i, r) in d.repos.iter().enumerate() {
                put.execute(params![i as i64, r.url, r.name, r.key])?;
            }
        }
        "categories" => {
            tx.execute("DELETE FROM categories", [])?;
            let mut put = tx.prepare_cached("INSERT INTO categories(pos, name, sort, flags, uid, version) VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
            for (i, c) in d.categories.iter().enumerate() {
                put.execute(params![i as i64, c.name, c.order, c.flags, c.uid, c.version])?;
            }
        }
        _ => {
            tx.execute("DELETE FROM trackers", [])?;
            let mut put = tx.prepare_cached("INSERT INTO trackers(tracker, access, refresh, expires, user, user_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
            for (id, l) in &d.trackers {
                put.execute(params![id, l.access, l.refresh, l.expires, l.user, l.user_id])?;
            }
        }
    }
    Ok(())
}

/// What each group would write, hashed, so a save can tell which groups changed.
fn group_hashes(d: &Data) -> [(&'static str, u64); 4] {
    [
        ("settings", hash(&format!("{:?}", settings(d)))),
        ("repos", hash(&format!("{:?}", d.repos))),
        ("categories", hash(&format!("{:?}", d.categories))),
        ("trackers", hash(&format!("{:?}", d.trackers))),
    ]
}

impl Db {
    pub fn new(path: PathBuf) -> Self {
        Db { path, conn: None, series: HashMap::new(), groups: HashMap::new() }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    fn open(&mut self) -> Res<&mut Connection> {
        if self.conn.is_none() {
            let conn = Connection::open(&self.path)?;
            let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
            if version != VERSION && tables > 0 {
                return Err(format!("this library is laid out for another version of Nano (layout {version}, expected {VERSION})").into());
            }
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", VERSION)?;
            self.conn = Some(conn);
        }
        Ok(self.conn.as_mut().unwrap())
    }

    /// Reads everything back, or None when there is no database yet.
    pub fn load(&mut self) -> Res<Option<Data>> {
        if !self.exists() {
            return Ok(None);
        }
        let conn = self.open()?;
        let mut data = Data::default();
        let mut hashes = HashMap::new();
        {
            let scalars: HashMap<String, Sql> = conn.prepare("SELECT key, value FROM settings")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let items: Vec<(String, String, Sql)> = conn.prepare("SELECT key, sub, value FROM setting_items ORDER BY key, sub, pos")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            let fonts: Vec<(String, String)> = conn.prepare("SELECT key, value FROM fonts")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            apply_settings(&mut data, scalars, items, fonts);
            data.repos = conn.prepare("SELECT url, name, key FROM repos ORDER BY pos")?.query_map([], |r| Ok(Repo { url: r.get(0)?, name: r.get(1)?, key: r.get(2)? }))?.collect::<Result<_, _>>()?;
            data.categories = conn
                .prepare("SELECT name, sort, flags, uid, version FROM categories ORDER BY pos")?
                .query_map([], |r| Ok(Category { name: r.get(0)?, order: r.get(1)?, flags: r.get(2)?, uid: r.get(3)?, version: r.get(4)? }))?
                .collect::<Result<_, _>>()?;
            for row in conn.prepare("SELECT tracker, access, refresh, expires, user, user_id FROM trackers")?.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, Login { access: r.get(1)?, refresh: r.get(2)?, expires: r.get(3)?, user: r.get(4)?, user_id: r.get(5)? }))
            })? {
                let (id, login) = row?;
                data.trackers.insert(id, login);
            }
            for (group, h) in group_hashes(&data) {
                hashes.insert(group, h);
            }
        }
        let mut keys: Vec<String> = vec![];
        {
            let mut stmt = conn.prepare(
                "SELECT s.key, s.source, s.source_name, s.manga, s.in_library, s.added_at, s.scanlators_chosen, s.independent_chapters, s.history_cleared_at,
                        s.read_url, s.read_name, s.read_page, s.read_pages, s.read_at, c.list
                 FROM series s LEFT JOIN chapters c ON c.key = s.key ORDER BY s.pos",
            )?;
            let rows = stmt.query_map([], |r| {
                let mut s = Series::new(r.get(1)?, r.get(2)?, Value::Null);
                s.in_library = r.get(4)?;
                s.added_at = r.get(5)?;
                s.scanlators_chosen = r.get(6)?;
                s.independent_chapters = r.get(7)?;
                s.history_cleared_at = r.get(8)?;
                if let Some(chapter_url) = r.get::<_, Option<String>>(9)? {
                    s.progress = Some(Progress { chapter_url, chapter_name: r.get(10)?, page: r.get::<_, i64>(11)? as usize, pages: r.get::<_, i64>(12)? as usize, at: r.get(13)? });
                }
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(14)?, s))
            })?;
            for row in rows {
                let (key, manga, list, mut s) = row?;
                s.manga = serde_json::from_str(&manga)?;
                if let Some(list) = list {
                    s.chapters = Arc::new(serde_json::from_str(&list)?);
                }
                keys.push(key);
                data.series.push(s);
            }
        }
        let index: HashMap<&str, usize> = keys.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        for row in conn.prepare("SELECT key, pos, name FROM hidden_scanlators ORDER BY key, pos")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(2)?)))? {
            let (key, name) = row?;
            if let Some(&i) = index.get(key.as_str()) {
                data.series[i].hidden_scanlators.push(name);
            }
        }
        for row in conn.prepare("SELECT key, kind, in_library, read_url, read_page, read_at FROM sent")?.query_map([], |r| {
            let progress = match r.get::<_, Option<String>>(3)? {
                Some(url) => Some((url, r.get::<_, i64>(4)? as usize, r.get::<_, i64>(5)?)),
                None => None,
            };
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, Snapshot { in_library: r.get(2)?, progress, ..Default::default() }))
        })? {
            let (key, kind, snapshot) = row?;
            if let Some(&i) = index.get(key.as_str()) {
                let s = &mut data.series[i];
                match kind {
                    1 => s.sync = Some(Meta { sent: snapshot }),
                    2 => s.sync1 = Some(Meta { sent: snapshot }),
                    _ => {}
                }
            }
        }
        for row in conn.prepare("SELECT key, kind, chapter, read FROM marks")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?)))? {
            let (key, kind, chapter, read) = row?;
            if let Some(p) = index.get(key.as_str()).and_then(|&i| part(&mut data.series[i], kind)) {
                p.marks.insert(chapter, read);
            }
        }
        for row in conn.prepare("SELECT key, kind, chapter FROM bookmarks")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))? {
            let (key, kind, chapter) = row?;
            if let Some(p) = index.get(key.as_str()).and_then(|&i| part(&mut data.series[i], kind)) {
                p.bookmarks.insert(chapter);
            }
        }
        for row in conn.prepare("SELECT key, kind, category FROM series_categories ORDER BY key, kind, pos")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
            let (key, kind, category) = row?;
            if let Some(p) = index.get(key.as_str()).and_then(|&i| part(&mut data.series[i], kind)) {
                p.categories.push(category);
            }
        }
        for row in conn
            .prepare("SELECT key, kind, tracker, library_id, media_id, url, title, last_read, total, score, status, started, finished, private FROM tracks ORDER BY key, kind, pos")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    Track {
                        tracker: r.get(2)?,
                        library_id: r.get(3)?,
                        media_id: r.get(4)?,
                        url: r.get(5)?,
                        title: r.get(6)?,
                        last_read: r.get::<_, f64>(7)? as f32,
                        total: r.get(8)?,
                        score: r.get::<_, f64>(9)? as f32,
                        status: r.get(10)?,
                        started: r.get(11)?,
                        finished: r.get(12)?,
                        private: r.get(13)?,
                    },
                ))
            })?
        {
            let (key, kind, t) = row?;
            if let Some(p) = index.get(key.as_str()).and_then(|&i| part(&mut data.series[i], kind)) {
                p.tracks.push(t);
            }
        }
        let mut saved = HashMap::new();
        for (pos, (key, s)) in keys.iter().zip(data.series.iter_mut()).enumerate() {
            saved.insert(key.clone(), Saved { pos, body: hash(&body(s)?), chapters: s.chapters.clone() });
        }
        self.groups = hashes;
        self.series = saved;
        Ok(Some(data))
    }

    /// Writes what changed since the last save and answers how many rows that took.
    pub fn save(&mut self, mut data: Data) -> Res<usize> {
        let mut series = std::mem::take(&mut data.series);
        let keys = keys(&series);
        let bodies: Vec<String> = series.iter_mut().map(body).collect::<Result<_, _>>()?;
        let groups = group_hashes(&data);
        let (old_series, old_groups) = (std::mem::take(&mut self.series), std::mem::take(&mut self.groups));
        match self.write(&data, &groups, &series, &keys, &bodies, &old_series, &old_groups) {
            Ok((writes, next_series)) => {
                self.series = next_series;
                self.groups = groups.into_iter().collect();
                Ok(writes)
            }
            Err(e) => {
                // The transaction rolled back, so disk still matches what it was before.
                self.series = old_series;
                self.groups = old_groups;
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        data: &Data,
        groups: &[(&'static str, u64); 4],
        series: &[Series],
        keys: &[String],
        bodies: &[String],
        old_series: &HashMap<String, Saved>,
        old_groups: &HashMap<&'static str, u64>,
    ) -> Res<(usize, HashMap<String, Saved>)> {
        let conn = self.open()?;
        let tx = conn.transaction()?;
        let mut writes = 0;
        for (group, h) in groups {
            if old_groups.get(group) != Some(h) {
                write_group(&tx, group, data)?;
                writes += 1;
            }
        }
        let mut next_series = HashMap::with_capacity(series.len());
        {
            let mut mv = tx.prepare_cached("UPDATE series SET pos = ?2 WHERE key = ?1")?;
            let mut chapters = tx.prepare_cached("INSERT INTO chapters(key, list) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET list = excluded.list")?;
            for (pos, ((s, key), body)) in series.iter().zip(keys).zip(bodies).enumerate() {
                let h = hash(body);
                let old = old_series.get(key);
                match old {
                    Some(o) if o.body == h => {
                        if o.pos != pos {
                            mv.execute(params![key, pos as i64])?;
                            writes += 1;
                        }
                    }
                    _ => {
                        write_series(&tx, key, pos, s)?;
                        writes += 1;
                    }
                }
                let changed = match old {
                    Some(o) => !Arc::ptr_eq(&o.chapters, &s.chapters),
                    None => !s.chapters.is_empty(),
                };
                if changed {
                    chapters.execute(params![key, serde_json::to_string(&*s.chapters)?])?;
                    writes += 1;
                }
                next_series.insert(key.clone(), Saved { pos, body: h, chapters: s.chapters.clone() });
            }
        }
        let gone: HashSet<&String> = old_series.keys().filter(|k| !next_series.contains_key(*k)).collect();
        for k in gone {
            tx.execute("DELETE FROM series WHERE key = ?1", params![k])?;
            writes += 1;
        }
        tx.commit()?;
        Ok((writes, next_series))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Progress, Series};
    use super::*;
    use serde_json::json;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nano-db-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn series(url: &str, chapters: usize) -> Series {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": url, "title": url }));
        s.chapters = Arc::new((0..chapters).rev().map(|i| json!({ "url": format!("{url}/c{i}"), "name": format!("Ch.{i}") })).collect());
        s
    }

    fn data() -> Data {
        let mut d = Data::default();
        let mut a = series("/a", 5);
        a.in_library = true;
        a.progress = Some(Progress { chapter_url: "/a/c1".into(), chapter_name: "Ch.1".into(), page: 2, pages: 9, at: 100 });
        a.marks.insert("/a/c0".into(), true);
        d.series = vec![a, series("/b", 3), series("", 0), series("", 2)];
        d.zoom = Some(1.5);
        d
    }

    /// Every struct is spelled out with no `..Default::default()`, so adding a field to any of them stops this compiling until it is given a value here.
    /// Every value differs from its default, so the round trip fails for a field this file doesn't save.
    fn full() -> Data {
        use crate::{downloads::Format, extensions::Repo, store::{Nsfw, Prefs}, sync::{Config, Meta, Snapshot}, track::Login};
        let track = |n: i64| Track { tracker: n, library_id: n + 1, media_id: n + 2, url: format!("/t{n}"), title: format!("T{n}"), last_read: 1.5, total: n + 3, score: 7.5, status: n + 4, started: n + 5, finished: n + 6, private: true };
        let snapshot = |n: i64| Snapshot {
            in_library: true,
            progress: Some((format!("/s{n}/c1"), n as usize + 2, n + 300)),
            marks: [(format!("/s{n}/c1"), true), (format!("/s{n}/c2"), false)].into(),
            bookmarks: [format!("/s{n}/c3")].into(),
            categories: vec![n, n + 1],
            tracks: vec![track(n)],
        };
        let series = Series {
            source: "9".into(),
            source_name: "Source".into(),
            manga: json!({ "url": "/full", "title": "Full", "extra": { "nested": [1, 2] } }),
            in_library: true,
            added_at: 11,
            chapters: Arc::new(vec![json!({ "url": "/full/c1", "name": "Ch.1", "scanlator": "Group" })]),
            progress: Some(Progress { chapter_url: "/full/c1".into(), chapter_name: "Ch.1".into(), page: 4, pages: 20, at: 12 }),
            hidden_scanlators: vec!["Group".into(), "Other".into()],
            scanlators_chosen: true,
            sync: Some(Meta { sent: snapshot(10) }),
            sync1: Some(Meta { sent: snapshot(20) }),
            marks: [("/full/c1".to_string(), true), ("/full/c2".to_string(), false)].into(),
            bookmarks: ["/full/c1".to_string(), "/full/c2".to_string()].into(),
            categories: vec![3, 1],
            tracks: vec![track(1), track(2)],
            independent_chapters: false,
            history_cleared_at: 13,
        };
        Data {
            series: vec![series],
            languages: [("1".to_string(), vec!["en".to_string(), "ja".to_string()]), ("2".to_string(), vec![])].into(),
            sites: [("3".to_string(), vec!["a.example".to_string()]), ("4".to_string(), vec![])].into(),
            zoom: Some(1.25),
            solver_url: "http://solver".into(),
            page_cache_mb: Some(512),
            prefs: Prefs {
                host_heap_mb: Some(2048),
                vertical_reader: true,
                right_to_left: true,
                sidebar_collapsed: true,
                auto_download: true,
                fonts: [("ui".to_string(), "Inter".to_string())].into(),
                reader_fullscreen: true,
                chapters_oldest_first: true,
                updates_off: true,
                last_update_check: 14,
                dismissed_update: "0.9.9".into(),
                source_order: vec!["2".into(), "1".into()],
                download_dir: "/dl".into(),
                download_format: Format::Cbz,
                nsfw: Nsfw { all: true, mixed: true, library: true, updates: true, history: true, downloaded: true, sources: true, extensions: true },
                nsfw_sources: vec!["5".into(), "6".into()],
                mixed_sources: vec!["7".into()],
                nsfw_categories: vec![2, 1],
            },
            sync: Config { host: "http://sync".into(), key: "k".into(), device_id: "d".into(), cursor: 15, v1_etag: "e".into() },
            repos: vec![Repo { url: "https://r.example/index.json".into(), name: "R".into(), key: "ab".repeat(32) }],
            categories: vec![Category { name: "Reading".into(), order: 1, flags: 2, uid: 3, version: 4 }],
            categories_changed: true,
            deleted_categories: vec!["Old".into(), "Older".into()],
            trackers: [(1, Login { access: "a".into(), refresh: "r".into(), expires: 16, user: "u".into(), user_id: 17 })].into(),
        }
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let path = dir("full").join("library.db");
        let d = full();
        Db::new(path.clone()).save(d.clone()).unwrap();
        let back = Db::new(path.clone()).load().unwrap().unwrap();
        assert_eq!(json_of(&back), json_of(&d), "a field was not saved, add it to db.rs");
        // And once more through a library that was already loaded, which saves nothing it didn't change.
        let mut db = Db::new(path);
        let loaded = db.load().unwrap().unwrap();
        assert_eq!(db.save(loaded).unwrap(), 0);
    }

    fn json_of(d: &Data) -> Value {
        serde_json::to_value(d).unwrap()
    }

    #[test]
    fn everything_survives_a_round_trip_including_repeated_keys() {
        let path = dir("round").join("library.db");
        let d = data();
        Db::new(path.clone()).save(d.clone()).unwrap();
        let back = Db::new(path).load().unwrap().unwrap();
        assert_eq!(json_of(&back), json_of(&d));
        assert_eq!(back.series.len(), 4);
        assert_eq!(back.series[3].chapters.len(), 2);
    }

    #[test]
    fn a_save_writes_only_what_changed() {
        let path = dir("incremental").join("library.db");
        let mut db = Db::new(path.clone());
        let d = data();
        assert!(db.save(d.clone()).unwrap() > 0);
        assert_eq!(db.save(d.clone()).unwrap(), 0, "nothing changed");

        // A reload must not look changed either.
        let mut db = Db::new(path);
        let mut d = db.load().unwrap().unwrap();
        assert_eq!(db.save(d.clone()).unwrap(), 0, "a loaded library saves nothing");

        // Reading on rewrites one small row and leaves the chapter list alone.
        d.series[0].progress.as_mut().unwrap().page = 3;
        assert_eq!(db.save(d.clone()).unwrap(), 1);

        // A new chapter list writes that list, and the row only if the series itself changed.
        d.series[1].chapters = Arc::new(vec![json!({ "url": "/b/c9" })]);
        assert_eq!(db.save(d.clone()).unwrap(), 1);

        // A setting is one row too.
        d.zoom = Some(2.0);
        assert_eq!(db.save(d.clone()).unwrap(), 1);

        // Removing a series removes its row and its chapters.
        d.series.remove(1);
        assert_eq!(db.save(d.clone()).unwrap(), 1 + 2, "the row, then the two series after it move up");
        let back = Db::new(db.path.clone()).load().unwrap().unwrap();
        assert_eq!(json_of(&back), json_of(&d));
    }

    #[test]
    fn moving_a_series_only_updates_its_position() {
        let path = dir("order").join("library.db");
        let mut db = Db::new(path.clone());
        let mut d = data();
        db.save(d.clone()).unwrap();
        d.series.swap(0, 1);
        assert_eq!(db.save(d.clone()).unwrap(), 2);
        let back = Db::new(path).load().unwrap().unwrap();
        assert_eq!(back.series[0].url(), "/b");
        assert_eq!(back.series[1].url(), "/a");
    }

    #[test]
    fn a_backup_reads_back_the_same_and_replaces_the_last_one() {
        let d = dir("backup");
        let (path, copy) = (d.join("library.db"), d.join("library.db.bak"));
        let mut db = Db::new(path.clone());
        let mut data = data();
        db.save(data.clone()).unwrap();
        backup(&path, &copy).unwrap();
        data.zoom = Some(3.0);
        db.save(data.clone()).unwrap();
        backup(&path, &copy).unwrap();
        let back = Db::new(copy).load().unwrap().unwrap();
        assert_eq!(json_of(&back), json_of(&data));
        assert!(!d.join("library.db.bak.new").exists());
    }

    #[test]
    fn no_database_means_nothing_to_load() {
        assert!(Db::new(dir("none").join("library.db")).load().unwrap().is_none());
    }
}
