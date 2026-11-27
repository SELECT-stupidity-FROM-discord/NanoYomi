//! The light novel side's own library, history and updates, kept apart from manga in ln/library.json.

use crate::store::now_ms;
use gpui::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Chapter {
    pub name: String,
    pub path: String,
    /// As the plugin wrote it, so it stays whatever format the site uses.
    pub release: String,
    pub number: Option<f64>,
    /// Which page of the chapter list it came from, for plugins that page them.
    pub page: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Progress {
    pub chapter: String,
    pub name: String,
    /// How far down the chapter you got, from 0 to 1.
    pub scroll: f32,
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Novel {
    pub source: String,
    pub source_name: String,
    pub path: String,
    pub name: String,
    pub cover: String,
    pub summary: String,
    pub author: String,
    pub status: String,
    pub genres: String,
    pub in_library: bool,
    pub added_at: i64,
    pub chapters: Vec<Chapter>,
    /// Chapters finished, by path.
    pub read: BTreeSet<String>,
    pub progress: Option<Progress>,
}

impl Novel {
    pub fn unread(&self) -> usize {
        self.chapters.iter().filter(|c| !self.read.contains(&c.path)).count()
    }

    /// The chapter to open: the one in progress, else the first unread, else the first.
    pub fn resume_index(&self) -> usize {
        let by_progress = self.progress.as_ref().and_then(|p| self.chapters.iter().position(|c| c.path == p.chapter));
        by_progress.or_else(|| self.chapters.iter().position(|c| !self.read.contains(&c.path))).unwrap_or(0)
    }

    /// Kept while it's in the library or has been read.
    fn kept(&self) -> bool {
        self.in_library || self.progress.is_some() || !self.read.is_empty()
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Update {
    pub source: String,
    pub path: String,
    pub novel: String,
    pub chapter: String,
    pub chapter_path: String,
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Prefs {
    pub font_size: f32,
    pub line_height: f32,
    pub width: f32,
    pub oldest_first: bool,
    pub repos: Vec<String>,
    /// Source ids in the order the sidebar shows them.
    pub source_order: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { font_size: 19., line_height: 1.7, width: 720., oldest_first: false, repos: vec![super::plugins::DEFAULT_REPO.into()], source_order: vec![] }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Data {
    novels: Vec<Novel>,
    updates: Vec<Update>,
    prefs: Prefs,
}

const MAX_UPDATES: usize = 300;

pub struct LnStore {
    path: PathBuf,
    data: Data,
    saving: bool,
}

impl LnStore {
    pub fn load(path: PathBuf) -> Self {
        let mut data: Data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| eprintln!("{}: {e}", path.display())).ok())
            .unwrap_or_default();
        data.novels.retain(Novel::kept);
        if data.prefs.repos.is_empty() {
            data.prefs.repos = Prefs::default().repos;
        }
        LnStore { path, data, saving: false }
    }

    pub fn prefs(&self) -> &Prefs {
        &self.data.prefs
    }

    pub fn edit_prefs(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Prefs)) {
        f(&mut self.data.prefs);
        self.changed(cx);
    }

    pub fn get(&self, source: &str, path: &str) -> Option<&Novel> {
        self.data.novels.iter().find(|n| n.source == source && n.path == path)
    }

    pub fn library(&self) -> Vec<&Novel> {
        let mut list: Vec<&Novel> = self.data.novels.iter().filter(|n| n.in_library).collect();
        list.sort_by_key(|n| std::cmp::Reverse(n.progress.as_ref().map_or(n.added_at, |p| p.at.max(n.added_at))));
        list
    }

    pub fn history(&self) -> Vec<&Novel> {
        let mut list: Vec<&Novel> = self.data.novels.iter().filter(|n| n.progress.is_some()).collect();
        list.sort_by_key(|n| std::cmp::Reverse(n.progress.as_ref().map_or(0, |p| p.at)));
        list
    }

    pub fn updates(&self) -> &[Update] {
        &self.data.updates
    }

    /// Saves what the plugin knows about a novel. Your library flag, reads and progress stay.
    pub fn remember(&mut self, mut novel: Novel, cx: &mut Context<Self>) {
        match self.data.novels.iter_mut().find(|n| n.source == novel.source && n.path == novel.path) {
            Some(old) => {
                novel.in_library = old.in_library;
                novel.added_at = old.added_at;
                novel.read = std::mem::take(&mut old.read);
                novel.progress = old.progress.take();
                if novel.chapters.is_empty() {
                    novel.chapters = std::mem::take(&mut old.chapters);
                }
                *old = novel;
            }
            None => self.data.novels.push(novel),
        }
        self.changed(cx);
    }

    /// Changes a stored novel. Nothing happens for one that isn't stored.
    pub fn edit(&mut self, source: &str, path: &str, cx: &mut Context<Self>, f: impl FnOnce(&mut Novel)) {
        if let Some(n) = self.data.novels.iter_mut().find(|n| n.source == source && n.path == path) {
            f(n);
            self.data.novels.retain(Novel::kept);
            self.changed(cx);
        }
    }

    pub fn set_in_library(&mut self, source: &str, path: &str, on: bool, cx: &mut Context<Self>) {
        self.edit(source, path, cx, |n| {
            n.in_library = on;
            n.added_at = if on { now_ms() } else { 0 };
        });
    }

    /// Notes chapters the library hasn't seen. Returns how many were new.
    pub fn found_chapters(&mut self, source: &str, path: &str, chapters: Vec<Chapter>, cx: &mut Context<Self>) -> usize {
        let Some(n) = self.data.novels.iter_mut().find(|n| n.source == source && n.path == path) else { return 0 };
        let known: BTreeSet<&str> = n.chapters.iter().map(|c| c.path.as_str()).collect();
        let fresh: Vec<Chapter> = chapters.iter().filter(|c| !known.contains(c.path.as_str())).cloned().collect();
        // The first check only learns the list.
        let first = n.chapters.is_empty();
        n.chapters = chapters;
        if !first && n.in_library {
            let (novel, source, path) = (n.name.clone(), n.source.clone(), n.path.clone());
            for c in &fresh {
                self.data.updates.insert(0, Update { source: source.clone(), path: path.clone(), novel: novel.clone(), chapter: c.name.clone(), chapter_path: c.path.clone(), at: now_ms() });
            }
            self.data.updates.truncate(MAX_UPDATES);
        }
        self.changed(cx);
        if first { 0 } else { fresh.len() }
    }

    pub fn clear_updates(&mut self, cx: &mut Context<Self>) {
        self.data.updates.clear();
        self.changed(cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
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
            cx.background_executor()
                .spawn(async move {
                    let tmp = path.with_extension("tmp");
                    if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
                        eprintln!("couldn't save {}", path.display());
                    }
                })
                .await;
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(p: &str) -> Chapter {
        Chapter { name: p.into(), path: p.into(), ..Default::default() }
    }

    #[test]
    fn resume_picks_progress_then_first_unread() {
        let mut n = Novel { chapters: vec![ch("a"), ch("b"), ch("c")], ..Default::default() };
        assert_eq!(n.resume_index(), 0);
        n.read.insert("a".into());
        assert_eq!(n.resume_index(), 1);
        n.progress = Some(Progress { chapter: "c".into(), name: "c".into(), scroll: 0.2, at: 1 });
        assert_eq!(n.resume_index(), 2);
        assert_eq!(n.unread(), 2);
    }

    #[test]
    fn unkept_novels_are_dropped() {
        let n = Novel::default();
        assert!(!n.kept());
        assert!(Novel { in_library: true, ..Default::default() }.kept());
    }
}
