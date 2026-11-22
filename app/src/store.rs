//! What Nano remembers between runs, in one JSON file saved a second after each change.

use gpui::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone)]
pub struct Series {
    pub source: String,
    pub source_name: String,
    pub manga: Value,
    pub in_library: bool,
    pub added_at: i64,
    pub chapters: Vec<Value>,
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
}

/// A series on MyAnimeList or AniList. Mihon's BackupTracking fields, so sync carries them unchanged.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Track {
    /// Mihon's tracker id: 1 MyAnimeList, 2 AniList.
    pub tracker: i64,
    /// AniList's list entry id. MyAnimeList has none.
    pub library_id: i64,
    pub media_id: i64,
    pub url: String,
    pub title: String,
    pub last_read: f32,
    pub total: i64,
    /// 0 to 10 on MyAnimeList, 0 to 100 on AniList.
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

#[derive(Serialize, Deserialize, Clone)]
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
            chapters: vec![],
            progress: None,
            hidden_scanlators: vec![],
            scanlators_chosen: false,
            sync: None,
            sync1: None,
            marks: Default::default(),
            bookmarks: Default::default(),
            categories: vec![],
            tracks: vec![],
        }
    }

    /// In the library, read, or with an unsynced change.
    fn kept(&self) -> bool {
        self.in_library || self.progress.is_some() || !self.marks.is_empty() || !self.bookmarks.is_empty() || crate::sync::unsent(self)
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

    /// How far through the series you are, from 0 to 1. The current chapter counts by page.
    pub fn read_fraction(&self) -> Option<f32> {
        let p = self.progress.as_ref()?;
        let total = self.visible().len();
        let i = self.reading_index()?;
        let page = if p.pages == 0 { 0. } else { ((p.page + 1) as f32 / p.pages as f32).min(1.) };
        Some(((total - i - 1) as f32 + page) / total as f32)
    }

    /// Whether each chapter in `list` (newest first) is read: older than the one being read, finished, or marked by hand.
    pub fn read_flags(&self, list: &[Value]) -> Vec<bool> {
        let p = self.progress.as_ref();
        let reading = p.and_then(|p| list.iter().position(|c| c["url"] == p.chapter_url.as_str()));
        let finished = p.is_some_and(|p| p.pages > 0 && p.page + 1 >= p.pages);
        list.iter()
            .enumerate()
            .map(|(i, c)| match self.marks.get(c["url"].as_str().unwrap_or_default()) {
                Some(read) => *read,
                None => reading.is_some_and(|r| i > r || (i == r && finished)),
            })
            .collect()
    }

    /// Whether each chapter counts as new: newer than the one being read, or marked unread.
    /// An unstarted series has nothing new, so its whole list isn't a backlog.
    pub fn new_flags(&self, list: &[Value]) -> Vec<bool> {
        let reading = self.progress.as_ref().and_then(|p| list.iter().position(|c| c["url"] == p.chapter_url.as_str()));
        list.iter()
            .enumerate()
            .map(|(i, c)| match self.marks.get(c["url"].as_str().unwrap_or_default()) {
                Some(read) => !read,
                None => reading.is_some_and(|r| i < r),
            })
            .collect()
    }

    pub fn caught_up(&self) -> bool {
        let list = self.visible();
        (self.progress.is_some() || !self.marks.is_empty()) && !list.is_empty() && self.read_flags(&list).iter().all(|r| *r)
    }

    pub fn unread(&self) -> usize {
        self.new_flags(&self.visible()).iter().filter(|n| **n).count()
    }

    /// Marks chapters read or unread by hand.
    pub fn mark(&mut self, urls: &[String], read: bool) {
        for url in urls {
            self.marks.insert(url.clone(), read);
        }
    }

    /// The highest chapter number read, which is what trackers count. Falls back to how many chapters are read.
    pub fn read_number(&self) -> f32 {
        let mut list = self.visible();
        if self.progress.as_ref().is_some_and(|p| !list.iter().any(|c| c["url"] == p.chapter_url.as_str())) {
            list = self.chapters.clone();
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
#[derive(Serialize, Deserialize, Default)]
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
}

#[derive(Serialize, Deserialize, Default)]
struct Data {
    series: Vec<Series>,
    #[serde(default)]
    languages: std::collections::HashMap<String, Vec<String>>,
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
    data: Data,
    saving: bool,
    pub sync_status: SyncStatus,
    /// The last tracker update that failed, shown on the series page.
    pub track_error: Option<String>,
    pub update: Option<crate::update::Update>,
    pub update_note: &'static str,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| eprintln!("{}: {e}", path.display())).ok())
            .unwrap_or_default();
        Store { path, data, saving: false, sync_status: SyncStatus::Idle, track_error: None, update: None, update_note: "" }
    }

    pub fn get(&self, source: &str, url: &str) -> Option<&Series> {
        self.data.series.iter().find(|s| s.source == source && s.url() == url)
    }

    pub fn library(&self) -> Vec<&Series> {
        let mut list: Vec<_> = self.data.series.iter().filter(|s| s.in_library).collect();
        list.sort_by_key(|s| -s.last_active());
        list
    }

    pub fn history(&self) -> Vec<&Series> {
        let mut list: Vec<_> = self.data.series.iter().filter(|s| s.progress.is_some()).collect();
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
        let out = f(&mut self.data.series[ix]);
        let s = &mut self.data.series[ix];
        let after = s.read_number();
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
        self.data.series.retain(Series::kept);
        self.save_soon(cx);
        cx.notify();
        out
    }

    pub fn dir(&self) -> std::path::PathBuf {
        self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    }

    pub fn all_series(&self) -> &[Series] {
        &self.data.series
    }

    pub fn edit_series(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Vec<Series>)) {
        f(&mut self.data.series);
        self.data.series.retain(Series::kept);
        self.save_soon(cx);
        cx.notify();
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
        self.edit_series(cx, |series| series.iter_mut().for_each(|s| (s.sync, s.sync1) = (None, None)));
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
        if let Some(c) = self.data.categories.iter().find(|c| c.order == order) {
            self.data.deleted_categories.push(c.name.to_lowercase());
        }
        self.data.categories.retain(|c| c.order != order);
        for s in &mut self.data.series {
            s.categories.retain(|o| *o != order);
        }
        self.data.categories_changed = true;
        self.save_soon(cx);
        cx.notify();
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

    pub fn set_login(&mut self, tracker: i64, login: Option<crate::track::Login>, cx: &mut Context<Self>) {
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
            if let Some(s) = series.iter_mut().find(|s| s.source == key.0 && s.url() == key.1) {
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

    fn save_soon(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok((path, bytes)) = this.update(cx, |s, _| {
                s.saving = false;
                (s.path.clone(), serde_json::to_vec(&s.data).unwrap_or_default())
            }) else {
                return;
            };
            let tmp = path.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)) {
                eprintln!("save {}: {e}", path.display());
            }
        })
        .detach();
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
        s.chapters = (0..5).rev().map(|i| json!({ "url": format!("/c{i}"), "chapterNumber": -1.0 })).collect();
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "".into(), page: 0, pages: 2, at: 0 });
        assert_eq!(s.read_number(), 2.);
        s.chapters[3] = json!({ "url": "/c1", "chapterNumber": -1.0, "name": "Chapter 7" });
        assert_eq!(s.read_number(), 7.);
        s.progress = Some(Progress { chapter_url: "/gone".into(), chapter_name: "Ch. 9".into(), page: 1, pages: 2, at: 0 });
        assert_eq!(s.read_number(), 9.);
    }

    #[test]
    fn unread_counts_chapters_newer_than_the_one_being_read() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.in_library = true;
        s.chapters = (0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect();
        assert_eq!(s.unread(), 0);
        s.progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "".into(), page: 0, pages: 1, at: 0 });
        assert_eq!(s.unread(), 3); // c4, c3, c2
        s.progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "".into(), page: 0, pages: 2, at: 0 });
        assert_eq!(s.read_fraction(), Some(0.3));
        assert!(!s.caught_up());
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
    fn marks_override_what_progress_implies() {
        let mut s = Series::new("1".into(), "S".into(), json!({ "url": "/m" }));
        s.chapters = (0..5).rev().map(|i| json!({ "url": format!("/c{i}") })).collect();
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "".into(), page: 0, pages: 4, at: 0 });
        assert_eq!(s.read_flags(&s.chapters), [false, false, false, true, true]); // c4 c3 c2 c1 c0
        assert_eq!(s.unread(), 2);
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
    fn default_filter_keeps_the_main_or_current_group() {
        let ch = |url: &str, g: &str| json!({ "url": url, "scanlator": g });
        let chapters = vec![ch("/1", "A"), ch("/2", "B"), ch("/3", "B"), ch("/4", "")];
        assert_eq!(default_hidden(&chapters, None), vec!["A".to_string(), "Unknown".to_string()]);
        assert_eq!(default_hidden(&chapters, Some("/1")), vec!["B".to_string(), "Unknown".to_string()]);
        assert!(default_hidden(&[], None).is_empty());
    }
}

#[cfg(test)]
#[test]
fn prefs_read_from_old_files() {
    let d: Data = serde_json::from_str(r#"{"series":[],"vertical_reader":true,"host_heap_mb":128}"#).unwrap();
    assert!(d.prefs.vertical_reader && !d.prefs.auto_download);
    assert_eq!(d.prefs.host_heap_mb, Some(128));
    assert!(serde_json::to_string(&d).unwrap().contains(r#""vertical_reader":true"#));
}
