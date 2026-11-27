//! What Nano remembers between runs, in one JSON file saved a second after each change.

use gpui::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod db;

mod arc_vec {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer, T: Serialize>(v: &Arc<Vec<T>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_slice().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Arc<Vec<T>>, D::Error> {
        Vec::<T>::deserialize(d).map(Arc::new)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub unread: usize,
    pub caught_up: bool,
    pub read_fraction: Option<f32>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Series {
    pub source: String,
    pub source_name: String,
    pub manga: Value,
    pub in_library: bool,
    pub added_at: i64,
    #[serde(with = "arc_vec")]
    pub chapters: Arc<Vec<Value>>,
    pub progress: Option<Progress>,
    /// Hidden scanlators (or languages, on multi-language sources).
    #[serde(default)]
    pub hidden_scanlators: Vec<String>,
    /// Until the user picks, only the main scanlator shows.
    #[serde(default)]
    pub scanlators_chosen: bool,
    #[serde(default)]
    pub sync: Option<crate::sync::Meta>,
    /// Same as `sync`, for the old whole-library file.
    #[serde(default)]
    pub sync1: Option<crate::sync::Meta>,
    /// Chapters marked read (true) or unread (false) by hand, by url. They win over what the progress implies.
    #[serde(default)]
    pub marks: std::collections::BTreeMap<String, bool>,
    /// Bookmarked chapters, by url.
    #[serde(default)]
    pub bookmarks: std::collections::BTreeSet<String>,
    /// The categories it's in, by their `order`, as Mihon's backup does.
    #[serde(default)]
    pub categories: Vec<i64>,
    /// Tracker entries, as Mihon's backup keeps them.
    #[serde(default)]
    pub tracks: Vec<Track>,
    //
    #[serde(default)]
    pub independent_chapters: bool,
    /// Timestamp of the last explicit history clear, so sync does not resurrect older progress.
    #[serde(default)]
    pub history_cleared_at: i64,
}

/// A series on a tracker. Mihon's BackupTracking fields, so sync carries them unchanged.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Track {
    /// Mihon's tracker id: 1 MyAnimeList, 2 AniList, 3 Kitsu, 7 MangaUpdates.
    pub tracker: i64,
    /// AniList's or Kitsu's list entry id. The others have none.
    pub library_id: i64,
    pub media_id: i64,
    pub url: String,
    pub title: String,
    pub last_read: f32,
    pub total: i64,
    /// 0 to 10 on MyAnimeList and MangaUpdates, 2 to 20 on Kitsu, 0 to 100 on AniList.
    pub score: f32,
    pub status: i64,
    pub started: i64,
    pub finished: i64,
    pub private: bool,
}

/// A library shelf. Mihon's backup fields, so phones and Nano agree on them through sync.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Category {
    pub name: String,
    pub order: i64,
    #[serde(default)]
    pub flags: i64,
    #[serde(default)]
    pub uid: i64,
    #[serde(default)]
    pub version: i64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Progress {
    pub chapter_url: String,
    pub chapter_name: String,
    pub page: usize,
    pub pages: usize,
    pub at: i64,
}

impl Series {
    pub fn new(source: String, source_name: String, manga: Value) -> Self {
        Series {
            source,
            source_name,
            manga,
            in_library: false,
            added_at: 0,
            chapters: Arc::default(),
            progress: None,
            hidden_scanlators: vec![],
            scanlators_chosen: false,
            sync: None,
            sync1: None,
            marks: Default::default(),
            bookmarks: Default::default(),
            categories: vec![],
            tracks: vec![],
            independent_chapters: true,
            history_cleared_at: 0,
        }
    }

    /// In the library, read, or with an unsynced change.
    fn kept(&self) -> bool {
        self.in_library
            || self.progress.is_some()
            || !self.marks.is_empty()
            || !self.bookmarks.is_empty()
            || crate::sync::unsent(self)
    }

    pub fn url(&self) -> &str {
        self.manga["url"].as_str().unwrap_or_default()
    }

    pub fn title(&self) -> &str {
        self.manga["title"].as_str().unwrap_or("Untitled")
    }

    pub fn visible(&self) -> Vec<Value> {
        visible_chapters(&self.chapters, &self.hidden())
    }

    pub fn hidden(&self) -> Vec<String> {
        if self.scanlators_chosen {
            self.hidden_scanlators.clone()
        } else {
            default_hidden(&self.chapters, self.progress.as_ref().map(|p| p.chapter_url.as_str()))
        }
    }

    pub fn reading_index(&self) -> Option<usize> {
        let p = self.progress.as_ref()?;
        self.visible().iter().position(|c| c["url"] == p.chapter_url.as_str())
    }

    /// How far through the series you are, from 0 to 1. The current chapter counts by page. Kept to check `stats_of` against.
    #[cfg(test)]
    pub fn read_fraction(&self) -> Option<f32> {
        let p = self.progress.as_ref()?;
        let list = self.visible();
        let total = list.len();
        if total == 0 {
            return None;
        }
        let flags = self.read_flags(&list);
        let read_count = flags.iter().filter(|r| **r).count() as f32;
        let current_partial = match list.iter().position(|c| c["url"] == p.chapter_url.as_str()) {
            Some(i) if !flags[i] && p.pages > 0 => ((p.page + 1) as f32 / p.pages as f32).min(1.),
            _ => 0.,
        };
        Some(((read_count + current_partial) / total as f32).min(1.))
    }

    /// Whether each chapter in `list` (newest first) is read: older than the one being read, finished, or marked by hand.
    pub fn read_flags(&self, list: &[Value]) -> Vec<bool> {
        list.iter()
            .map(|c| self.marks.get(c["url"].as_str().unwrap_or_default()) == Some(&true))
            .collect()
    }
    /// Whether each chapter counts as new: newer than the one being read, or marked unread.
    /// An unstarted series has nothing new, so its whole list isn't a backlog.
    /// Whether each chapter counts as new: explicitly marked unread, or unread and newer than the latest read chapter.
    /// An unstarted series without manual unread marks has nothing new, so its whole list isn't a backlog.
    pub fn new_flags(&self, list: &[Value]) -> Vec<bool> {
        let flags = self.read_flags(list);
        let latest_read = flags.iter().position(|r| *r);
        list.iter()
            .enumerate()
            .map(|(i, c)| {
                let url = c["url"].as_str().unwrap_or_default();
                self.marks.get(url) == Some(&false)
                    || latest_read.is_some_and(|r| i < r && !flags[i])
            })
            .collect()
    }

    #[cfg(test)]
    pub fn caught_up(&self) -> bool {
        let list = self.visible();
        (self.progress.is_some() || !self.marks.is_empty()) && !list.is_empty() && self.read_flags(&list).iter().all(|r| *r)
    }

    pub fn unread(&self) -> usize {
        self.new_flags(&self.visible()).iter().filter(|n| **n).count()
    }

    /// Single-pass unread, caught-up, and progress fraction over an already-filtered slice.
    pub fn stats_of(&self, list: &[Value]) -> Stats {
        let mark = |c: &Value| self.marks.get(c["url"].as_str().unwrap_or_default()).copied();
        let latest_read = list.iter().position(|c| mark(c) == Some(true));
        let (mut read_count, mut unread) = (0usize, 0usize);
        for (i, c) in list.iter().enumerate() {
            match mark(c) {
                Some(true) => read_count += 1,
                Some(false) => unread += 1,
                None => unread += latest_read.is_some_and(|r| i < r) as usize,
            }
        }
        let caught_up = (self.progress.is_some() || !self.marks.is_empty()) && !list.is_empty() && read_count == list.len();
        let read_fraction = self.progress.as_ref().filter(|_| !list.is_empty()).map(|p| {
            let partial = match list.iter().position(|c| c["url"] == p.chapter_url.as_str()) {
                Some(i) if mark(&list[i]) != Some(true) && p.pages > 0 => ((p.page + 1) as f32 / p.pages as f32).min(1.),
                _ => 0.,
            };
            ((read_count as f32 + partial) / list.len() as f32).min(1.)
        });
        Stats { unread, caught_up, read_fraction }
    }

    /// Marks chapters read or unread by hand.
    pub fn mark(&mut self, urls: &[String], read: bool) {
        for url in urls {
            self.marks.insert(url.clone(), read);
        }
    }

    /// Migrates legacy linear `progress` into explicit per-chapter `marks` once.
    pub fn migrate_read_marks(&mut self) {
        if self.independent_chapters {
            return;
        }
        self.independent_chapters = true;
        let Some(p) = &self.progress else { return };
        let mut list = self.visible();
        if !list.iter().any(|c| c["url"] == p.chapter_url.as_str()) {
            list = self.chapters.to_vec();
        }
        let Some(r) = list.iter().position(|c| c["url"] == p.chapter_url.as_str()) else { return };
        let finished = p.pages > 0 && p.page + 1 >= p.pages;
        for (i, c) in list.iter().enumerate() {
            if i > r || (i == r && finished) {
                if let Some(url) = c["url"].as_str().filter(|u| !u.is_empty()) {
                    self.marks.entry(url.to_string()).or_insert(true);
                }
            }
        }
    }

    /// The highest chapter number read, which is what trackers count. Falls back to how many chapters are read.
    pub fn read_number(&self) -> f32 {
        let mut list = self.visible();
        if self.progress.as_ref().is_some_and(|p| !list.iter().any(|c| c["url"] == p.chapter_url.as_str())) {
            list = (*self.chapters).clone();
        }
        let flags = self.read_flags(&list);
        let read: Vec<&Value> = list.iter().zip(flags).filter(|(_, r)| *r).map(|(c, _)| c).collect();
        let highest = read.iter().filter_map(|c| crate::migrate::chapter_number(c)).fold(0., f64::max) as f32;
        match highest {
            n if n > 0. => n,
            _ if !read.is_empty() => read.len() as f32,
            _ => self.progress.as_ref().and_then(|p| crate::migrate::name_number(&p.chapter_name)).unwrap_or(0.) as f32,
        }
    }

    pub fn toggle_bookmark(&mut self, url: &str) {
        if !self.bookmarks.remove(url) {
            self.bookmarks.insert(url.to_string());
        }
    }

    fn last_active(&self) -> i64 {
        self.progress.as_ref().map_or(self.added_at, |p| p.at.max(self.added_at))
    }
}

pub fn scanlator(c: &Value) -> &str {
    c["scanlator"].as_str().filter(|s| !s.is_empty()).unwrap_or("Unknown")
}

pub fn scanlator_counts(chapters: &[Value]) -> Vec<(String, usize)> {
    let mut groups: Vec<(String, usize)> = vec![];
    for c in chapters {
        let s = scanlator(c);
        match groups.iter_mut().find(|(g, _)| g == s) {
            Some((_, n)) => *n += 1,
            None => groups.push((s.to_string(), 1)),
        }
    }
    groups.sort_by_key(|(g, n)| (std::cmp::Reverse(*n), g.clone()));
    groups
}

/// Before the user picks, show the group of the chapter being read, or else the biggest group.
pub fn default_hidden(chapters: &[Value], reading_url: Option<&str>) -> Vec<String> {
    let groups = scanlator_counts(chapters);
    let reading = reading_url.and_then(|u| chapters.iter().find(|c| c["url"] == u)).map(scanlator);
    let Some(keep) = reading.or(groups.first().map(|(g, _)| g.as_str())) else { return vec![] };
    groups.iter().filter(|(g, _)| g != keep).map(|(g, _)| g.clone()).collect()
}

pub fn visible_chapters(chapters: &[Value], hidden: &[String]) -> Vec<Value> {
    chapters.iter().filter(|c| !hidden.iter().any(|h| h == scanlator(c))).cloned().collect()
}

/// Plain settings with no side effects, changed through `edit_prefs`.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Prefs {
    /// Unset means sized for this machine.
    #[serde(default)]
    pub host_heap_mb: Option<u32>,
    #[serde(default)]
    pub vertical_reader: bool,
    /// Paged mode turns pages right to left, as most manga reads.
    #[serde(default)]
    pub right_to_left: bool,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default)]
    pub auto_download: bool,
    #[serde(default)]
    pub fonts: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub reader_fullscreen: bool,
    #[serde(default)]
    pub chapters_oldest_first: bool,
    #[serde(default)]
    pub updates_off: bool,
    #[serde(default)]
    pub last_update_check: i64,
    #[serde(default)]
    pub dismissed_update: String,
    #[serde(default)]
    pub source_order: Vec<String>,
    /// Empty means the default folder inside Nano's data.
    #[serde(default)]
    pub download_dir: String,
    #[serde(default)]
    pub download_format: crate::downloads::Format,
    /// Where NSFW sources are hidden.
    #[serde(default)]
    pub nsfw: Nsfw,
    /// Ids of the sources the repos flag as NSFW, remembered so they stay hidden after their extension is removed.
    #[serde(default)]
    pub nsfw_sources: Vec<String>,
    /// Same, for sources in extensions that mix 18+ content in with the rest.
    #[serde(default)]
    pub mixed_sources: Vec<String>,
    /// Categories marked NSFW, by their `order`.
    #[serde(default)]
    pub nsfw_categories: Vec<i64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Area {
    Library,
    Updates,
    History,
    Downloaded,
    Sources,
    Extensions,
}

/// One switch per place NSFW can show up.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Nsfw {
    /// Hides everywhere, whatever the switches below say.
    pub all: bool,
    /// Counts sources from extensions that mix in 18+ content as NSFW too.
    pub mixed: bool,
    pub library: bool,
    pub updates: bool,
    pub history: bool,
    pub downloaded: bool,
    pub sources: bool,
    pub extensions: bool,
}

impl Default for Nsfw {
    fn default() -> Self {
        Nsfw {
            all: false,
            mixed: true,
            library: false,
            updates: false,
            history: false,
            downloaded: false,
            sources: false,
            extensions: false,
        }
    }
}

impl Nsfw {
    pub fn get(&self, area: Area) -> bool {
        self.all
            || match area {
            Area::Library => self.library,
            Area::Updates => self.updates,
            Area::History => self.history,
            Area::Downloaded => self.downloaded,
            Area::Sources => self.sources,
            Area::Extensions => self.extensions,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Data {
    #[serde(default)]
    series: Vec<Series>,
    #[serde(default)]
    languages: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    sites: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    zoom: Option<f32>,
    #[serde(default)]
    solver_url: String,
    #[serde(default)]
    page_cache_mb: Option<u32>,
    #[serde(flatten)]
    prefs: Prefs,
    #[serde(default)]
    sync: crate::sync::Config,
    #[serde(default)]
    repos: Vec<crate::extensions::Repo>,
    #[serde(default)]
    categories: Vec<Category>,
    /// Categories were added, renamed or removed here since the last sync.
    #[serde(default)]
    categories_changed: bool,
    /// Names deleted here, so a sync doesn't bring them back. Mihon's backup has no way to say "deleted".
    #[serde(default)]
    deleted_categories: Vec<String>,
    /// Tracker sign-ins, by Mihon's tracker id.
    #[serde(default)]
    trackers: std::collections::BTreeMap<i64, crate::track::Login>,
}

#[derive(Clone)]
pub enum SyncStatus {
    Idle,
    Syncing,
    Done(i64),
    Failed(String),
}

pub struct Store {
    path: PathBuf,
    db: Arc<Mutex<db::Db>>,
    history_undo_gen: u64,
    data: Data,
    saving: bool,
    dirty: bool,
    pub sync_status: SyncStatus,
    /// The last tracker update that failed, shown on the series page.
    pub track_error: Option<String>,
    pub update: Option<crate::update::Update>,
    pub update_note: &'static str,
    history_undo: Vec<HistoryEntry>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Store {
    /// Builds the database from a JSON copy of the library, and sets that file aside when it was the live one.
    fn rebuild_from(src: &Path, db_path: &Path, db: &mut db::Db, set_aside: bool) -> Option<Data> {
        let mut data = std::fs::read(src).ok().and_then(|b| {
            serde_json::from_slice::<Data>(&b)
                .map_err(|e| eprintln!("{}: {e}", src.display()))
                .ok()
        })?;
        for s in &mut data.series {
            s.migrate_read_marks();
        }
        // Built beside the real file and moved in once complete, so a crash can't leave half a library.
        let staging = db_path.with_extension("db.new");
        let _ = std::fs::remove_file(&staging);
        let built = {
            let mut tmp = db::Db::new(staging.clone());
            tmp.save(data.clone())
        };
        if let Err(e) = built {
            eprintln!("{}: {e}", staging.display());
            return Some(data);
        }
        if std::fs::rename(&staging, db_path).is_err() {
            return Some(data);
        }
        match db.load() {
            Ok(Some(stored)) => {
                if set_aside {
                    let _ = std::fs::rename(src, src.with_extension("json.pre-sqlite"));
                }
                Some(stored)
            }
            _ => Some(data),
        }
    }

    /// Puts the last good database back, copying it over the missing one.
    fn restore_backup(db_path: &Path, db: &mut db::Db) -> Option<Data> {
        let backup = db_path.with_extension("db.bak");
        std::fs::copy(&backup, db_path).ok()?;
        *db = db::Db::new(db_path.to_path_buf());
        match db.load() {
            Ok(Some(data)) => {
                eprintln!("{} restored from {}", db_path.display(), backup.display());
                Some(data)
            }
            _ => {
                *db = db::Db::new(db_path.to_path_buf());
                let _ = std::fs::remove_file(db_path);
                None
            }
        }
    }

    /// With no usable database, takes the library from the first copy that reads: library.json, the last backup, the copy kept at the upgrade.
    fn recover(path: &Path, db_path: &Path, db: &mut db::Db) -> Data {
        Self::rebuild_from(path, db_path, db, true)
            .or_else(|| Self::restore_backup(db_path, db))
            .or_else(|| Self::rebuild_from(&path.with_extension("json.pre-sqlite"), db_path, db, false))
            .unwrap_or_default()
    }

    /// Keeps an unreadable database, and its write-ahead log so that is not applied to the next one.
    fn set_aside(db_path: &Path) {
        for (from, to) in [("db", "db.broken"), ("db-wal", "db.broken-wal"), ("db-shm", "db.broken-shm")] {
            let to = db_path.with_extension(to);
            let _ = std::fs::remove_file(&to);
            let _ = std::fs::rename(db_path.with_extension(from), to);
        }
    }

    /// A fresh copy of the database once per launch, so a bad one costs the last session and not the library.
    fn keep_a_backup(db_path: &Path) {
        let (from, to) = (db_path.to_path_buf(), db_path.with_extension("db.bak"));
        let run = move || {
            if let Err(e) = db::backup(&from, &to) {
                eprintln!("backup {}: {e}", to.display());
            }
        };
        if cfg!(test) {
            run();
        } else {
            std::thread::spawn(run);
        }
    }

    pub fn load(path: PathBuf) -> Self {
        let db_path = path.with_extension("db");
        let mut db = db::Db::new(db_path.clone());
        let mut data: Data = match db.load() {
            Ok(Some(data)) => data,
            Ok(None) => Self::recover(&path, &db_path, &mut db),
            Err(e) => {
                eprintln!("{}: {e}", db_path.display());
                db = db::Db::new(db_path.clone());
                Self::set_aside(&db_path);
                Self::recover(&path, &db_path, &mut db)
            }
        };
        if db_path.exists() {
            Self::keep_a_backup(&db_path);
        }
        for s in &mut data.series {
            s.migrate_read_marks();
        }
        Store {
            history_undo_gen: 0,
            path,
            db: Arc::new(Mutex::new(db)),
            data,
            saving: false,
            dirty: false,
            sync_status: SyncStatus::Idle,
            track_error: None,
            update: None,
            update_note: "",
            history_undo: vec![],
        }
    }

    pub fn get(&self, source: &str, url: &str) -> Option<&Series> {
        self.data
            .series
            .iter()
            .find(|s| s.source == source && s.url() == url)
    }

    /// Whether series from this source stay out of `area`.
    pub fn hides(&self, area: Area, source: &str) -> bool {
        let p = &self.data.prefs;
        p.nsfw.get(area)
            && (p.nsfw_sources.iter().any(|s| s == source)
                || (p.nsfw.mixed && p.mixed_sources.iter().any(|s| s == source)))
    }

    /// Whether a series stays out of `area`, because of its source or because it's in a category marked NSFW.
    pub fn hides_series(&self, area: Area, s: &Series) -> bool {
        let p = &self.data.prefs;
        self.hides(area, &s.source)
            || (p.nsfw.get(area) && s.categories.iter().any(|c| p.nsfw_categories.contains(c)))
    }

    /// Whether the category's tab is left off the Library page.
    pub fn hides_category(&self, order: i64) -> bool {
        let p = &self.data.prefs;
        p.nsfw.get(Area::Library) && p.nsfw_categories.contains(&order)
    }

    pub fn library(&self) -> Vec<&Series> {
        let mut list: Vec<_> = self.data.series.iter().filter(|s| s.in_library).collect();
        list.sort_by_key(|s| -s.last_active());
        list
    }

    pub fn history(&self) -> Vec<&Series> {
        let mut list: Vec<_> = self
            .data
            .series
            .iter()
            .filter(|s| s.progress.is_some())
            .collect();
        list.sort_by_key(|s| -s.progress.as_ref().map_or(0, |p| p.at));
        list
    }

    pub fn update<T>(
        &mut self,
        source: &str,
        source_name: &str,
        manga: &Value,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Series) -> T,
    ) -> T {
        let url = manga["url"].as_str().unwrap_or_default();
        let ix = match self.data.series.iter().position(|s| s.source == source && s.url() == url) {
            Some(ix) => ix,
            None => {
                self.data.series.push(Series::new(source.into(), source_name.into(), manga.clone()));
                self.data.series.len() - 1
            }
        };
        let before = self.data.series[ix].read_number();
        let before_progress = self.data.series[ix].progress.clone();
        let out = f(&mut self.data.series[ix]);
        let s = &mut self.data.series[ix];
        let after = s.read_number();
        let after_progress = s.progress.clone();
        if after > before {
            let now = now_ms();
            let moved: Vec<usize> = (0..s.tracks.len()).filter(|&i| crate::track::advance(&mut s.tracks[i], after, now)).collect();
            if !moved.is_empty() {
                let key = (s.source.clone(), s.url().to_string());
                for i in moved {
                    let t = self.data.series[ix].tracks[i].clone();
                    self.push_track(key.clone(), t, cx);
                }
            }
        }
        if after_progress.is_some() && after_progress != before_progress {
            self.history_undo.clear();
        }
        self.data.series.retain(Series::kept);
        self.save_soon(cx);
        cx.notify();
        out
    }

    pub fn dir(&self) -> std::path::PathBuf {
        self.path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
    }

    pub fn all_series(&self) -> &[Series] {
        &self.data.series
    }

    pub fn edit_series<T>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Vec<Series>) -> T,
    ) -> T {
        let out = f(&mut self.data.series);
        self.data.series.retain(Series::kept);
        self.save_soon(cx);
        cx.notify();
        out
    }

    pub fn history_undo(&self) -> &[HistoryEntry] {
        &self.history_undo
    }

    pub fn remove_from_history(&mut self, source: &str, url: &str, cx: &mut Context<Self>) {
        let at = now_ms();
        if let Some(entry) =
            self.edit_series(cx, |series| clear_progress_for(series, source, url, at))
        {
            self.history_undo.push(entry);
            self.schedule_undo_dismiss(cx);
        }
    }

    /// Clears reading progress from all series and appends lightweight snapshots for Undo.
    pub fn clear_history(&mut self, cx: &mut Context<Self>) {
        let at = now_ms();
        let cleared = self.edit_series(cx, |series| clear_all_progress(series, at));
        if !cleared.is_empty() {
            self.history_undo.extend(cleared);
            self.schedule_undo_dismiss(cx);
        }
    }

    /// Restores series progress from the current Undo snapshot without overwriting newer reading progress.
    pub fn undo_history(&mut self, cx: &mut Context<Self>) {
        if self.history_undo.is_empty() {
            return;
        }
        let saved = std::mem::take(&mut self.history_undo);
        self.edit_series(cx, |series| restore_progress_entries(series, saved));
    }

    pub fn dismiss_history_undo(&mut self, cx: &mut Context<Self>) {
        if !self.history_undo.is_empty() {
            self.history_undo.clear();
            cx.notify();
        }
    }

    fn schedule_undo_dismiss(&mut self, cx: &mut Context<Self>) {
        self.history_undo_gen = self.history_undo_gen.wrapping_add(1);
        let stamp = self.history_undo_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            let _ = this.update(cx, |s, cx| {
                if s.history_undo_gen == stamp {
                    s.dismiss_history_undo(cx);
                }
            });
        })
        .detach();
    }

    pub fn sync_config(&self) -> &crate::sync::Config {
        &self.data.sync
    }

    /// A new server or key starts over.
    pub fn set_sync_account(&mut self, host: &str, key: &str, cx: &mut Context<Self>) {
        let (host, key) = (host.trim().trim_end_matches('/'), key.trim());
        if self.data.sync.host == host && self.data.sync.key == key {
            return;
        }
        self.data.sync.host = host.into();
        self.data.sync.key = key.into();
        self.data.sync.cursor = 0;
        self.data.sync.v1_etag.clear();
        self.edit_series(cx, |series| {
            series
                .iter_mut()
                .for_each(|s| (s.sync, s.sync1) = (None, None))
        });
    }

    pub fn set_v1_etag(&mut self, etag: String, cx: &mut Context<Self>) {
        self.data.sync.v1_etag = etag;
        self.save_soon(cx);
    }

    pub fn set_sync_cursor(&mut self, cursor: i64, cx: &mut Context<Self>) {
        self.data.sync.cursor = cursor;
        self.save_soon(cx);
    }

    pub fn start_sync(&mut self, cx: &mut Context<Self>) -> Option<crate::sync::Config> {
        let c = &mut self.data.sync;
        if matches!(self.sync_status, SyncStatus::Syncing) || c.host.is_empty() || c.key.is_empty() {
            return None;
        }
        if c.device_id.is_empty() {
            c.device_id = format!("nano-{:x}-{:x}", now_ms(), std::process::id());
        }
        self.sync_status = SyncStatus::Syncing;
        cx.notify();
        Some(self.data.sync.clone())
    }

    pub fn finish_sync(&mut self, result: Result<usize, String>, cx: &mut Context<Self>) {
        self.sync_status = match result {
            Ok(_) => SyncStatus::Done(now_ms()),
            Err(e) => SyncStatus::Failed(e),
        };
        cx.notify();
    }

    pub fn zoom(&self) -> f32 {
        self.data.zoom.unwrap_or(1.)
    }

    pub fn set_zoom(&mut self, z: f32, cx: &mut Context<Self>) {
        self.data.zoom = Some(z);
        self.save_soon(cx);
    }

    pub fn repos(&self) -> &[crate::extensions::Repo] {
        &self.data.repos
    }

    pub fn add_repo(&mut self, repo: crate::extensions::Repo, cx: &mut Context<Self>) {
        self.data.repos.retain(|r| r.url != repo.url);
        self.data.repos.push(repo);
        self.save_soon(cx);
    }

    pub fn remove_repo(&mut self, url: &str, cx: &mut Context<Self>) {
        self.data.repos.retain(|r| r.url != url);
        self.save_soon(cx);
    }

    /// In display order.
    pub fn categories(&self) -> Vec<Category> {
        let mut list = self.data.categories.clone();
        list.sort_by_key(|c| c.order);
        list
    }

    pub fn add_category(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() || self.data.categories.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            return;
        }
        let order = self.data.categories.iter().map(|c| c.order).max().map_or(1, |o| o + 1);
        // Mihon gives each category a random uid for sync to match on.
        let uid = (now_ms() ^ (std::process::id() as i64) << 20).abs();
        self.data.categories.push(Category { name: name.into(), order, flags: 0, uid, version: 1 });
        self.data.categories_changed = true;
        self.save_soon(cx);
        cx.notify();
    }

    pub fn remove_category(&mut self, order: i64, cx: &mut Context<Self>) {
        self.drop_category(order);
        self.save_soon(cx);
        cx.notify();
    }

    fn drop_category(&mut self, order: i64) {
        if let Some(c) = self.data.categories.iter().find(|c| c.order == order) {
            self.data.deleted_categories.push(c.name.to_lowercase());
        }
        self.data.categories.retain(|c| c.order != order);
        for s in &mut self.data.series {
            s.categories.retain(|o| *o != order);
        }
        // A new category can reuse the number, so it must not inherit the NSFW mark.
        self.data.prefs.nsfw_categories.retain(|o| *o != order);
        self.data.categories_changed = true;
    }

    fn renumber(&mut self, i: usize, to: i64) {
        let from = self.data.categories[i].order;
        self.data.categories[i].order = to;
        for s in &mut self.data.series {
            for o in s.categories.iter_mut().filter(|o| **o == from) {
                *o = to;
            }
        }
        self.data.categories_changed = true;
    }

    pub(crate) fn categories_changed(&self) -> bool {
        self.data.categories_changed
    }

    /// Takes the server's categories, matched by name. Its order numbers win, since series point at them.
    pub(crate) fn take_categories(&mut self, server: Vec<Category>, sent: bool) {
        for c in server {
            if self.data.deleted_categories.contains(&c.name.to_lowercase()) {
                continue;
            }
            // Another local category on that number moves out of the way first, taking its series along.
            if let Some(i) = self.data.categories.iter().position(|l| l.order == c.order && !l.name.eq_ignore_ascii_case(&c.name)) {
                let free = self.data.categories.iter().map(|l| l.order).max().unwrap_or(0).max(c.order) + 1;
                self.renumber(i, free);
            }
            match self.data.categories.iter_mut().find(|l| l.name.eq_ignore_ascii_case(&c.name)) {
                Some(local) if local.order != c.order => {
                    let old = local.order;
                    *local = c.clone();
                    for s in &mut self.data.series {
                        for o in s.categories.iter_mut().filter(|o| **o == old) {
                            *o = c.order;
                        }
                    }
                }
                Some(local) => *local = c,
                None => self.data.categories.push(c),
            }
        }
        if sent {
            self.data.categories_changed = false;
        }
    }

    /// Categories from a backup file, sent on the next sync.
    pub(crate) fn restore_categories(&mut self, categories: Vec<Category>) {
        if !categories.is_empty() {
            self.take_categories(categories, false);
            self.data.categories_changed = true;
        }
    }

    pub fn login(&self, tracker: i64) -> Option<&crate::track::Login> {
        self.data.trackers.get(&tracker)
    }

    pub fn set_login(
        &mut self,
        tracker: i64,
        login: Option<crate::track::Login>,
        cx: &mut Context<Self>,
    ) {
        match login {
            Some(l) => self.data.trackers.insert(tracker, l),
            None => self.data.trackers.remove(&tracker),
        };
        self.save_soon(cx);
        cx.notify();
    }

    /// Saves a track and sends it, refreshing the sign-in first if needed.
    pub fn push_track(&mut self, key: (String, String), mut t: Track, cx: &mut Context<Self>) {
        let Some(mut login) = self.login(t.tracker).cloned() else { return };
        self.put_track(&key, t.clone(), cx);
        cx.spawn(async move |this, cx| {
            let tracker = t.tracker;
            let result = cx
                .background_executor()
                .spawn(async move {
                    let refreshed = crate::track::refresh(tracker, &mut login)?;
                    crate::track::push(&login, &mut t)?;
                    Ok::<_, String>((t, refreshed.then_some(login)))
                })
                .await;
            let _ = this.update(cx, |store, cx| match result {
                Ok((t, login)) => {
                    if let Some(l) = login {
                        store.set_login(tracker, Some(l), cx);
                    }
                    store.put_track(&key, t, cx);
                    store.track_error = None;
                }
                Err(e) => {
                    eprintln!("{}: {e}", crate::track::name(tracker));
                    store.track_error = Some(format!("{}: {e}", crate::track::name(tracker)));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Replaces or adds a series' entry for that tracker, or removes it when `t.media_id` is 0.
    pub fn put_track(&mut self, key: &(String, String), t: Track, cx: &mut Context<Self>) {
        self.edit_series(cx, |series| {
            if let Some(s) = series
                .iter_mut()
                .find(|s| s.source == key.0 && s.url() == key.1)
            {
                s.tracks.retain(|x| x.tracker != t.tracker);
                if t.media_id != 0 {
                    s.tracks.push(t);
                }
            }
        });
    }

    pub fn prefs(&self) -> &Prefs {
        &self.data.prefs
    }

    pub fn edit_prefs(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Prefs)) {
        f(&mut self.data.prefs);
        self.save_soon(cx);
        cx.notify();
    }

    pub fn solver_url(&self) -> &str {
        &self.data.solver_url
    }

    pub fn set_solver_url(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.data.solver_url != url {
            self.data.solver_url = url.to_string();
            crate::solver::set_url(url);
            self.save_soon(cx);
        }
    }

    pub fn page_cache_mb(&self) -> u32 {
        self.data.page_cache_mb.unwrap_or(crate::page_cache::DEFAULT_MB)
    }

    pub fn set_page_cache_mb(&mut self, mb: u32, cx: &mut Context<Self>) {
        self.data.page_cache_mb = Some(mb);
        crate::page_cache::set_cap_mb(mb);
        self.save_soon(cx);
        cx.notify();
    }

        pub fn languages(&self, pkg: &str) -> Option<&Vec<String>> {
        self.data.languages.get(pkg)
    }

    pub fn set_languages(&mut self, pkg: &str, langs: Vec<String>, cx: &mut Context<Self>) {
        self.data.languages.insert(pkg.to_string(), langs);
        self.save_soon(cx);
        cx.notify();
    }

    pub fn sites(&self, pkg: &str) -> Option<&Vec<String>> {
        self.data.sites.get(pkg)
    }

    pub fn set_sites(&mut self, pkg: &str, sites: Vec<String>, cx: &mut Context<Self>) {
        self.data.sites.insert(pkg.to_string(), sites);
        self.save_soon(cx);
        cx.notify();
    }

    fn save_soon(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        if self.saving {
            return;
        }
        self.saving = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(Some((db, snapshot))) = this.update(cx, |s, _| {
                    if !s.dirty {
                        s.saving = false;
                        return None;
                    }
                    s.dirty = false;
                    Some((s.db.clone(), s.data.clone()))
                }) else {
                    return;
                };
                cx.background_executor()
                    .spawn(async move {
                        let mut db = db.lock().unwrap_or_else(|e| e.into_inner());
                        if let Err(e) = db.save(snapshot) {
                            eprintln!("save: {e}");
                        }
                    })
                    .await;
                let more = this
                    .update(cx, |s, _| {
                        if s.dirty {
                            true
                        } else {
                            s.saving = false;
                            false
                        }
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }
}

#[derive(Clone)]
pub enum HistoryEntry {
    /// Series stays in `data.series`, so only progress fields are kept for Undo.
    Progress {
        source: String,
        url: String,
        progress: Progress,
        cleared_at: i64,
    },
    /// Series will be dropped by `Series::kept`, so keep the full record to re-insert on Undo.
    Dropped(Box<Series>),
}

fn take_history_entry(s: &mut Series, at: i64) -> Option<HistoryEntry> {
    let progress = s.progress.take()?;
    let prev_cleared_at = s.history_cleared_at;
    s.history_cleared_at = prev_cleared_at.max(at).max(progress.at);
    if s.kept() {
        Some(HistoryEntry::Progress {
            source: s.source.clone(),
            url: s.url().to_string(),
            progress,
            cleared_at: prev_cleared_at,
        })
    } else {
        let mut snapshot = s.clone();
        snapshot.progress = Some(progress);
        snapshot.history_cleared_at = prev_cleared_at;
        Some(HistoryEntry::Dropped(Box::new(snapshot)))
    }
}

pub(crate) fn clear_progress_for(
    series: &mut [Series],
    source: &str,
    url: &str,
    at: i64,
) -> Option<HistoryEntry> {
    series
        .iter_mut()
        .find(|s| s.source == source && s.url() == url)
        .and_then(|s| take_history_entry(s, at))
}

pub(crate) fn clear_all_progress(series: &mut [Series], at: i64) -> Vec<HistoryEntry> {
    series
        .iter_mut()
        .filter_map(|s| take_history_entry(s, at))
        .collect()
}

pub(crate) fn restore_progress_entries(series: &mut Vec<Series>, saved: Vec<HistoryEntry>) {
    for entry in saved {
        let (source, url, progress, cleared_at, dropped) = match entry {
            HistoryEntry::Progress {
                source,
                url,
                progress,
                cleared_at,
            } => (source, url, progress, cleared_at, None),
            HistoryEntry::Dropped(mut old) => {
                let Some(progress) = old.progress.take() else {
                    continue;
                };
                let cleared_at = old.history_cleared_at;
                (
                    old.source.clone(),
                    old.url().to_string(),
                    progress,
                    cleared_at,
                    Some(old),
                )
            }
        };
        match series
            .iter_mut()
            .find(|s| s.source == source && s.url() == url)
        {
            Some(s) if s.progress.is_none() => {
                s.progress = Some(progress);
                s.history_cleared_at = cleared_at;
            }
            Some(_) => {} // read again after clearing; keep the newer progress
            None => {
                if let Some(mut old) = dropped {
                    old.progress = Some(progress);
                    old.history_cleared_at = cleared_at;
                    series.push(*old);
                }
            }
        }
    }
}

pub fn ago(ms: i64, now: i64) -> String {
    let mins = (now - ms).max(0) / 60_000;
    match mins {
        0 => "just now".into(),
        1..60 => format!("{mins} min ago"),
        60..1440 => format!("{} h ago", mins / 60),
        1440..2880 => "Yesterday".into(),
        2880..10080 => format!("{} days ago", mins / 1440),
        _ => crate::manga::ymd(ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relative_times() {
        let now = 1_763_337_600_000;
        assert_eq!(ago(now - 30_000, now), "just now");
        assert_eq!(ago(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(ago(now - 3 * 3_600_000, now), "3 h ago");
        assert_eq!(ago(now - 30 * 3_600_000, now), "Yesterday");
        assert_eq!(ago(now - 4 * 86_400_000, now), "4 days ago");
        assert_eq!(ago(now - 40 * 86_400_000, now), "2025-10-08");
    }

    #[test]
    fn read_number_survives_unnumbered_and_hidden_chapters() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.chapters = Arc::new((0..5).rev().map(|i| json!({ "url": format!("/c{i}"), "chapterNumber": -1.0 })).collect());
        s.mark(&["/c0".into(), "/c1".into()], true);
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "".into(), page: 0, pages: 2, at: 0 });
        assert_eq!(s.read_number(), 2.);
        Arc::make_mut(&mut s.chapters)[3] = json!({ "url": "/c1", "chapterNumber": -1.0, "name": "Chapter 7" });
        assert_eq!(s.read_number(), 7.);
        s.marks.clear();
        s.progress = Some(Progress { chapter_url: "/gone".into(), chapter_name: "Ch. 9".into(), page: 1, pages: 2, at: 0 });
        assert_eq!(s.read_number(), 9.);
    }

    #[test]
    fn unread_counts_chapters_newer_than_the_latest_read() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.in_library = true;
        s.chapters = Arc::new((0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect());
        assert_eq!(s.unread(), 0);
        s.mark(&["/c1".into()], true);
        s.progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "".into(), page: 0, pages: 2, at: 0 });
        assert_eq!(s.unread(), 3); // c4, c3, c2
        assert_eq!(s.read_fraction(), Some(0.2));
        assert!(!s.caught_up());
        s.mark(&["/c0".into(), "/c2".into(), "/c3".into(), "/c4".into()], true);
        s.progress = Some(Progress { chapter_url: "/c4".into(), chapter_name: "".into(), page: 1, pages: 2, at: 0 });
        assert!(s.caught_up());
    }

    #[test]
    fn server_categories_take_over_their_numbers() {
        let mut store = Store::load(std::env::temp_dir().join("nano-no-such-library.json"));
        let cat = |name: &str, order: i64| Category { name: name.into(), order, flags: 0, uid: order, version: 1 };
        store.data.categories = vec![cat("Reading", 1), cat("Plan", 2)];
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.categories = vec![2]; // in Plan
        store.data.series.push(s);
        // The phone has Reading at 2 and a new Done at 3.
        store.take_categories(vec![cat("reading", 2), cat("Done", 3)], false);
        let by = |n: &str| store.data.categories.iter().find(|c| c.name.eq_ignore_ascii_case(n)).map(|c| c.order);
        assert_eq!(by("Reading"), Some(2));
        let plan = by("Plan").unwrap();
        assert!(plan != 2 && plan != 3, "Plan moved out of the way");
        assert_eq!(store.data.series[0].categories, [plan], "its series moved with it");
        assert_eq!(by("Done"), Some(3));
        assert!(store.categories_changed(), "the move gets sent");
    }

    #[test]
    fn deleting_a_category_clears_its_nsfw_mark() {
        let mut store = Store::load(std::env::temp_dir().join("nano-no-such-library.json"));
        let cat = |name: &str, order: i64| Category {
            name: name.into(),
            order,
            flags: 0,
            uid: order,
            version: 1,
        };
        store.data.categories = vec![cat("Reading", 1), cat("Spicy", 2)];
        store.data.prefs.nsfw_categories = vec![2];
        store.drop_category(2);
        assert!(store.data.prefs.nsfw_categories.is_empty());
        // The next category gets number 2 again and starts unmarked.
        let order = store
            .data
            .categories
            .iter()
            .map(|c| c.order)
            .max()
            .map_or(1, |o| o + 1);
        assert!(!store.data.prefs.nsfw_categories.contains(&order));
    }

    #[test]
    fn marks_determine_read_state_independently_of_progress() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.chapters = Arc::new((0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect());
        s.mark(&["/c1".into(), "/c0".into()], true);
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "".into(), page: 0, pages: 4, at: 0 });
        assert_eq!(s.read_flags(&s.chapters), [false, false, false, true, true]); // c4 c3 c2 c1 c0
        assert_eq!(s.unread(), 3);
        s.mark(&["/c4".into(), "/c3".into()], true);
        s.mark(&["/c0".into()], false);
        assert_eq!(s.read_flags(&s.chapters), [true, true, false, true, false]);
        assert_eq!(s.new_flags(&s.chapters), [false, false, false, false, true]);
        assert_eq!(s.unread(), 1);
        assert!(!s.caught_up());
        s.mark(&["/c2".into(), "/c0".into()], true);
        assert!(s.caught_up());
        s.toggle_bookmark("/c3");
        assert!(s.bookmarks.contains("/c3"));
        s.toggle_bookmark("/c3");
        assert!(s.bookmarks.is_empty());
    }

    #[test]
    fn chapters_are_marked_independently() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.in_library = true;
        s.chapters = Arc::new((1..=10).rev().map(|i| json!({ "url": format!("/c{i}"), "chapterNumber": i as f64 })).collect());

        // Jumping to chapter 10 and finishing it marks only chapter 10 read.
        s.mark(&["/c10".into()], true);
        s.progress = Some(Progress { chapter_url: "/c10".into(), chapter_name: "Ch. 10".into(), page: 9, pages: 10, at: 100 });
        assert!(s.read_flags(&s.chapters)[0]);
        assert!(s.read_flags(&s.chapters)[1..].iter().all(|r| !*r));
        assert_eq!(s.unread(), 0);
        assert!(!s.caught_up());

        // Going back to chapter 5 and finishing it marks chapter 5 without clearing chapter 10.
        s.mark(&["/c5".into()], true);
        s.progress = Some(Progress { chapter_url: "/c5".into(), chapter_name: "Ch. 5".into(), page: 7, pages: 8, at: 200 });
        assert_eq!(s.marks.get("/c10"), Some(&true));
        assert_eq!(s.marks.get("/c5"), Some(&true));
        assert_eq!(s.marks.get("/c4"), None);
        assert_eq!(s.read_number(), 10.);
        assert_eq!(s.read_fraction(), Some(0.2));
    }

    #[test]
    fn legacy_progress_migrates_once_to_independent_marks() {
        let mut s: Series = serde_json::from_value(json!({
            "source": "1",
            "source_name": "S",
            "manga": { "url": "/m" },
            "in_library": true,
            "added_at": 0,
            "chapters": (0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect::<Vec<_>>(),
            "progress": { "chapter_url": "/c2", "chapter_name": "", "page": 1, "pages": 2, "at": 100 },
            "marks": { "/c0": false }
        }))
        .unwrap();
        assert!(!s.independent_chapters);
        s.migrate_read_marks();
        assert!(s.independent_chapters);
        assert_eq!(s.marks.get("/c2"), Some(&true));
        assert_eq!(s.marks.get("/c1"), Some(&true));
        assert_eq!(s.marks.get("/c0"), Some(&false));
        assert_eq!(s.marks.get("/c3"), None);
        assert_eq!(s.marks.get("/c4"), None);

        // Second call does not re-migrate if user later marks c1 unread.
        s.mark(&["/c1".into()], false);
        s.migrate_read_marks();
        assert_eq!(s.marks.get("/c1"), Some(&false));
    }

    #[test]
    fn default_filter_keeps_the_main_or_current_group() {
        let ch = |url: &str, g: &str| json!({ "url": url, "scanlator": g });
        let chapters = vec![ch("/1", "A"), ch("/2", "B"), ch("/3", "B"), ch("/4", "")];
        assert_eq!(
            default_hidden(&chapters, None),
            vec!["A".to_string(), "Unknown".to_string()]
        );
        assert_eq!(
            default_hidden(&chapters, Some("/1")),
            vec!["B".to_string(), "Unknown".to_string()]
        );
        assert!(default_hidden(&[], None).is_empty());
    }

    #[test]
    fn stats_of_matches_individual_methods() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        let assert_parity = |s: &Series| {
            let mut copy = s.clone();
            copy.chapters = Arc::new(s.visible());
            copy.hidden_scanlators.clear();
            copy.scanlators_chosen = true;
            assert_eq!(
                copy.stats_of(&copy.chapters),
                Stats { unread: copy.unread(), caught_up: copy.caught_up(), read_fraction: copy.read_fraction() }
            );
        };
        assert_parity(&s);
        s.chapters = Arc::new((0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect());
        s.scanlators_chosen = true;
        assert_parity(&s);
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "".into(), page: 0, pages: 4, at: 0 });
        assert_parity(&s);
        s.mark(&["/c4".into(), "/c3".into()], true);
        s.mark(&["/c0".into()], false);
        assert_parity(&s);
        s.mark(&["/c2".into(), "/c0".into()], true);
        assert_parity(&s);
        s.progress = Some(Progress { chapter_url: "/c4".into(), chapter_name: "".into(), page: 1, pages: 2, at: 0 });
        s.marks.clear();
        assert_parity(&s);
    }

    #[test]
    fn arc_vec_json_round_trip() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.chapters = Arc::new(vec![json!({ "url": "/c1", "memo": { "k": "v" } })]);
        let encoded = serde_json::to_string(&s).unwrap();
        assert!(encoded.contains(r#""chapters":[{"#));
        let decoded: Series = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.chapters.as_ref(), s.chapters.as_ref());
    }

    #[test]
    fn clear_and_undo_history() {
        let mut lib = Series::new("1".into(), "S".into(), json!({ "url": "/a" }));
        lib.in_library = true;
        lib.progress = Some(Progress {
            chapter_url: "/c1".into(),
            chapter_name: "Ch.1".into(),
            page: 1,
            pages: 5,
            at: 100,
        });

        let mut transient = Series::new("1".into(), "S".into(), json!({ "url": "/b" }));
        transient.progress = Some(Progress {
            chapter_url: "/c2".into(),
            chapter_name: "Ch.2".into(),
            page: 2,
            pages: 5,
            at: 200,
        });

        let mut series = vec![lib, transient];
        let single = clear_progress_for(&mut series, "1", "/b", 500).unwrap();
        assert!(matches!(single, HistoryEntry::Dropped(_)));
        series.retain(Series::kept);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].url(), "/a");

        // Undo re-inserts a dropped non-library series.
        restore_progress_entries(&mut series, vec![single]);
        assert_eq!(series.len(), 2);
        assert_eq!(series[1].url(), "/b");
        assert_eq!(series[1].progress.as_ref().map(|p| p.at), Some(200));

        // Clear all produces Progress for library series and Dropped for transient ones.
        let snapshot = clear_all_progress(&mut series, 600);
        assert_eq!(snapshot.len(), 2);
        series.retain(Series::kept);
        assert_eq!(series.len(), 1);
        assert!(series[0].in_library);
        assert!(series[0].progress.is_none());
        assert_eq!(series[0].history_cleared_at, 600);

        // Reading /a again before Undo must not be overwritten by Undo.
        series[0].progress = Some(Progress {
            chapter_url: "/c9".into(),
            chapter_name: "Ch.9".into(),
            page: 0,
            pages: 4,
            at: 700,
        });
        restore_progress_entries(&mut series, snapshot);
        assert_eq!(series.len(), 2, "dropped transient series is restored");
        assert_eq!(
            series[0].progress.as_ref().map(|p| p.chapter_url.as_str()),
            Some("/c9")
        );
        // Sequential individual removals accumulate and restore together.
        let e1 = clear_progress_for(&mut series, "1", "/a", 800).unwrap();
        let e2 = clear_progress_for(&mut series, "1", "/b", 810).unwrap();
        series.retain(Series::kept);
        assert_eq!(series.len(), 1);
        assert!(series[0].progress.is_none());
        restore_progress_entries(&mut series, vec![e1, e2]);
        assert_eq!(series.len(), 2);
        assert_eq!(
            series[0].progress.as_ref().map(|p| p.chapter_url.as_str()),
            Some("/c9")
        );
        assert_eq!(
            series[1].progress.as_ref().map(|p| p.chapter_url.as_str()),
            Some("/c2")
        );
    }
}

#[cfg(test)]
#[test]
fn prefs_read_from_old_files() {
    let d: Data =
        serde_json::from_str(r#"{"series":[],"vertical_reader":true,"host_heap_mb":128}"#).unwrap();
    assert!(d.prefs.vertical_reader && !d.prefs.auto_download);
    assert_eq!(d.prefs.host_heap_mb, Some(128));
    assert!(
        serde_json::to_string(&d)
            .unwrap()
            .contains(r#""vertical_reader":true"#)
    );
}

#[cfg(test)]
#[test]
fn the_old_json_library_moves_into_sqlite_once() {
    let dir = std::env::temp_dir().join(format!("nano-move-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("library.json");
    let mut s = Series::new("1".into(), "S".into(), serde_json::json!({ "url": "/m", "title": "T" }));
    s.in_library = true;
    s.chapters = Arc::new(vec![serde_json::json!({ "url": "/c1" })]);
    let data = Data { series: vec![s], ..Default::default() };
    std::fs::write(&json, serde_json::to_vec(&data).unwrap()).unwrap();

    let store = Store::load(json.clone());
    assert_eq!(store.all_series().len(), 1);
    assert!(dir.join("library.db").exists());
    assert!(!json.exists() && dir.join("library.json.pre-sqlite").exists());

    let again = Store::load(json);
    assert_eq!(again.all_series()[0].chapters.len(), 1);
    assert!(again.all_series()[0].in_library);
}

#[cfg(test)]
#[test]
#[ignore]
fn sqlite_matches_json_on_a_real_library() {
    use std::time::Instant;
    let src: PathBuf = std::env::var_os("NANO_BENCH_FILE").expect("set NANO_BENCH_FILE").into();
    let dir = std::env::temp_dir().join(format!("nano-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("library.json");
    std::fs::copy(&src, &json).unwrap();
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;

    let t = Instant::now();
    let mut expected: Data = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    for s in &mut expected.series {
        s.migrate_read_marks();
    }
    println!("BENCH json_load_ms {:.1}", ms(t));
    let want = serde_json::to_value(&expected.series).unwrap();

    let t = Instant::now();
    let first = Store::load(json.clone());
    println!("BENCH migrate_ms {:.1}", ms(t));
    assert_eq!(serde_json::to_value(first.all_series()).unwrap(), want, "migrated library differs");
    drop(first);

    let t = Instant::now();
    let second = Store::load(json.clone());
    println!("BENCH sqlite_load_ms {:.1}", ms(t));
    assert_eq!(serde_json::to_value(second.all_series()).unwrap(), want, "reloaded library differs");
    drop(second);

    let json_bytes = std::fs::metadata(dir.join("library.json.pre-sqlite")).unwrap().len();
    let db_bytes = std::fs::metadata(dir.join("library.db")).unwrap().len();
    println!("BENCH json_bytes {json_bytes} db_bytes {db_bytes}");

    // Reading on in one series, saved the old way and the new way.
    let mut db = db::Db::new(dir.join("library.db"));
    let mut data = db.load().unwrap().unwrap();
    let p = data.series.iter().position(|s| s.progress.is_some()).unwrap_or(0);
    data.series[p].progress = Some(Progress { chapter_url: "/x".into(), chapter_name: "x".into(), page: 7, pages: 9, at: 1 });
    let t = Instant::now();
    let rows = db.save(data.clone()).unwrap();
    println!("BENCH sqlite_one_change_save_ms {:.2} rows {rows}", ms(t));
    let t = Instant::now();
    let rows = db.save(data.clone()).unwrap();
    println!("BENCH sqlite_no_change_save_ms {:.2} rows {rows}", ms(t));
    let t = Instant::now();
    std::fs::write(dir.join("old.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    println!("BENCH json_save_ms {:.2}", ms(t));
}

#[cfg(test)]
fn migrated(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("nano-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("library.json");
    let mut s = Series::new("1".into(), "S".into(), serde_json::json!({ "url": "/m", "title": "T" }));
    s.in_library = true;
    s.chapters = Arc::new(vec![serde_json::json!({ "url": "/c1" })]);
    std::fs::write(&json, serde_json::to_vec(&Data { series: vec![s], ..Default::default() }).unwrap()).unwrap();
    drop(Store::load(json.clone()));
    (dir, json)
}

#[cfg(test)]
fn wreck(dir: &Path) {
    std::fs::write(dir.join("library.db"), vec![0xAB; 4096]).unwrap();
    let _ = std::fs::remove_file(dir.join("library.db-wal"));
    let _ = std::fs::remove_file(dir.join("library.db-shm"));
}

#[cfg(test)]
#[test]
fn a_broken_database_comes_back_from_the_last_backup() {
    let (dir, json) = migrated("from-backup");
    assert!(dir.join("library.db.bak").exists(), "the first launch keeps a backup");
    // Something done after the upgrade, then another backup, then the database breaks.
    let mut db = db::Db::new(dir.join("library.db"));
    let mut data = db.load().unwrap().unwrap();
    data.series[0].added_at = 42;
    db.save(data).unwrap();
    drop(db);
    db::backup(&dir.join("library.db"), &dir.join("library.db.bak")).unwrap();
    wreck(&dir);

    let store = Store::load(json);
    assert_eq!(store.all_series().len(), 1);
    assert_eq!(store.all_series()[0].added_at, 42, "newer than the copy kept at the upgrade");
    assert!(dir.join("library.db.broken").exists());
}

#[cfg(test)]
#[test]
fn without_a_backup_it_falls_back_to_the_copy_kept_at_the_upgrade() {
    let (dir, json) = migrated("from-snapshot");
    std::fs::remove_file(dir.join("library.db.bak")).unwrap();
    wreck(&dir);

    let store = Store::load(json.clone());
    assert_eq!(store.all_series().len(), 1);
    assert!(store.all_series()[0].in_library);
    assert!(dir.join("library.db.broken").exists());
    assert!(dir.join("library.json.pre-sqlite").exists(), "the snapshot stays for next time");
    assert!(!json.exists());
    drop(store);
    assert_eq!(Store::load(json).all_series().len(), 1, "the rebuilt database loads");
}

#[cfg(test)]
#[test]
fn with_nothing_left_it_starts_empty_instead_of_failing() {
    let (dir, json) = migrated("nothing-left");
    std::fs::remove_file(dir.join("library.db.bak")).unwrap();
    std::fs::remove_file(dir.join("library.json.pre-sqlite")).unwrap();
    wreck(&dir);
    assert!(Store::load(json).all_series().is_empty());
}
