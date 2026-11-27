//! A novel's page: its details, a library button, and the chapters.

use super::plugins::{Details, Plugins, Source};
use super::store::{LnStore, Novel};
use super::{PAD, cover_fetch};
use crate::catalog::small_button;
use crate::covers::{Covers, cover};
use crate::images::ImageCache;
use crate::{ACCENT, ACCENT_TEXT, FAINT, HOVER, MUTED, TEXT, pill, u};
use gpui::{
    Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, div, prelude::FluentBuilder, relative, rgb, uniform_list,
};
use std::ops::Range;

pub enum NovelEvent {
    Back,
    /// Read the chapter at this index of the novel's list, oldest first.
    Read(usize),
}

pub struct NovelView {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    cache: Entity<ImageCache>,
    source: Option<Source>,
    novel: Novel,
    loading: bool,
    error: Option<SharedString>,
    more: bool,
    covers: Covers,
    _sub: Subscription,
}

impl EventEmitter<NovelEvent> for NovelView {}

fn fill(into: &mut String, from: String) {
    if !from.is_empty() {
        *into = from;
    }
}

impl NovelView {
    pub fn new(source: &str, path: &str, store: Entity<LnStore>, plugins: Entity<Plugins>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let src = plugins.update(cx, |p, _| p.source(source));
        let name = plugins.read(cx).entry(source).map_or(source.to_string(), |e| e.name.clone());
        let stored = store.read(cx).get(source, path).cloned();
        let novel = stored.unwrap_or_else(|| Novel { source: source.into(), source_name: name, path: path.into(), ..Default::default() });
        let sub = cx.observe(&store, |this, store, cx| {
            // Library flag, reads and progress change in the reader and elsewhere.
            if let Some(s) = store.read(cx).get(&this.novel.source, &this.novel.path) {
                this.novel.in_library = s.in_library;
                this.novel.read = s.read.clone();
                this.novel.progress = s.progress.clone();
            }
            cx.notify();
        });
        let mut this = NovelView { store, plugins, cache, source: src, novel, loading: false, error: None, more: false, covers: Covers::default(), _sub: sub };
        this.refresh(cx);
        this
    }

    pub fn novel(&self) -> &Novel {
        &self.novel
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            self.error = Some(format!("{} isn't installed. Install it in Extensions to refresh this novel.", self.novel.source_name).into());
            return;
        };
        self.loading = true;
        self.error = None;
        let path = self.novel.path.clone();
        cx.spawn(async move |this, cx| {
            let result = super::guarded(cx, || source.novel(&path)).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(d) => this.apply(d, cx),
                    Err(e) => this.error = Some(e.lines().next().unwrap_or("The source failed").to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, d: Details, cx: &mut Context<Self>) {
        let n = &mut self.novel;
        fill(&mut n.name, d.name);
        fill(&mut n.cover, d.cover);
        fill(&mut n.summary, d.summary);
        fill(&mut n.author, d.author);
        fill(&mut n.status, d.status);
        fill(&mut n.genres, d.genres);
        // A plugin that finds no chapters shouldn't wipe the ones already known.
        let found = (!d.chapters.is_empty()).then_some(d.chapters);
        if let Some(chapters) = &found {
            n.chapters = chapters.clone();
        }
        let stored = self.store.read(cx).get(&n.source, &n.path).is_some();
        if stored {
            let (novel, found) = (n.clone(), found);
            self.store.update(cx, |s, cx| {
                if let Some(chapters) = found {
                    s.found_chapters(&novel.source, &novel.path, chapters, cx);
                }
                s.remember(novel, cx);
            });
        }
    }

    /// Reading and library marks need the novel saved first.
    fn ensure_stored(&mut self, cx: &mut Context<Self>) {
        if self.store.read(cx).get(&self.novel.source, &self.novel.path).is_none() {
            let n = self.novel.clone();
            self.store.update(cx, |s, cx| s.remember(n, cx));
        }
    }

    fn toggle_library(&mut self, cx: &mut Context<Self>) {
        self.ensure_stored(cx);
        let (source, path, on) = (self.novel.source.clone(), self.novel.path.clone(), !self.novel.in_library);
        self.store.update(cx, |s, cx| s.set_in_library(&source, &path, on, cx));
    }

    fn toggle_read(&mut self, chapter: &str, cx: &mut Context<Self>) {
        self.ensure_stored(cx);
        let (source, path, chapter) = (self.novel.source.clone(), self.novel.path.clone(), chapter.to_string());
        self.store.update(cx, |s, cx| {
            s.edit(&source, &path, cx, |n| {
                if !n.read.remove(&chapter) {
                    n.read.insert(chapter);
                }
            })
        });
    }
}

impl Render for NovelView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let oldest_first = self.store.read(cx).prefs().oldest_first;
        let count = self.novel.chapters.len();
        // Position in the chapter list for each row, so newest first still opens the right chapter.
        let order: Vec<usize> = if oldest_first { (0..count).collect() } else { (0..count).rev().collect() };
        let how = cover_fetch(self.plugins.read(cx), &self.novel.source);
        let url = Some(self.novel.cover.clone()).filter(|c| !c.is_empty());
        let image = self.covers.get(&self.cache, how, url.as_deref(), |v: &mut NovelView| &mut v.covers, cx);
        let n = &self.novel;
        let meta: Vec<String> = [n.author.clone(), n.status.clone(), n.source_name.clone()].into_iter().filter(|s| !s.is_empty()).collect();
        let resume = n.progress.is_some();
        let start = n.resume_index();
        let label = if resume { "Continue".to_string() } else if n.read.is_empty() { "Start reading".to_string() } else { "Read next".to_string() };
        let in_library = n.in_library;
        let status: SharedString = if self.loading { "loading chapters…".into() } else { format!("{count} chapters").into() };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(u(PAD)).pt(u(20.)).child(small_button("ln-back", "← Back", false).on_click(cx.listener(|_, _, _, cx| cx.emit(NovelEvent::Back)))))
            .child(
                div()
                    .px(u(PAD))
                    .pt(u(16.))
                    .pb(u(16.))
                    .flex()
                    .gap(u(28.))
                    .child(cover(image, &n.name, 180., 270., 0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(8.))
                            .child(div().font_family(crate::fonts::display()).font_weight(FontWeight::EXTRA_BOLD).text_size(u(30.)).line_height(relative(1.1)).line_clamp(2).child(n.name.to_uppercase()))
                            .child(div().text_color(rgb(MUTED)).child(meta.join("  ·  ")))
                            .children((!n.genres.is_empty()).then(|| div().text_size(u(13.)).text_color(rgb(FAINT)).line_clamp(2).child(n.genres.clone())))
                            .child(
                                div()
                                    .pt(u(4.))
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(pill("ln-read", label, true).on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.novel.chapters.is_empty() {
                                            this.ensure_stored(cx);
                                            cx.emit(NovelEvent::Read(start));
                                        }
                                    })))
                                    .child(pill("ln-lib", if in_library { "In library ✓" } else { "Add to library" }, false).on_click(cx.listener(|this, _, _, cx| this.toggle_library(cx)))),
                            )
                            .children(self.error.clone().map(|e| div().text_color(rgb(ACCENT_TEXT)).child(e)))
                            .children((!n.summary.is_empty()).then(|| {
                                div()
                                    .id("ln-summary")
                                    .pt(u(4.))
                                    .max_w(u(760.))
                                    .text_color(rgb(crate::BODY))
                                    .when(!self.more, |d| d.line_clamp(5))
                                    .cursor_pointer()
                                    .child(n.summary.clone())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.more = !this.more;
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .px(u(PAD))
                    .py(u(8.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().font_family(crate::fonts::mono()).text_size(u(13.)).text_color(rgb(FAINT)).child(status))
                    .child(small_button("ln-sort", if oldest_first { "Oldest first" } else { "Newest first" }, false).on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.oldest_first = !p.oldest_first));
                    }))),
            )
            .child(
                uniform_list(
                    "ln-chapters",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .filter_map(|row| {
                                let i = *order.get(row)?;
                                let c = this.novel.chapters.get(i)?.clone();
                                let read = this.novel.read.contains(&c.path);
                                let here = this.novel.progress.as_ref().filter(|p| p.chapter == c.path && !read).map(|p| format!("{}%", (p.scroll * 100.).round() as u32));
                                let path = c.path.clone();
                                Some(
                                    div()
                                        .id(("ln-ch", row))
                                        .mx(u(PAD))
                                        .h(u(44.))
                                        .px(u(12.))
                                        .rounded(u(8.))
                                        .flex()
                                        .items_center()
                                        .gap_4()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .text_color(rgb(if read { FAINT } else { TEXT }))
                                        .child(div().flex_1().min_w_0().truncate().child(c.name.clone()))
                                        .children(here.map(|h| div().flex_none().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(ACCENT_TEXT)).child(h)))
                                        .child(div().flex_none().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(c.release.clone()))
                                        .child(
                                            div()
                                                .id(("ln-mark", row))
                                                .flex_none()
                                                .size(u(28.))
                                                .rounded(u(14.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .hover(|s| s.bg(rgb(crate::LINE)))
                                                .child(crate::check(14., if read { ACCENT } else { crate::LINE }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_read(&path, cx);
                                                    cx.stop_propagation();
                                                })),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.ensure_stored(cx);
                                            cx.emit(NovelEvent::Read(i));
                                        })),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
    }
}
