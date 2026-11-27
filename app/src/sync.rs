//! SyncYomi sync. Nano only changes its own fields, so everything else on the server survives.

use crate::store::{Progress, Series, Store, Track, now_ms};
use gpui::{App, Entity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub host: String,
    pub key: String,
    pub device_id: String,
    pub cursor: i64,
    pub v1_etag: String,
}

/// Nano's view of a series as last synced.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Meta {
    pub(crate) sent: Snapshot,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub(crate) struct Snapshot {
    pub(crate) in_library: bool,
    pub(crate) progress: Option<(String, usize, i64)>,
    #[serde(default)]
    pub(crate) marks: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    pub(crate) bookmarks: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) categories: Vec<i64>,
    #[serde(default)]
    pub(crate) tracks: Vec<Track>,
}

impl Snapshot {
    fn of(s: &Series) -> Self {
        Snapshot {
            in_library: s.in_library,
            progress: s.progress.as_ref().map(|p| (p.chapter_url.clone(), p.page, p.at)),
            marks: s.marks.clone(),
            bookmarks: s.bookmarks.clone(),
            categories: s.categories.clone(),
            tracks: s.tracks.clone(),
        }
    }
}

/// Changed here since the last sync.
pub fn unsent(s: &Series) -> bool {
    s.sync.as_ref().is_some_and(|m| m.sent != Snapshot::of(s))
}

fn dirty(s: &Series) -> bool {
    dirty_in(s, &s.sync)
}

fn dirty1(s: &Series) -> bool {
    dirty_in(s, &s.sync1)
}

fn dirty_in(s: &Series, meta: &Option<Meta>) -> bool {
    !s.url().is_empty()
        && match meta {
            Some(m) => m.sent != Snapshot::of(s),
            None => s.in_library || s.progress.is_some() || !s.marks.is_empty() || !s.bookmarks.is_empty() || !s.tracks.is_empty(),
        }
}

// Protobuf wire format, keeping unknown fields.

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Varint(u64),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Fixed32(u32),
}

#[derive(Clone, Default, Debug, PartialEq)]
struct Msg(Vec<(u32, Val)>);

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte < 0x80 {
            return Some(v);
        }
    }
    None
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

impl Msg {
    fn parse(b: &[u8]) -> Option<Msg> {
        let (mut i, mut fields) = (0, vec![]);
        while i < b.len() {
            let tag = varint(b, &mut i)?;
            let val = match tag & 7 {
                0 => Val::Varint(varint(b, &mut i)?),
                1 => {
                    let v = u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?);
                    i += 8;
                    Val::Fixed64(v)
                }
                2 => {
                    let n = varint(b, &mut i)? as usize;
                    let v = b.get(i..i.checked_add(n)?)?.to_vec();
                    i += n;
                    Val::Bytes(v)
                }
                5 => {
                    let v = u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?);
                    i += 4;
                    Val::Fixed32(v)
                }
                _ => return None,
            };
            fields.push(((tag >> 3) as u32, val));
        }
        Some(Msg(fields))
    }

    fn encoded_len(&self) -> usize {
        let varint_len = |x: u64| 1usize + (63 - x.max(1).leading_zeros() as usize) / 7;
        let mut total = 0usize;
        for (n, v) in &self.0 {
            let wire = match v {
                Val::Varint(_) => 0,
                Val::Fixed64(_) => 1,
                Val::Bytes(_) => 2,
                Val::Fixed32(_) => 5,
            };
            total += varint_len(((*n as u64) << 3) | wire);
            total += match v {
                Val::Varint(x) => varint_len(*x),
                Val::Fixed64(_) => 8,
                Val::Bytes(x) => varint_len(x.len() as u64) + x.len(),
                Val::Fixed32(_) => 4,
            };
        }
        total
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        for (n, v) in &self.0 {
            let wire = match v {
                Val::Varint(_) => 0,
                Val::Fixed64(_) => 1,
                Val::Bytes(_) => 2,
                Val::Fixed32(_) => 5,
            };
            put_varint(&mut out, ((*n as u64) << 3) | wire);
            match v {
                Val::Varint(x) => put_varint(&mut out, *x),
                Val::Fixed64(x) => out.extend(x.to_le_bytes()),
                Val::Bytes(x) => {
                    put_varint(&mut out, x.len() as u64);
                    out.extend(x);
                }
                Val::Fixed32(x) => out.extend(x.to_le_bytes()),
            }
        }
        out
    }

    fn get(&self, n: u32) -> Option<&Val> {
        self.0.iter().rev().find(|(k, _)| *k == n).map(|(_, v)| v)
    }

    fn int(&self, n: u32) -> i64 {
        match self.get(n) {
            Some(Val::Varint(v)) => *v as i64,
            _ => 0,
        }
    }

    fn bool(&self, n: u32) -> Option<bool> {
        match self.get(n) {
            Some(Val::Varint(v)) => Some(*v != 0),
            _ => None,
        }
    }

    fn str(&self, n: u32) -> String {
        match self.get(n) {
            Some(Val::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        }
    }

    fn f32(&self, n: u32) -> f32 {
        match self.get(n) {
            Some(Val::Fixed32(v)) => f32::from_bits(*v),
            _ => 0.,
        }
    }

    fn all(&self, n: u32) -> impl Iterator<Item = &[u8]> {
        self.0.iter().filter(move |(k, _)| *k == n).filter_map(|(_, v)| if let Val::Bytes(b) = v { Some(b.as_slice()) } else { None })
    }

    /// A repeated number field, whether written one per entry (Kotlin) or packed (proto3, as the Go server does).
    fn ints(&self, n: u32) -> Vec<i64> {
        let mut out = vec![];
        for (k, v) in &self.0 {
            match v {
                Val::Varint(x) if *k == n => out.push(*x as i64),
                Val::Bytes(b) if *k == n => {
                    let mut i = 0;
                    while i < b.len() {
                        let Some(x) = varint(b, &mut i) else { break };
                        out.push(x as i64);
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn without(&self, ns: &[u32]) -> Msg {
        Msg(self.0.iter().filter(|(k, _)| !ns.contains(k)).cloned().collect())
    }

    fn set(&mut self, n: u32, v: Val) {
        self.0.retain(|(k, _)| *k != n);
        self.0.push((n, v));
    }

    fn set_int(&mut self, n: u32, v: i64) {
        self.set(n, Val::Varint(v as u64));
    }

    fn set_str(&mut self, n: u32, s: &str) {
        self.set(n, Val::Bytes(s.as_bytes().to_vec()));
    }

    fn push(&mut self, n: u32, m: &Msg) {
        self.0.push((n, Val::Bytes(m.encode())));
    }
}

// Field numbers from SyncYomi's backup.proto.
const B_MANGA: u32 = 1;
const B_CATEGORIES: u32 = 2;
const CAT_NAME: u32 = 1;
const CAT_ORDER: u32 = 2;
const CAT_FLAGS: u32 = 100;
const CAT_VERSION: u32 = 601;
const CAT_UID: u32 = 602;
const M_CATEGORIES: u32 = 17;
const B_SOURCES: u32 = 101;
const M_SOURCE: u32 = 1;
const M_URL: u32 = 2;
const M_TITLE: u32 = 3;
const M_ARTIST: u32 = 4;
const M_AUTHOR: u32 = 5;
const M_DESC: u32 = 6;
const M_GENRE: u32 = 7;
const M_STATUS: u32 = 8;
const M_THUMB: u32 = 9;
const M_ADDED: u32 = 13;
const M_CHAPTERS: u32 = 16;
const M_FAVORITE: u32 = 100;
const M_HISTORY: u32 = 104;
const M_MODIFIED: u32 = 106;
const M_VERSION: u32 = 109;
const C_URL: u32 = 1;
const C_NAME: u32 = 2;
const C_SCANLATOR: u32 = 3;
const C_READ: u32 = 4;
const C_BOOKMARK: u32 = 5;
const C_PAGE: u32 = 6;
const C_UPLOAD: u32 = 8;
const C_NUMBER: u32 = 9;
const C_ORDER: u32 = 10;
const C_MODIFIED: u32 = 11;
const C_VERSION: u32 = 12;
const M_TRACKING: u32 = 18;
const T_SYNC_ID: u32 = 1;
const T_LIBRARY_ID: u32 = 2;
const T_MEDIA_ID_INT: u32 = 3;
const T_URL: u32 = 4;
const T_TITLE: u32 = 5;
const T_LAST_READ: u32 = 6;
const T_TOTAL: u32 = 7;
const T_SCORE: u32 = 8;
const T_STATUS: u32 = 9;
const T_STARTED: u32 = 10;
const T_FINISHED: u32 = 11;
const T_PRIVATE: u32 = 12;
const T_MEDIA_ID: u32 = 100;
const H_URL: u32 = 1;
const H_READ_AT: u32 = 2;

/// The server's copy of a series, kept in `sync/` because it's too big for library.json.
#[derive(Clone, Default)]
struct Raw {
    manga: Msg,
    chapters: HashMap<String, Msg>,
}

impl Raw {
    /// A delta may only carry some of the chapters.
    fn update(&mut self, m: &Msg) {
        self.manga = m.without(&[M_CHAPTERS, M_HISTORY]);
        for c in m.all(M_CHAPTERS).filter_map(Msg::parse) {
            self.chapters.insert(c.str(C_URL), c);
        }
    }

    fn path(dir: &Path, source: &str, url: &str) -> PathBuf {
        let h = crate::fnv(&format!("{source}|{url}"));
        dir.join(format!("{h:016x}.pb"))
    }

    fn load(dir: &Path, source: &str, url: &str) -> Option<Raw> {
        let mut raw = Raw::default();
        raw.update(&Msg::parse(&std::fs::read(Raw::path(dir, source, url)).ok()?)?);
        Some(raw)
    }

    fn save(&self, dir: &Path, source: &str, url: &str) {
        let mut m = self.manga.clone();
        for c in self.chapters.values() {
            m.push(M_CHAPTERS, c);
        }
        let path = Raw::path(dir, source, url);
        let tmp = path.with_extension("tmp");
        let result = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&tmp, m.encode())).and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = result {
            eprintln!("sync: save {}: {e}", path.display());
        }
    }
}

fn new_manga(s: &Series) -> Msg {
    let mut m = Msg::default();
    let text = |k: &str| s.manga[k].as_str().unwrap_or("").to_string();
    m.set_int(M_SOURCE, s.source.parse::<u64>().unwrap_or(0) as i64);
    m.set_str(M_URL, s.url());
    m.set_str(M_TITLE, s.title());
    for (n, k) in [(M_ARTIST, "artist"), (M_AUTHOR, "author"), (M_DESC, "description"), (M_THUMB, "thumbnailUrl")] {
        if !text(k).is_empty() {
            m.set_str(n, &text(k));
        }
    }
    for g in text("genre").split(',').map(str::trim).filter(|g| !g.is_empty()) {
        m.0.push((M_GENRE, Val::Bytes(g.as_bytes().to_vec())));
    }
    m.set_int(M_STATUS, s.manga["status"].as_i64().unwrap_or(0));
    m
}

fn new_chapter(c: &Value, order: usize) -> Msg {
    let mut m = Msg::default();
    m.set_str(C_URL, c["url"].as_str().unwrap_or(""));
    m.set_str(C_NAME, c["name"].as_str().unwrap_or(""));
    if let Some(g) = c["scanlator"].as_str().filter(|g| !g.is_empty()) {
        m.set_str(C_SCANLATOR, g);
    }
    m.set_int(C_UPLOAD, c["dateUpload"].as_i64().unwrap_or(0));
    m.set(C_NUMBER, Val::Fixed32((c["chapterNumber"].as_f64().unwrap_or(-1.) as f32).to_bits()));
    m.set_int(C_ORDER, order as i64);
    m
}

/// The series to send, and the server's copy as it will be after.
fn outgoing(s: &Series, raw: Option<Raw>, now: i64) -> (Msg, Raw) {
    outgoing_since(s, s.sync.as_ref().map(|m| &m.sent), raw, now)
}

/// `old` is what the server file last had.
fn outgoing_since(s: &Series, old: Option<&Snapshot>, raw: Option<Raw>, now: i64) -> (Msg, Raw) {
    let secs = now / 1000;
    let mut raw = raw.unwrap_or_else(|| Raw { manga: new_manga(s), chapters: HashMap::new() });
    if old.is_none_or(|o| o.in_library != s.in_library) {
        let m = &mut raw.manga;
        m.set_int(M_FAVORITE, s.in_library as i64);
        if s.in_library && s.added_at > 0 {
            m.set_int(M_ADDED, s.added_at);
        }
        m.set_int(M_VERSION, m.int(M_VERSION) + 1);
        m.set_int(M_MODIFIED, secs);
    }
    if old.is_none_or(|o| o.categories != s.categories) {
        let m = &mut raw.manga;
        *m = m.without(&[M_CATEGORIES]);
        for o in &s.categories {
            m.0.push((M_CATEGORIES, Val::Varint(*o as u64)));
        }
        m.set_int(M_VERSION, m.int(M_VERSION) + 1);
        m.set_int(M_MODIFIED, secs);
    }
    if old.map_or(!s.tracks.is_empty(), |o| o.tracks != s.tracks) {
        let m = &mut raw.manga;
        put_tracks(m, &s.tracks);
        m.set_int(M_VERSION, m.int(M_VERSION) + 1);
        m.set_int(M_MODIFIED, secs);
    }
    let mut out = raw.manga.clone();
    // Same fields as Mihon's backup: read (4), bookmark (5) and last page read (6), each with a version bump.
    let visible = s.visible();
    let read = s.read_flags(&visible);
    let p = s.progress.as_ref();
    let reading = p.and_then(|p| visible.iter().position(|c| c["url"] == p.chapter_url.as_str()));
    let cur_progress = s.progress.as_ref().map(|p| (p.chapter_url.as_str(), p.page, p.at));
    let progress_moved = old.is_none_or(|o| o.progress.as_ref().map(|p| (p.0.as_str(), p.1, p.2)) != cur_progress);
    let mut chapter_order: HashMap<&str, usize> = HashMap::with_capacity(s.chapters.len());
    for (idx, ch) in s.chapters.iter().enumerate() {
        if let Some(u) = ch["url"].as_str() {
            chapter_order.entry(u).or_insert(idx);
        }
    }
    for (i, c) in visible.iter().enumerate() {
        let url = c["url"].as_str().unwrap_or("");
        let current = progress_moved && Some(i) == reading;
        let have = raw.chapters.get(url);
        let (was_read, page, was_marked) = have.map_or((false, 0, false), |m| {
            (
                m.bool(C_READ).unwrap_or(false),
                m.int(C_PAGE),
                m.bool(C_BOOKMARK).unwrap_or(false),
            )
        });
        // Only an explicit unread here clears a read from the phone.
        let unread_here = s.marks.get(url) == Some(&false);
        let send_read = (read[i] && !was_read) || (unread_here && was_read);
        let bookmarked = s.bookmarks.contains(url);
        let page_now = p.map_or(0, |p| p.page as i64);
        // Always send the current chapter once, so history has a chapter to point at.
        let send_page = current && (page != page_now || have.is_none());
        if !send_read && bookmarked == was_marked && !send_page {
            continue;
        }
        let order = chapter_order.get(url).copied().unwrap_or(i);
        let mut ch = have.cloned().unwrap_or_else(|| new_chapter(c, order));
        if send_read {
            ch.set_int(C_READ, read[i] as i64);
        }
        if bookmarked != was_marked {
            ch.set_int(C_BOOKMARK, bookmarked as i64);
        }
        if send_page {
            ch.set_int(C_PAGE, page_now);
        }
        ch.set_int(C_VERSION, ch.int(C_VERSION) + 1);
        ch.set_int(C_MODIFIED, secs);
        out.push(M_CHAPTERS, &ch);
        raw.chapters.insert(url.to_string(), ch);
    }
    if let Some(p) = p.filter(|_| progress_moved) {
        let mut h = Msg::default();
        h.set_str(H_URL, &p.chapter_url);
        h.set_int(H_READ_AT, p.at);
        out.push(M_HISTORY, &h);
    }
    (out, raw)
}

/// Replaces the tracks Nano handles. Trackers it doesn't (Shikimori and the rest) go back as they came.
fn put_tracks(m: &mut Msg, tracks: &[Track]) {
    let entries: Vec<Msg> = m.all(M_TRACKING).filter_map(Msg::parse).collect();
    let mut kept: Vec<Msg> = entries.iter().filter(|e| !crate::track::TRACKERS.contains(&e.int(T_SYNC_ID))).cloned().collect();
    for t in tracks {
        let old = entries.iter().find(|e| e.int(T_SYNC_ID) == t.tracker).cloned().unwrap_or_default();
        kept.push(track_msg(old, t));
    }
    *m = m.without(&[M_TRACKING]);
    for e in &kept {
        m.push(M_TRACKING, e);
    }
}

/// A track over the server's entry for it, so fields Nano doesn't know survive.
fn track_msg(old: Msg, t: &Track) -> Msg {
    let mut e = old.without(&[T_MEDIA_ID_INT]);
    e.set_int(T_SYNC_ID, t.tracker);
    e.set_int(T_LIBRARY_ID, t.library_id);
    e.set_str(T_URL, &t.url);
    e.set_str(T_TITLE, &t.title);
    e.set(T_LAST_READ, Val::Fixed32(t.last_read.to_bits()));
    e.set_int(T_TOTAL, t.total);
    e.set(T_SCORE, Val::Fixed32(t.score.to_bits()));
    e.set_int(T_STATUS, t.status);
    e.set_int(T_STARTED, t.started);
    e.set_int(T_FINISHED, t.finished);
    e.set_int(T_PRIVATE, t.private as i64);
    e.set_int(T_MEDIA_ID, t.media_id);
    e
}

fn track_of(e: &Msg) -> Track {
    // Old backups put the id in field 3.
    let media_id = if e.int(T_MEDIA_ID_INT) != 0 { e.int(T_MEDIA_ID_INT) } else { e.int(T_MEDIA_ID) };
    Track {
        tracker: e.int(T_SYNC_ID),
        library_id: e.int(T_LIBRARY_ID),
        media_id,
        url: e.str(T_URL),
        title: e.str(T_TITLE),
        last_read: e.f32(T_LAST_READ),
        total: e.int(T_TOTAL),
        score: e.f32(T_SCORE),
        status: e.int(T_STATUS),
        started: e.int(T_STARTED),
        finished: e.int(T_FINISHED),
        private: e.bool(T_PRIVATE).unwrap_or(false),
    }
}

/// Takes a server series into Nano. The newer reading position wins.
/// Sync leaves series that are only in history on the server; a restore brings them in with `history`.
fn incoming(
    series: &mut Vec<Series>,
    m: &Msg,
    raw: Option<Raw>,
    source_names: &HashMap<String, String>,
    history: bool,
) -> Option<(usize, Raw)> {
    incoming_from(series, m, raw, source_names, history, false)
}

/// `v1` skips categories and trackers, which only the new protocol carries.
fn incoming_from(
    series: &mut Vec<Series>,
    m: &Msg,
    raw: Option<Raw>,
    source_names: &HashMap<String, String>,
    history: bool,
    v1: bool,
) -> Option<(usize, Raw)> {
    let source = (m.int(M_SOURCE) as u64).to_string();
    let url = m.str(M_URL);
    let favorite = m.bool(M_FAVORITE).unwrap_or(true); // absent is true in Mihon's encoding
    let ix = match series
        .iter()
        .position(|s| s.source == source && s.url() == url)
    {
        Some(ix) => ix,
        None if favorite || (history && m.all(M_HISTORY).next().is_some()) => {
            let genre: Vec<String> = m
                .all(M_GENRE)
                .map(|g| String::from_utf8_lossy(g).into_owned())
                .collect();
            let manga = json!({
                "url": url, "title": m.str(M_TITLE), "artist": m.str(M_ARTIST), "author": m.str(M_AUTHOR),
                "description": m.str(M_DESC), "genre": genre.join(", "), "status": m.int(M_STATUS),
                "thumbnailUrl": m.str(M_THUMB), "initialized": true,
            });
            let name = source_names
                .get(&source)
                .cloned()
                .unwrap_or_else(|| format!("Source {source}"));
            series.push(Series::new(source.clone(), name, manga));
            series.len() - 1
        }
        None => return None,
    };
    let mut raw = raw.unwrap_or_default();
    raw.update(m);
    let s = &mut series[ix];
    if s.chapters.is_empty() {
        let mut list: Vec<&Msg> = raw.chapters.values().collect();
        list.sort_by_key(|c| c.int(C_ORDER));
        s.chapters = Arc::new(
            list.iter()
                .map(|c| json!({ "url": c.str(C_URL), "name": c.str(C_NAME), "scanlator": c.str(C_SCANLATOR), "chapterNumber": c.f32(C_NUMBER), "dateUpload": c.int(C_UPLOAD) }))
                .collect(),
        );
    }
    s.in_library = favorite;
    if favorite && s.added_at == 0 {
        s.added_at = m.int(M_ADDED);
    }
    let last = m
        .all(M_HISTORY)
        .filter_map(Msg::parse)
        .max_by_key(|h| h.int(H_READ_AT));
    let server = last.map(|h| {
        let url = h.str(H_URL);
        let ch = raw.chapters.get(&url);
        let page = ch.map_or(0, |c| c.int(C_PAGE).max(0) as usize);
        let name = ch.map(|c| c.str(C_NAME)).unwrap_or_default();
        Progress {
            chapter_url: url,
            chapter_name: name,
            page,
            pages: page + 1,
            at: h.int(H_READ_AT),
        }
    });
    // Read flags and bookmarks from other devices. Absence never clears one, like the rest of sync.
    let visible = s.visible();
    let derived = s.read_flags(&visible);
    let mut read_by_url: HashMap<&str, bool> = HashMap::with_capacity(visible.len());
    for (i, v) in visible.iter().enumerate() {
        if let Some(u) = v["url"].as_str() {
            read_by_url.entry(u).or_insert(derived[i]);
        }
    }
    for c in m.all(M_CHAPTERS).filter_map(Msg::parse) {
        let url = c.str(C_URL);
        if c.bool(C_BOOKMARK) == Some(true) {
            s.bookmarks.insert(url.clone());
        } else if c.bool(C_BOOKMARK) == Some(false) {
            s.bookmarks.remove(&url);
        }
        let read_here = read_by_url.get(url.as_str()).copied();
        match c.bool(C_READ) {
            Some(true) if read_here != Some(true) => {
                s.marks.insert(url, true);
            }
            Some(false) if s.marks.get(&url) == Some(&true) => {
                s.marks.remove(&url);
            }
            _ => {}
        }
    }
    if !v1 {
        s.categories = m.ints(M_CATEGORIES);
        s.tracks = m.all(M_TRACKING).filter_map(Msg::parse).map(|e| track_of(&e)).filter(|t| crate::track::TRACKERS.contains(&t.tracker) && t.media_id != 0).collect();
    }
    let meta = if v1 { &mut s.sync1 } else { &mut s.sync }.get_or_insert_with(Meta::default);
    meta.sent.in_library = favorite;
    meta.sent.categories = s.categories.clone();
    meta.sent.tracks = s.tracks.clone();
    meta.sent.marks = s.marks.clone();
    meta.sent.bookmarks = s.bookmarks.clone();
    if let Some(p) = server {
        meta.sent.progress = Some((p.chapter_url.clone(), p.page, p.at));
        if p.at > s.history_cleared_at && s.progress.as_ref().is_none_or(|l| p.at >= l.at) {
            s.progress = Some(p);
        }
    }
    Some((ix, raw))
}

fn reply_from(backup: &Msg, cursor: i64, full_requested: bool) -> Reply {
    let sources = backup.all(B_SOURCES).filter_map(Msg::parse).map(|s| ((s.int(2) as u64).to_string(), s.str(1))).collect();
    let mangas = backup.all(B_MANGA).filter_map(Msg::parse).collect();
    let categories = backup
        .all(B_CATEGORIES)
        .filter_map(Msg::parse)
        .map(|c| crate::store::Category { name: c.str(CAT_NAME), order: c.int(CAT_ORDER), flags: c.int(CAT_FLAGS), uid: c.int(CAT_UID), version: c.int(CAT_VERSION) })
        .filter(|c| !c.name.is_empty())
        .collect();
    Reply { mangas, categories, sources, cursor, full_requested }
}

fn decode(bytes: Vec<u8>, cursor: i64, full_requested: bool) -> Result<Reply, String> {
    let backup = Msg::parse(&gunzip(bytes)?).ok_or("The server's backup couldn't be read")?;
    Ok(reply_from(&backup, cursor, full_requested))
}

fn gunzip(b: Vec<u8>) -> Result<Vec<u8>, String> {
    if !b.starts_with(&[0x1f, 0x8b]) {
        return Ok(b);
    }
    let mut out = vec![];
    flate2::read::GzDecoder::new(&b[..]).read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn gzip(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(b).map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
}

struct Reply {
    mangas: Vec<Msg>,
    categories: Vec<crate::store::Category>,
    sources: HashMap<String, String>,
    cursor: i64,
    full_requested: bool,
}

fn status_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(401) => "The server didn't accept the API key".to_string(),
        ureq::Error::StatusCode(404) => "No SyncYomi v2 server at that address".to_string(),
        e => e.to_string(),
    }
}

/// The full library, or None if unchanged since `cursor`. Much faster than a merge.
fn snapshot(cfg: &Config, cursor: i64) -> Result<Option<Reply>, String> {
    let host = cfg.host.trim().trim_end_matches('/');
    let res = ureq::get(format!("{host}/api/sync/v2/snapshot"))
        .header("X-API-Token", cfg.key.trim())
        .header("X-Device-ID", &cfg.device_id)
        .header("X-Device-Name", "Nano")
        .header("X-Sync-Cursor", cursor.to_string())
        .call();
    // A fresh server 404s until the first upload. Treat that as an empty library.
    let mut res = match res {
        Err(ureq::Error::StatusCode(404)) => return Ok(Some(Reply { mangas: vec![], categories: vec![], sources: HashMap::new(), cursor: 0, full_requested: false })),
        r => r.map_err(status_error)?,
    };
    if res.status() == 304 {
        return Ok(None);
    }
    let etag = res.headers().get("ETag").and_then(|v| v.to_str().ok()).unwrap_or("").trim_matches('"').to_string();
    let cursor = etag.strip_prefix("seq=").and_then(|v| v.parse().ok()).ok_or("The server sent no sync cursor")?;
    let bytes = res.body_mut().with_config().limit(512 << 20).read_to_vec().map_err(|e| e.to_string())?;
    decode(bytes, cursor, false).map(Some)
}

fn merge(cfg: &Config, body: &Msg, full: bool) -> Result<Reply, String> {
    let gz = gzip(&body.encode())?;
    let host = cfg.host.trim().trim_end_matches('/');
    let mut res = ureq::post(format!("{host}/api/sync/v2/merge"))
        .header("X-API-Token", cfg.key.trim())
        .header("X-Device-ID", &cfg.device_id)
        .header("X-Device-Name", "Nano")
        .header("X-Sync-Cursor", if full { "0".into() } else { cfg.cursor.to_string() })
        .header("X-Sync-Full", if full { "true" } else { "false" })
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "gzip")
        .send(&gz[..])
        .map_err(status_error)?;
    let header = |k: &str| res.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let cursor = header("X-Sync-Cursor").and_then(|v| v.parse().ok()).ok_or("The server sent no sync cursor")?;
    let full_requested = header("X-Sync-Full-Requested").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let bytes = res.body_mut().with_config().limit(512 << 20).read_to_vec().map_err(|e| e.to_string())?;
    decode(bytes, cursor, full_requested)
}

enum V1Get {
    Missing,
    Unchanged,
    Got(Vec<u8>, String),
}

/// The old protocol keeps the whole library in one ETag-guarded file.
fn v1_get(cfg: &Config, etag: &str) -> Result<V1Get, String> {
    let host = cfg.host.trim().trim_end_matches('/');
    let mut req = ureq::get(format!("{host}/api/sync/content")).header("X-API-Token", cfg.key.trim()).header("X-Device-ID", &cfg.device_id).header("X-Device-Name", "Nano");
    if !etag.is_empty() {
        req = req.header("If-None-Match", etag);
    }
    let mut res = match req.call() {
        Err(ureq::Error::StatusCode(404)) => return Ok(V1Get::Missing),
        r => r.map_err(status_error)?,
    };
    if res.status() == 304 {
        return Ok(V1Get::Unchanged);
    }
    let tag = res.headers().get("ETag").and_then(|v| v.to_str().ok()).ok_or("The server sent no ETag")?.to_string();
    let bytes = res.body_mut().with_config().limit(512 << 20).read_to_vec().map_err(|e| e.to_string())?;
    Ok(V1Get::Got(bytes, tag))
}

/// None if another device changed the file since we read it.
fn v1_put(cfg: &Config, body: &Msg, etag: &str) -> Result<Option<String>, String> {
    let gz = gzip(&body.encode())?;
    let host = cfg.host.trim().trim_end_matches('/');
    let res = ureq::put(format!("{host}/api/sync/content"))
        .header("X-API-Token", cfg.key.trim())
        .header("X-Device-ID", &cfg.device_id)
        .header("X-Device-Name", "Nano")
        .header("If-Match", etag)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "gzip")
        .send(&gz[..]);
    match res {
        Err(ureq::Error::StatusCode(412)) => Ok(None),
        r => {
            let res = r.map_err(status_error)?;
            Ok(Some(res.headers().get("ETag").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()))
        }
    }
}

/// Keeps the old whole-library file in step too, if the server has one.
async fn v1_pass(store: &Entity<Store>, cfg: &mut Config, cx: &mut gpui::AsyncApp) -> Result<usize, String> {
    let dirty = store.read_with(cx, |s, _| s.sync_dirty_v1());
    // With changes to send, fetch the file itself.
    let etag = if dirty { String::new() } else { cfg.v1_etag.clone() };
    let c = cfg.clone();
    enum V1Parsed {
        Skip,
        Got { bytes: Vec<u8>, tag: String, remote: Msg, reply: Reply },
    }
    let parsed = cx
        .background_executor()
        .spawn(async move {
            match v1_get(&c, &etag)? {
                V1Get::Got(bytes, tag) => {
                    let raw = gunzip(bytes.clone())?;
                    let remote = Msg::parse(&raw).ok_or("The server's old library file couldn't be read")?;
                    let reply = reply_from(&remote, 0, false);
                    Ok::<_, String>(V1Parsed::Got { bytes, tag, remote, reply })
                }
                V1Get::Missing | V1Get::Unchanged => Ok(V1Parsed::Skip),
            }
        })
        .await?;
    let V1Parsed::Got { bytes, tag, remote, reply } = parsed else { return Ok(0) };
    let taken = store.update(cx, |s, cx| s.apply_v1(&reply, cx));
    cfg.v1_etag = tag.clone();
    let (body, sent) = store.read_with(cx, |s, _| s.v1_outgoing(&remote));
    if sent.is_empty() {
        store.update(cx, |s, cx| s.set_v1_etag(tag, cx));
        return Ok(taken);
    }
    // The file is replaced whole, so keep the last one we saw.
    let kept = store.read_with(cx, |s, _| s.sync_dir().join("v1-before-upload.bin"));
    let c = cfg.clone();
    let put = cx
        .background_executor()
        .spawn(async move {
            let _ = std::fs::create_dir_all(kept.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(kept, &bytes);
            v1_put(&c, &body, &tag)
        })
        .await?;
    match put {
        Some(new_tag) => {
            cfg.v1_etag = new_tag.clone();
            store.update(cx, |s, cx| {
                s.mark_synced_v1(sent, cx);
                s.set_v1_etag(new_tag, cx);
            });
        }
        // Another device wrote first, so start over from its file next time.
        None => store.update(cx, |s, cx| s.set_v1_etag(String::new(), cx)),
    }
    Ok(taken)
}

/// Uses the snapshot when there's nothing to send, and a merge when there is.
pub fn run(store: Entity<Store>, cx: &mut App) {
    let Some(cfg) = store.update(cx, |s, cx| s.start_sync(cx)) else { return };
    cx.spawn(async move |cx| {
        let mut cfg = cfg;
        let result: Result<usize, String> = async {
            let mut taken = 0;
            let dirty = store.read_with(cx, |s, _| s.sync_dirty());
            if cfg.cursor == 0 || !dirty {
                let (c, cursor) = (cfg.clone(), cfg.cursor);
                if let Some(reply) = cx.background_executor().spawn(async move { snapshot(&c, cursor) }).await? {
                    cfg.cursor = reply.cursor;
                    taken = store.update(cx, |s, cx| s.apply_sync(&reply, reply.cursor, cx));
                }
            }
            if store.read_with(cx, |s, _| s.sync_dirty()) {
                let (body, sent, categories) = store.read_with(cx, |s, _| s.sync_outgoing());
                let reply = cx.background_executor().spawn({ let cfg = cfg.clone(); async move { merge(&cfg, &body, false) } }).await?;
                taken += store.update(cx, |s, cx| {
                    s.mark_synced(sent, cx);
                    s.take_categories(reply.categories.clone(), categories);
                    s.apply_sync(&reply, if reply.full_requested { 0 } else { reply.cursor }, cx)
                });
            }
            Ok(taken + v1_pass(&store, &mut cfg, cx).await?)
        }
        .await;
        store.update(cx, |s, cx| s.finish_sync(result, cx));
    })
    .detach();
}

fn put_sources(body: &mut Msg, sources: HashMap<String, String>) {
    for (id, name) in sources {
        let mut src = Msg::default();
        src.set_str(1, &name);
        src.set_int(2, id.parse::<u64>().unwrap_or(0) as i64);
        body.push(B_SOURCES, &src);
    }
}

fn category_msg(c: &crate::store::Category, version: i64) -> Msg {
    let mut m = Msg::default();
    m.set_str(CAT_NAME, &c.name);
    m.set_int(CAT_ORDER, c.order);
    if c.flags != 0 {
        m.set_int(CAT_FLAGS, c.flags);
    }
    m.set_int(CAT_VERSION, version);
    m.set_int(CAT_UID, c.uid);
    m
}

// Backup files: the same gzipped message as Mihon's .tachibk.

/// One series as Mihon backs it up, with every chapter's read, bookmark and page.
/// Starts from the synced copy when there is one, so fields from the phone survive.
fn backup_manga(s: &Series, raw: Option<Raw>) -> Msg {
    let Raw { manga, chapters } = raw.unwrap_or_else(|| Raw { manga: new_manga(s), chapters: HashMap::new() });
    let mut m = manga.without(&[M_CATEGORIES, M_CHAPTERS, M_HISTORY]);
    m.set_int(M_FAVORITE, s.in_library as i64);
    if s.added_at > 0 {
        m.set_int(M_ADDED, s.added_at);
    }
    for o in &s.categories {
        m.0.push((M_CATEGORIES, Val::Varint(*o as u64)));
    }
    put_tracks(&mut m, &s.tracks);
    let p = s.progress.as_ref();
    if s.chapters.is_empty() {
        // Never opened here, so the synced chapters are all there is.
        for c in chapters.values() {
            m.push(M_CHAPTERS, c);
        }
    }
    let read = s.read_flags(&s.chapters);
    for (i, c) in s.chapters.iter().enumerate() {
        let url = c["url"].as_str().unwrap_or("");
        let mut ch = chapters.get(url).cloned().unwrap_or_else(|| new_chapter(c, i));
        ch.set_int(C_READ, read[i] as i64);
        ch.set_int(C_BOOKMARK, s.bookmarks.contains(url) as i64);
        if let Some(p) = p.filter(|p| p.chapter_url == url) {
            ch.set_int(C_PAGE, p.page as i64);
        }
        m.push(M_CHAPTERS, &ch);
    }
    if let Some(p) = p {
        let mut h = Msg::default();
        h.set_str(H_URL, &p.chapter_url);
        h.set_int(H_READ_AT, p.at);
        m.push(M_HISTORY, &h);
    }
    m
}

/// Takes backup series in. Only adds: a series already here stays in the library and keeps its categories,
/// and sync's own record stays as it was, so restored changes go up on the next sync.
fn restore_series(
    series: &mut Vec<Series>,
    mangas: &[Msg],
    sources: &HashMap<String, String>,
) -> usize {
    let mut taken = 0;
    for m in mangas {
        let (source, url) = ((m.int(M_SOURCE) as u64).to_string(), m.str(M_URL));
        let before = series
            .iter()
            .find(|s| s.source == source && s.url() == url)
            .cloned();
        if let Some((ix, _)) = incoming(series, m, None, sources, true) {
            let s = &mut series[ix];
            if let Some(old) = before {
                s.in_library |= old.in_library;
                let added: Vec<i64> = s
                    .categories
                    .iter()
                    .copied()
                    .filter(|c| !old.categories.contains(c))
                    .collect();
                s.categories = old.categories.into_iter().chain(added).collect();
                // The backup's unread chapters and missing bookmarks don't undo ones made here.
                s.marks
                    .extend(old.marks.into_iter().filter(|(_, read)| *read));
                s.bookmarks.extend(old.bookmarks);
                s.sync = old.sync;
            }
            taken += 1;
        }
    }
    taken
}

impl Store {
    /// The library and history as a Mihon backup file, and how many series it holds.
    pub fn backup(&self) -> Result<(Vec<u8>, usize), String> {
        let dir = self.sync_dir();
        let mut body = Msg::default();
        let mut sources = HashMap::new();
        let mut count = 0;
        for s in self.all_series().iter().filter(|s| !s.url().is_empty() && (s.in_library || s.progress.is_some())) {
            let raw = s.sync.as_ref().and_then(|_| Raw::load(&dir, &s.source, s.url()));
            body.push(B_MANGA, &backup_manga(s, raw));
            sources.insert(s.source.clone(), s.source_name.clone());
            count += 1;
        }
        put_sources(&mut body, sources);
        for c in self.categories() {
            body.push(B_CATEGORIES, &category_msg(&c, c.version));
        }
        Ok((gzip(&body.encode())?, count))
    }

    /// Merges a Mihon or Nano backup into the library. Newer reading progress wins; nothing is removed.
    pub fn restore(
        &mut self,
        bytes: Vec<u8>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<usize, String> {
        let reply =
            decode(bytes, 0, false).map_err(|_| "That file isn't a Mihon backup".to_string())?;
        if reply.mangas.is_empty() {
            return Err("That backup has no series in it".into());
        }
        self.restore_categories(reply.categories.clone());
        let mut taken = 0;
        self.edit_series(cx, |series| {
            taken = restore_series(series, &reply.mangas, &reply.sources)
        });
        Ok(taken)
    }
}

pub struct Sent {
    source: String,
    url: String,
    snapshot: Snapshot,
    raw: Raw,
}

/// The old file with our changes laid into it.
fn v1_merge(series: &[Series], remote: &Msg, now: i64) -> (Msg, Vec<Sent>) {
    let mut body = remote.clone();
    let mut sent = vec![];
    let mut sources: HashMap<String, String> = HashMap::new();
    for s in series.iter().filter(|s| dirty1(s)) {
        let at = body.0.iter().position(|(n, v)| {
            matches!((n, v), (&B_MANGA, Val::Bytes(b)) if Msg::parse(b).is_some_and(|m| (m.int(M_SOURCE) as u64).to_string() == s.source && m.str(M_URL) == s.url()))
        });
        let theirs = at.and_then(|i| match &body.0[i].1 {
            Val::Bytes(b) => Msg::parse(b),
            _ => None,
        });
        let raw = theirs.as_ref().map(|m| {
            let mut raw = Raw::default();
            raw.update(m);
            raw
        });
        let (out, raw) = outgoing_since(s, s.sync1.as_ref().map(|m| &m.sent), raw, now);
        // Their chapters and history stay, ours replace or join them.
        let mut full = raw.manga.clone();
        let mut seen = std::collections::HashSet::new();
        if let Some(t) = &theirs {
            for c in t.all(M_CHAPTERS).filter_map(Msg::parse) {
                let url = c.str(C_URL);
                full.push(M_CHAPTERS, raw.chapters.get(&url).unwrap_or(&c));
                seen.insert(url);
            }
            for h in t.all(M_HISTORY).filter_map(Msg::parse) {
                full.push(M_HISTORY, &h);
            }
        }
        let mut fresh: Vec<&Msg> = raw.chapters.values().filter(|c| !seen.contains(&c.str(C_URL))).collect();
        fresh.sort_by_key(|c| c.int(C_ORDER));
        for c in fresh {
            full.push(M_CHAPTERS, c);
        }
        for h in out.all(M_HISTORY).filter_map(Msg::parse) {
            full.push(M_HISTORY, &h);
        }
        match at {
            Some(i) => body.0[i].1 = Val::Bytes(full.encode()),
            None => body.push(B_MANGA, &full),
        }
        sources.insert(s.source.clone(), s.source_name.clone());
        sent.push(Sent { source: s.source.clone(), url: s.url().to_string(), snapshot: Snapshot::of(s), raw });
    }
    let known: std::collections::HashSet<String> = remote.all(B_SOURCES).filter_map(Msg::parse).map(|m| (m.int(2) as u64).to_string()).collect();
    sources.retain(|id, _| !known.contains(id));
    put_sources(&mut body, sources);
    (body, sent)
}

impl Store {
    fn sync_dirty(&self) -> bool {
        self.categories_changed() || self.all_series().iter().any(dirty)
    }

    fn sync_dir(&self) -> PathBuf {
        self.dir().join("sync")
    }

    fn sync_outgoing(&self) -> (Msg, Vec<Sent>, bool) {
        let (now, dir) = (now_ms(), self.sync_dir());
        let mut body = Msg::default();
        let mut sent = vec![];
        let mut sources: HashMap<String, String> = HashMap::new();
        for s in self.all_series().iter().filter(|s| dirty(s)) {
            let raw = s.sync.as_ref().and_then(|_| Raw::load(&dir, &s.source, s.url()));
            let (m, raw) = outgoing(s, raw, now);
            body.push(B_MANGA, &m);
            sources.insert(s.source.clone(), s.source_name.clone());
            sent.push(Sent { source: s.source.clone(), url: s.url().to_string(), snapshot: Snapshot::of(s), raw });
        }
        put_sources(&mut body, sources);
        let categories = self.categories_changed();
        if categories {
            for c in self.categories() {
                body.push(B_CATEGORIES, &category_msg(&c, c.version + 1));
            }
        }
        (body, sent, categories)
    }

    fn sync_dirty_v1(&self) -> bool {
        self.all_series().iter().any(dirty1)
    }

    fn v1_outgoing(&self, remote: &Msg) -> (Msg, Vec<Sent>) {
        v1_merge(self.all_series(), remote, now_ms())
    }

    fn mark_synced_v1(&mut self, sent: Vec<Sent>, cx: &mut gpui::Context<Self>) {
        self.edit_series(cx, |series| {
            for x in sent {
                if let Some(s) = series.iter_mut().find(|s| s.source == x.source && s.url() == x.url) {
                    s.sync1 = Some(Meta { sent: x.snapshot });
                }
            }
        });
    }

    fn apply_v1(&mut self, reply: &Reply, cx: &mut gpui::Context<Self>) -> usize {
        let mut taken = 0;
        self.edit_series(cx, |series| {
            for m in &reply.mangas {
                if incoming_from(series, m, None, &reply.sources, false, true).is_some() {
                    taken += 1;
                }
            }
        });
        taken
    }

    pub fn mark_synced(&mut self, sent: Vec<Sent>, cx: &mut gpui::Context<Self>) {
        let dir = self.sync_dir();
        for s in &sent {
            s.raw.save(&dir, &s.source, &s.url);
        }
        self.edit_series(cx, |series| {
            for x in sent {
                if let Some(s) = series.iter_mut().find(|s| s.source == x.source && s.url() == x.url) {
                    s.sync = Some(Meta { sent: x.snapshot });
                }
            }
        });
    }

    fn apply_sync(&mut self, reply: &Reply, cursor: i64, cx: &mut gpui::Context<Self>) -> usize {
        self.take_categories(reply.categories.clone(), false);
        let dir = self.sync_dir();
        let mut taken = 0;
        self.edit_series(cx, |series| {
            for m in &reply.mangas {
                let (source, url) = ((m.int(M_SOURCE) as u64).to_string(), m.str(M_URL));
                let known = series.iter().any(|s| s.source == source && s.url() == url);
                let raw = if known { Raw::load(&dir, &source, &url) } else { None };
                if let Some((_, raw)) = incoming(series, m, raw, &reply.sources, false) {
                    raw.save(&dir, &source, &url);
                    taken += 1;
                }
            }
        });
        self.set_sync_cursor(cursor, cx);
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series() -> Series {
        let mut s = Series::new("7537715367149829912".into(), "Comix".into(), json!({ "url": "/m", "title": "T" }));
        s.chapters = Arc::new((0..4).rev().map(|i| json!({ "url": format!("/c{i}"), "name": format!("Ch.{i}") })).collect());
        s
    }

    #[test]
    fn wire_round_trip_keeps_unknown_fields() {
        let mut m = Msg::default();
        m.set_int(1, 7537715367149829912);
        m.set_str(2, "/m");
        m.0.push((18, Val::Bytes(vec![8, 1]))); // a tracker Nano doesn't read
        m.set(9000, Val::Fixed64(42));
        let back = Msg::parse(&m.encode()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.int(1), 7537715367149829912);
    }

    #[test]
    fn server_progress_comes_in_and_local_changes_go_out_on_its_bytes() {
        // The phone's copy: in the library, reading /c1 at page 3, /c0 read, plus a tracker.
        let mut ch0 = Msg::default();
        ch0.set_str(C_URL, "/c0");
        ch0.set_int(C_READ, 1);
        ch0.set_int(C_VERSION, 5);
        let mut ch1 = Msg::default();
        ch1.set_str(C_URL, "/c1");
        ch1.set_str(C_NAME, "Ch.1");
        ch1.set_int(C_PAGE, 3);
        ch1.set_int(5, 1); // bookmarked
        ch1.set_int(C_VERSION, 2);
        let mut h = Msg::default();
        h.set_str(H_URL, "/c1");
        h.set_int(H_READ_AT, 1000);
        let mut m = new_manga(&series());
        m.set_int(M_VERSION, 9);
        m.0.push((18, Val::Bytes(vec![8, 1])));
        m.push(M_CHAPTERS, &ch0);
        m.push(M_CHAPTERS, &ch1);
        m.push(M_HISTORY, &h);

        let mut list = vec![];
        let (_, raw) = incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let s = &mut list[0];
        assert!(s.in_library);
        assert_eq!(s.progress.as_ref().map(|p| (p.chapter_url.as_str(), p.page)), Some(("/c1", 3)));
        assert!(!dirty(s), "nothing changed here");

        // Read on into /c2 here.
        s.chapters = series().chapters;
        s.mark(&["/c1".into()], true);
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "Ch.2".into(), page: 1, pages: 10, at: 4000 });
        assert!(dirty(s));
        let (out, _) = outgoing(s, Some(raw), 5000);
        assert_eq!(out.int(M_VERSION), 9, "the series itself didn't change");
        assert!(out.all(18).next().is_some(), "tracker kept");
        let chs: Vec<Msg> = out.all(M_CHAPTERS).filter_map(Msg::parse).collect();
        let by = |u: &str| chs.iter().find(|c| c.str(C_URL) == u).cloned();
        let c1 = by("/c1").expect("/c1 now read");
        assert_eq!((c1.bool(C_READ), c1.int(5), c1.int(C_VERSION)), (Some(true), 1, 3), "bookmark kept, version bumped");
        assert_eq!(by("/c2").map(|c| c.int(C_PAGE)), Some(1));
        assert!(by("/c0").is_none(), "already read there");
        assert!(by("/c3").is_none());
        let hist = Msg::parse(out.all(M_HISTORY).next().unwrap()).unwrap();
        assert_eq!((hist.str(H_URL), hist.int(H_READ_AT)), ("/c2".into(), 4000));
    }

    #[test]
    fn clearing_history_blocks_older_server_progress_and_allows_newer() {
        let mut h = Msg::default();
        h.set_str(H_URL, "/c1");
        h.set_int(H_READ_AT, 1000);
        let mut m = new_manga(&series());
        m.push(M_HISTORY, &h);

        let mut list = vec![];
        incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        assert!(list[0].progress.is_some());

        crate::store::clear_progress_for(&mut list, "7537715367149829912", "/m", 2000);
        assert!(list[0].progress.is_none());

        // Older server history does not resurrect cleared progress.
        incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        assert!(list[0].progress.is_none());

        // Newer progress after the clear comes in normally.
        let mut newer_h = Msg::default();
        newer_h.set_str(H_URL, "/c2");
        newer_h.set_int(H_READ_AT, 3000);
        let mut newer_m = new_manga(&series());
        newer_m.push(M_HISTORY, &newer_h);
        incoming(&mut list, &newer_m, None, &HashMap::new(), false).unwrap();
        assert_eq!(
            list[0].progress.as_ref().map(|p| p.chapter_url.as_str()),
            Some("/c2")
        );
    }

    #[test]
    fn clear_then_sync_then_undo_restores_progress_and_tombstone() {
        let mut h = Msg::default();
        h.set_str(H_URL, "/c1");
        h.set_int(H_READ_AT, 1000);
        let mut m = new_manga(&series());
        m.push(M_HISTORY, &h);

        let mut list = vec![];
        incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let cleared =
            crate::store::clear_progress_for(&mut list, "7537715367149829912", "/m", 2000).unwrap();

        // Sync runs while cleared: old server progress stays blocked.
        incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        assert!(list[0].progress.is_none());
        assert_eq!(list[0].history_cleared_at, 2000);

        // Undo restores the pre-clear progress and pre-clear tombstone.
        crate::store::restore_progress_entries(&mut list, vec![cleared]);
        assert_eq!(
            list[0]
                .progress
                .as_ref()
                .map(|p| (p.chapter_url.as_str(), p.at)),
            Some(("/c1", 1000))
        );
        assert_eq!(list[0].history_cleared_at, 0);

        // Progress newer than the restored 1000 (still below the old 2000 clear) is accepted again.
        let mut mid_h = Msg::default();
        mid_h.set_str(H_URL, "/c2");
        mid_h.set_int(H_READ_AT, 1500);
        let mut mid_m = new_manga(&series());
        mid_m.push(M_HISTORY, &mid_h);
        incoming(&mut list, &mid_m, None, &HashMap::new(), false).unwrap();
        assert_eq!(
            list[0]
                .progress
                .as_ref()
                .map(|p| (p.chapter_url.as_str(), p.at)),
            Some(("/c2", 1500))
        );
    }

    #[test]
    fn tracks_use_mihons_tracking_field() {
        // The phone tracks it on MyAnimeList (with a field Nano doesn't know) and on Shikimori.
        let mut mal = Msg::default();
        mal.set_int(T_SYNC_ID, 1);
        mal.set_int(T_MEDIA_ID, 2);
        mal.set(T_LAST_READ, Val::Fixed32(4f32.to_bits()));
        mal.set_int(T_STATUS, 1);
        mal.set_int(500, 7);
        let mut shiki = Msg::default();
        shiki.set_int(T_SYNC_ID, 4);
        shiki.set_int(T_MEDIA_ID, 9);
        let mut m = new_manga(&series());
        m.set_int(M_VERSION, 3);
        m.push(M_TRACKING, &mal);
        m.push(M_TRACKING, &shiki);
        let mut list = vec![];
        let (_, raw) = incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let s = &mut list[0];
        assert_eq!(s.tracks.len(), 1, "only the trackers Nano handles");
        assert_eq!((s.tracks[0].media_id, s.tracks[0].last_read), (2, 4.));
        assert!(!dirty(s));

        s.tracks[0].last_read = 6.;
        s.tracks.push(Track { tracker: 2, media_id: 30, status: 5, ..Track::default() });
        let (out, _) = outgoing(s, Some(raw), 5000);
        assert_eq!(out.int(M_VERSION), 4);
        let sent: Vec<Msg> = out.all(M_TRACKING).filter_map(Msg::parse).collect();
        let by = |id: i64| sent.iter().find(|e| e.int(T_SYNC_ID) == id).cloned().unwrap();
        assert_eq!(sent.len(), 3);
        assert_eq!((by(1).f32(T_LAST_READ), by(1).int(500)), (6., 7), "unknown field kept");
        assert_eq!(by(4), shiki);
        assert_eq!((by(2).int(T_MEDIA_ID), by(2).int(T_STATUS)), (30, 5));
    }

    #[test]
    fn marks_and_bookmarks_use_mihons_chapter_fields() {
        // The phone read /c3 and bookmarked /c2, and we're reading /c1.
        let mut ch3 = Msg::default();
        ch3.set_str(C_URL, "/c3");
        ch3.set_int(C_READ, 1);
        let mut ch2 = Msg::default();
        ch2.set_str(C_URL, "/c2");
        ch2.set_int(C_BOOKMARK, 1);
        let mut m = new_manga(&series());
        m.push(M_CHAPTERS, &ch3);
        m.push(M_CHAPTERS, &ch2);
        let mut list = vec![series()];
        list[0].in_library = true;
        list[0].progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "".into(), page: 0, pages: 5, at: 1 });
        let (_, raw) = incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let s = &mut list[0];
        assert_eq!(s.marks.get("/c3"), Some(&true), "read on the phone, so read here too");
        assert!(s.bookmarks.contains("/c2"));

        // Here: unread /c3 again, bookmark /c0, unbookmark /c2.
        s.mark(&["/c3".into()], false);
        s.mark(&["/c0".into()], true);
        s.toggle_bookmark("/c0");
        s.toggle_bookmark("/c2");
        let (out, _) = outgoing(s, Some(raw), 9000);
        let chs: Vec<Msg> = out.all(M_CHAPTERS).filter_map(Msg::parse).collect();
        let by = |u: &str| chs.iter().find(|c| c.str(C_URL) == u).cloned();
        assert_eq!(by("/c3").map(|c| (c.bool(C_READ), c.int(C_VERSION))), Some((Some(false), 1)));
        assert_eq!(by("/c0").and_then(|c| c.bool(C_BOOKMARK)), Some(true));
        assert_eq!(by("/c2").and_then(|c| c.bool(C_BOOKMARK)), Some(false));
        // /c0 is before the current chapter, so it goes up as read too, as Mihon would show it.
        assert_eq!(by("/c0").and_then(|c| c.bool(C_READ)), Some(true));
    }

    #[test]
    fn categories_read_packed_or_not_and_go_out_one_per_entry() {
        let mut m = new_manga(&series());
        m.0.push((M_CATEGORIES, Val::Varint(2)));
        m.0.push((M_CATEGORIES, Val::Bytes(vec![5, 7]))); // packed, as a Go server writes it
        assert_eq!(m.ints(M_CATEGORIES), [2, 5, 7]);

        let mut list = vec![];
        let (_, raw) = incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let s = &mut list[0];
        assert_eq!(s.categories, [2, 5, 7]);
        assert!(!dirty(s));
        s.categories = vec![3];
        assert!(dirty(s));
        let (out, _) = outgoing(s, Some(raw), 1000);
        assert_eq!(out.ints(M_CATEGORIES), [3]);
        assert!(out.0.iter().all(|(k, v)| *k != M_CATEGORIES || matches!(v, Val::Varint(_))), "Mihon's unpacked form");
    }

    #[test]
    fn backups_carry_every_chapter_and_restore_to_the_same_series() {
        let mut s = series();
        s.in_library = true;
        s.added_at = 1234;
        s.categories = vec![2];
        s.progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "Ch.1".into(), page: 4, pages: 9, at: 777 });
        s.mark(&["/c0".into()], true);
        s.bookmarks.insert("/c3".into());
        s.tracks.push(Track { tracker: 2, media_id: 30, status: 1, ..Track::default() });
        let m = backup_manga(&s, None);
        let chs: Vec<Msg> = m.all(M_CHAPTERS).filter_map(Msg::parse).collect();
        assert_eq!(chs.len(), 4, "unread chapters too, as Mihon expects");
        let by = |u: &str| chs.iter().find(|c| c.str(C_URL) == u).cloned().unwrap();
        assert_eq!((by("/c0").bool(C_READ), by("/c3").bool(C_READ)), (Some(true), Some(false)));
        assert_eq!(by("/c1").int(C_PAGE), 4);

        let mut list = vec![];
        incoming(&mut list, &m, None, &HashMap::new(), false).unwrap();
        let back = &list[0];
        assert!(back.in_library);
        assert_eq!((back.added_at, back.categories.clone()), (1234, vec![2]));
        assert_eq!(back.progress.as_ref().map(|p| (p.chapter_url.as_str(), p.page, p.at)), Some(("/c1", 4, 777)));
        assert!(back.bookmarks.contains("/c3"));
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(back.chapters.len(), 4);
    }

    #[test]
    fn restoring_an_old_backup_removes_nothing() {
        // The backup has the series out of the library, in category 1.
        let mut old = series();
        old.categories = vec![1];
        old.progress = Some(Progress { chapter_url: "/c3".into(), chapter_name: "".into(), page: 0, pages: 5, at: 10 });
        let m = backup_manga(&old, None);
        // Here it's in the library, in category 2, read further.
        let mut here = series();
        here.in_library = true;
        here.categories = vec![2];
        here.progress = Some(Progress { chapter_url: "/c1".into(), chapter_name: "".into(), page: 2, pages: 5, at: 99 });
        here.marks.insert("/c0".into(), true);
        here.bookmarks.insert("/c2".into());
        let mut list = vec![here];
        assert_eq!(restore_series(&mut list, &[m], &HashMap::new()), 1);
        assert!(list[0].in_library);
        assert_eq!(list[0].categories, [2, 1]);
        assert_eq!(list[0].progress.as_ref().map(|p| p.chapter_url.as_str()), Some("/c1"), "newer progress kept");
        assert_eq!(list[0].marks.get("/c0"), Some(&true), "read here stays read");
        assert!(list[0].bookmarks.contains("/c2"));
        assert!(list[0].sync.is_none(), "still unsynced, so it goes up next sync");
    }

    #[test]
    fn history_only_series_stay_on_the_server() {
        let mut m = new_manga(&series());
        m.set_int(M_FAVORITE, 0);
        let mut h = Msg::default();
        h.set_str(H_URL, "/c1");
        h.set_int(H_READ_AT, 1000);
        m.push(M_HISTORY, &h);
        let mut list = vec![];
        assert!(incoming(&mut list, &m, None, &HashMap::new(), false).is_none());
        assert!(list.is_empty());
        let (ix, _) =
            incoming(&mut list, &m, None, &HashMap::new(), true).expect("a restore keeps history");
        assert!(!list[ix].in_library && list[ix].progress.is_some());
    }
    #[test]
    fn the_old_file_keeps_everything_else_and_takes_our_reads() {
        // The phone's file has /m with /c0 read, another series, and an unknown top-level field.
        let mut c0 = Msg::default();
        c0.set_str(C_URL, "/c0");
        c0.set_int(C_READ, 1);
        let mut c9 = Msg::default();
        c9.set_str(C_URL, "/c9");
        let mut phone = new_manga(&series());
        phone.set_int(M_VERSION, 4);
        phone.push(M_CHAPTERS, &c0);
        phone.push(M_CHAPTERS, &c9);
        let mut other = Msg::default();
        other.set_int(M_SOURCE, 1);
        other.set_str(M_URL, "/other");
        let mut remote = Msg::default();
        remote.push(B_MANGA, &phone);
        remote.push(B_MANGA, &other);
        remote.0.push((105, Val::Bytes(vec![1, 2, 3])));

        let mut list = vec![];
        incoming_from(&mut list, &phone, None, &HashMap::new(), false, true).unwrap();
        let s = &mut list[0];
        assert!(!dirty1(s), "just came from there");
        s.chapters = series().chapters;
        s.mark(&["/c1".into()], true);
        s.progress = Some(Progress { chapter_url: "/c2".into(), chapter_name: "Ch.2".into(), page: 1, pages: 10, at: 4000 });
        assert!(dirty1(s));

        let (body, sent) = v1_merge(&list, &remote, 5000);
        assert_eq!(sent.len(), 1);
        assert_eq!(body.all(B_MANGA).count(), 2, "their other series stays");
        assert_eq!(body.all(105).next(), Some(&[1u8, 2, 3][..]), "unknown top-level field stays");
        let mine = body.all(B_MANGA).filter_map(Msg::parse).find(|m| m.str(M_URL) == "/m").unwrap();
        let urls: Vec<String> = mine.all(M_CHAPTERS).filter_map(Msg::parse).map(|c| c.str(C_URL)).collect();
        assert!(urls.contains(&"/c0".to_string()) && urls.contains(&"/c9".to_string()), "their chapters stay: {urls:?}");
        let c1 = mine.all(M_CHAPTERS).filter_map(Msg::parse).find(|c| c.str(C_URL) == "/c1").unwrap();
        assert_eq!(c1.bool(C_READ), Some(true), "read before /c2");
        assert_eq!(mine.all(M_HISTORY).count(), 1);
    }

}
