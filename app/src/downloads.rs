//! Chapters saved for reading offline, laid out like Mihon's so the folders mean the same in both apps:
//! `<downloads>/<Source (LANG)>/<Series title>/<scanlator>_<chapter>/001.jpg`.
//! Chapters download into `<name>_tmp` and get renamed when done, so a half-finished one never looks complete.

use crate::Source;
use gpui::{App, AppContext, AsyncApp, Context, Entity, Global};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DIR: OnceLock<PathBuf> = OnceLock::new();
const TMP: &str = "_tmp";
/// Mihon's limit, less room for its optional hash suffix and ".cbz".
const MAX_NAME_BYTES: usize = 240 - 11;

pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// Mihon's `DiskUtil.buildValidFilename`: no FAT-invalid characters, no leading or trailing dots and spaces.
pub fn valid_name(name: &str, max_bytes: usize) -> String {
    let name = name.trim_matches(|c| c == '.' || c == ' ');
    if name.is_empty() {
        return "(invalid)".into();
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let bad = c <= '\u{1f}' || c == '\u{7f}' || matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|');
        let c = if bad { '_' } else { c };
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

/// "Name (LANG)", like Mihon's `HttpSource.toString`. A removed extension's language is unknown, so its folder is found by name.
fn source_dir(source: &Source) -> Option<PathBuf> {
    let root = dir()?;
    if !source.lang.is_empty() {
        return Some(root.join(valid_name(&format!("{} ({})", source.name, source.lang.to_uppercase()), 240)));
    }
    let prefix = format!("{} (", valid_name(&source.name, 240));
    std::fs::read_dir(root).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
}

pub fn series_dir(source: &Source, title: &str) -> Option<PathBuf> {
    Some(source_dir(source)?.join(valid_name(title, 240)))
}

/// Mihon's chapter folder name: "scanlator_name", or just the name.
pub fn chapter_name(chapter: &Value) -> String {
    let name = chapter["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or("Chapter");
    match chapter["scanlator"].as_str().filter(|s| !s.trim().is_empty()) {
        Some(group) => valid_name(&format!("{group}_{name}"), MAX_NAME_BYTES),
        None => valid_name(name, MAX_NAME_BYTES),
    }
}

/// The finished chapter folders of a series, by folder name.
pub fn downloaded(source: &Source, title: &str) -> HashSet<String> {
    let Some(dir) = series_dir(source, title) else { return HashSet::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return HashSet::new() };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.ends_with(TMP))
        .collect()
}

/// A downloaded chapter's page files in reading order, if it's downloaded.
pub fn local_pages(source: &Source, title: &str, chapter: &Value) -> Option<Vec<PathBuf>> {
    let dir = series_dir(source, title)?.join(chapter_name(chapter));
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| is_image(p)).collect();
    pages.sort();
    (!pages.is_empty()).then_some(pages)
}

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "jxl" | "heif"))
}

pub fn delete(source: &Source, title: &str, chapter: &Value) {
    if let Some(dir) = series_dir(source, title) {
        let name = chapter_name(chapter);
        let _ = std::fs::remove_dir_all(dir.join(&name));
        let _ = std::fs::remove_dir_all(dir.join(format!("{name}{TMP}")));
    }
}

#[derive(Clone)]
pub struct Job {
    pub source: Source,
    pub title: String,
    pub chapter: Value,
}

impl Job {
    pub fn url(&self) -> String {
        self.chapter["url"].as_str().unwrap_or_default().to_string()
    }
}

#[derive(Clone, PartialEq)]
pub enum State {
    Queued,
    /// Pages done out of the total.
    Running(usize, usize),
    Failed(String),
}

/// The download queue. One chapter at a time, one page at a time, so sources aren't hammered.
pub struct Downloads {
    pub queue: VecDeque<Job>,
    pub states: HashMap<String, State>,
    running: bool,
    /// Set when the running chapter is cancelled.
    cancelled: HashSet<String>,
}

struct Shared(Entity<Downloads>);
impl Global for Shared {}

pub fn init(cx: &mut App) {
    let entity = cx.new(|_| Downloads { queue: VecDeque::new(), states: HashMap::new(), running: false, cancelled: HashSet::new() });
    cx.set_global(Shared(entity));
}

pub fn get(cx: &App) -> Entity<Downloads> {
    cx.global::<Shared>().0.clone()
}

impl Downloads {
    pub fn enqueue(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        for job in jobs {
            let url = job.url();
            if self.queue.iter().any(|j| j.url() == url) {
                continue;
            }
            self.states.insert(url, State::Queued);
            self.queue.push_back(job);
        }
        cx.notify();
        self.next(cx);
    }

    pub fn cancel(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.queue.iter().position(|j| j.url() == url) {
            if i == 0 && self.running {
                self.cancelled.insert(url.to_string());
            } else {
                self.queue.remove(i);
            }
        }
        self.states.remove(url);
        cx.notify();
    }

    pub fn cancel_all(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<String> = self.queue.iter().map(Job::url).collect();
        for url in urls {
            self.cancel(&url, cx);
        }
    }

    /// Puts failed chapters back in line.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        for job in &self.queue {
            if matches!(self.states.get(&job.url()), Some(State::Failed(_))) {
                self.states.insert(job.url(), State::Queued);
            }
        }
        cx.notify();
        self.next(cx);
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(job) = self.queue.iter().find(|j| self.states.get(&j.url()) == Some(&State::Queued)).cloned() else { return };
        // The job being worked on sits at the front, so cancel can tell it apart.
        if let Some(i) = self.queue.iter().position(|j| j.url() == job.url()) {
            let job = self.queue.remove(i).unwrap();
            self.queue.push_front(job);
        }
        self.running = true;
        cx.spawn(async move |this, cx| {
            let url = job.url();
            let result = download(&job, &this, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.running = false;
                let cancelled = this.cancelled.remove(&url);
                match result {
                    Ok(()) => {
                        this.queue.retain(|j| j.url() != url);
                        this.states.remove(&url);
                    }
                    Err(_) if cancelled => {
                        this.queue.retain(|j| j.url() != url);
                        delete(&job.source, &job.title, &job.chapter);
                    }
                    Err(e) => {
                        // Failed ones go to the back so the rest keep going.
                        if let Some(i) = this.queue.iter().position(|j| j.url() == url) {
                            let job = this.queue.remove(i).unwrap();
                            this.queue.push_back(job);
                        }
                        this.states.insert(url, State::Failed(e));
                    }
                }
                cx.notify();
                this.next(cx);
            });
        })
        .detach();
    }
}

async fn download(job: &Job, this: &gpui::WeakEntity<Downloads>, cx: &mut AsyncApp) -> Result<(), String> {
    let dir = series_dir(&job.source, &job.title).ok_or("No downloads folder")?;
    let name = chapter_name(&job.chapter);
    let done = dir.join(&name);
    if done.is_dir() {
        return Ok(());
    }
    let tmp = dir.join(format!("{name}{TMP}"));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let pages = job.source.call(cx, "pages", json!({ "chapter": job.chapter })).await?;
    let pages = pages.as_array().cloned().unwrap_or_default();
    if pages.is_empty() {
        return Err("This chapter has no pages".into());
    }
    let digits = pages.len().to_string().len().max(3);
    let url = job.url();
    for (i, page) in pages.iter().enumerate() {
        if this.read_with(cx, |d, _| d.cancelled.contains(&url)).unwrap_or(true) {
            return Err("Cancelled".into());
        }
        let stem = format!("{:0digits$}", i + 1);
        // A retry keeps the pages it already has.
        let have = std::fs::read_dir(&tmp).ok().into_iter().flatten().flatten().any(|e| e.path().file_stem().is_some_and(|s| s == stem.as_str()));
        if !have {
            let bytes = match crate::page_cache::get(&page.to_string()) {
                Some(bytes) => bytes,
                None => {
                    let path = job.source.call(cx, "download", json!({ "page": page })).await?;
                    let path = PathBuf::from(path.as_str().ok_or("no file from host")?);
                    cx.background_executor()
                        .spawn(async move {
                            let bytes = std::fs::read(&path).map_err(|e| e.to_string());
                            let _ = std::fs::remove_file(&path);
                            bytes
                        })
                        .await?
                }
            };
            let ext = image::guess_format(&bytes).ok().and_then(|f| f.extensions_str().first().copied()).ok_or("A page isn't an image")?;
            let file = tmp.join(format!("{stem}.{ext}"));
            cx.background_executor().spawn(async move { std::fs::write(file, bytes) }).await.map_err(|e| e.to_string())?;
        }
        let _ = this.update(cx, |d, cx| {
            d.states.insert(url.clone(), State::Running(i + 1, pages.len()));
            cx.notify();
        });
    }
    std::fs::rename(&tmp, &done).map_err(|e| e.to_string())
}

impl gpui::Render for Downloads {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use crate::{ACCENT, ACCENT_TEXT, FAINT, LINE, MUTED, RULE, heading, u};
        use gpui::{InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, relative, rgb};
        let failed = self.states.values().filter(|s| matches!(s, State::Failed(_))).count();
        let note = match (self.queue.len(), failed) {
            (0, _) => "nothing queued".to_string(),
            (n, 0) => format!("{n} in queue"),
            (n, f) => format!("{n} in queue · {f} failed"),
        };
        let button = |id: &'static str, label: &'static str| {
            div().id(id).h(u(32.)).px(u(14.)).rounded(u(16.)).border_1().border_color(rgb(LINE)).flex().items_center().text_size(u(13.)).text_color(rgb(MUTED)).cursor_pointer().hover(|s| s.bg(rgb(crate::HOVER)).text_color(rgb(crate::TEXT))).child(label)
        };
        let rows = self.queue.iter().enumerate().map(|(i, job)| {
            let url = job.url();
            let state = self.states.get(&url).cloned().unwrap_or(State::Queued);
            let (text, fraction, color) = match &state {
                State::Queued => ("Waiting".to_string(), 0., FAINT),
                State::Running(d, n) => (format!("{d} of {n} pages"), *d as f32 / (*n).max(1) as f32, MUTED),
                State::Failed(e) => (e.clone(), 0., ACCENT_TEXT),
            };
            div()
                .py(u(12.))
                .flex()
                .items_center()
                .gap_4()
                .border_b_1()
                .border_color(rgb(RULE))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(u(6.))
                        .child(div().flex().items_baseline().gap_3().child(div().flex_none().font_weight(gpui::FontWeight::SEMIBOLD).child(job.title.clone())).child(div().min_w_0().truncate().text_color(rgb(MUTED)).child(job.chapter["name"].as_str().unwrap_or("Chapter").to_string())))
                        .child(div().h(u(3.)).rounded(u(2.)).bg(rgb(LINE)).child(div().h_full().rounded(u(2.)).w(relative(fraction)).bg(rgb(ACCENT))))
                        .child(div().truncate().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(color)).child(text)),
                )
                .child(button("cancel", "Cancel").id(("cancel", i)).on_click(cx.listener(move |this, _, _, cx| this.cancel(&url, cx))))
        });
        let empty = self.queue.is_empty();
        div()
            .id("downloads")
            .size_full()
            .overflow_y_scroll()
            .px(u(40.))
            .pt(u(32.))
            .pb(u(40.))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_3()
                    .child(div().flex_1().child(heading("Downloads", note)))
                    .when(failed > 0, |d| d.child(button("retry", "Retry failed").on_click(cx.listener(|this, _, _, cx| this.retry(cx)))))
                    .when(!empty, |d| d.child(button("cancel-all", "Cancel all").on_click(cx.listener(|this, _, _, cx| this.cancel_all(cx)))))
                    .child(button("open-folder", "Open folder").on_click(|_, _, cx| {
                        if let Some(dir) = dir() {
                            let _ = std::fs::create_dir_all(dir);
                            cx.open_with_system(dir);
                        }
                    })),
            )
            .child(div().pt(u(20.)).children(rows))
            .when(empty, |d| {
                d.child(div().pt(u(8.)).text_color(rgb(MUTED)).child("Download chapters from a series' page: right-click a chapter, or use Download unread. They're kept in Mihon's folder layout."))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_mihons() {
        assert_eq!(valid_name(" ..Re:Zero? / Part 2.. ", 240), "Re_Zero_ _ Part 2");
        assert_eq!(valid_name("...", 240), "(invalid)");
        assert_eq!(valid_name("ééé", 4), "éé");
        assert_eq!(chapter_name(&json!({ "name": "Ch. 1", "scanlator": "Team" })), "Team_Ch. 1");
        assert_eq!(chapter_name(&json!({ "name": "Ch. 1", "scanlator": "" })), "Ch. 1");
        assert_eq!(chapter_name(&json!({ "name": " " })), "Chapter");
    }
}
