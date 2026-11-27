//! Your novels, what changed in them, and what you read last. One view, three pages.

use super::plugins::Plugins;
use super::store::{LnStore, Novel};
use super::{GAP, OpenNovel, PAD, ReadChapter, card, cover_fetch, notice};
use crate::catalog::small_button;
use crate::covers::{CARD_W, Covers, grid};
use crate::images::ImageCache;
use crate::store::{ago, now_ms};
use crate::{FAINT, MUTED, TEXT, heading, u};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, div, prelude::FluentBuilder, rgb, uniform_list,
};
use std::ops::Range;

/// What a card needs, without carrying the chapter list around.
#[derive(Clone)]
struct Row {
    source: String,
    path: String,
    name: String,
    cover: String,
    unread: usize,
    chapter: String,
    lines: Vec<String>,
}

fn row(n: &Novel, now: i64, history: bool) -> Row {
    let mut lines = vec![];
    if let Some(p) = n.progress.as_ref().filter(|_| history) {
        lines.push(p.name.clone());
        lines.push(format!("{}% · {}", (p.scroll * 100.).round() as u32, ago(p.at, now)));
    }
    Row { source: n.source.clone(), path: n.path.clone(), name: n.name.clone(), cover: n.cover.clone(), unread: n.unread(), chapter: n.progress.as_ref().map(|p| p.chapter.clone()).unwrap_or_default(), lines }
}

pub struct Library {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    cache: Entity<ImageCache>,
    covers: Covers,
    updates: bool,
    history: bool,
    /// How many novels the update check has done, and how many it will do.
    checking: Option<(usize, usize)>,
    rows: Vec<Row>,
    cols: usize,
    card_w: f32,
    _sub: Subscription,
}

impl EventEmitter<OpenNovel> for Library {}
impl EventEmitter<ReadChapter> for Library {}

impl Library {
    pub fn new(store: Entity<LnStore>, plugins: Entity<Plugins>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Library { store, plugins, cache, covers: Covers::default(), updates: false, history: false, checking: None, rows: vec![], cols: 1, card_w: CARD_W, _sub: sub }
    }

    pub fn show(&mut self, updates: bool, history: bool, cx: &mut Context<Self>) {
        self.updates = updates;
        self.history = history;
        cx.notify();
    }

    /// Asks every library novel's source whether it has new chapters.
    fn check(&mut self, cx: &mut Context<Self>) {
        if self.checking.is_some() {
            return;
        }
        let list: Vec<(String, String)> = self.store.read(cx).library().iter().map(|n| (n.source.clone(), n.path.clone())).collect();
        let total = list.len();
        if total == 0 {
            return;
        }
        self.checking = Some((0, total));
        cx.notify();
        let (store, plugins) = (self.store.clone(), self.plugins.clone());
        cx.spawn(async move |this, cx| {
            for (i, (source, path)) in list.into_iter().enumerate() {
                let src = plugins.update(cx, |p, _| p.source(&source));
                if let Some(src) = src {
                    match src.novel(&path).await {
                        Ok(d) => {
                            let _ = store.update(cx, |s, cx| s.found_chapters(&source, &path, d.chapters, cx));
                        }
                        Err(e) => eprintln!("{}: {e}", src.name),
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    this.checking = Some((i + 1, total));
                    cx.notify();
                });
            }
            let _ = this.update(cx, |this, cx| {
                this.checking = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let r = self.rows[i].clone();
        let how = cover_fetch(self.plugins.read(cx), &r.source);
        let image = self.covers.get(&self.cache, how, Some(r.cover.as_str()).filter(|c| !c.is_empty()), |l: &mut Library| &mut l.covers, cx);
        let history = self.history;
        card(("ln-card", i), image, &r.name, self.card_w, if history { 0 } else { r.unread }, r.lines.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                let r = &this.rows[i];
                if history && !r.chapter.is_empty() {
                    cx.emit(ReadChapter { source: r.source.clone(), path: r.path.clone(), chapter: r.chapter.clone() });
                } else {
                    cx.emit(OpenNovel { source: r.source.clone(), path: r.path.clone() });
                }
            }))
            .into_any_element()
    }
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.updates {
            return self.render_updates(cx).into_any_element();
        }
        let now = now_ms();
        let history = self.history;
        {
            let store = self.store.read(cx);
            self.rows = if history { store.history() } else { store.library() }.into_iter().map(|n| row(n, now, history)).collect();
        }
        (self.cols, self.card_w) = grid(crate::content_width(window), PAD, GAP, CARD_W);
        let (count, cols) = (self.rows.len(), self.cols);
        let (title, empty, hint) = if history {
            ("History", "Nothing read yet", "Novels you open in the reader show up here.")
        } else {
            ("Library", "Your library is empty", "Pick a source in the sidebar, open a novel and add it to your library.")
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(u(PAD)).pt(u(28.)).pb(u(12.)).child(heading(title, format!("{count} novels"))))
            .child(if count == 0 {
                notice(empty, hint).into_any_element()
            } else {
                uniform_list(
                    "ln-grid",
                    count.div_ceil(cols),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row| {
                                let cards: Vec<AnyElement> = (row * cols..((row + 1) * cols).min(this.rows.len())).map(|i| this.card(i, cx)).collect();
                                div().flex().gap(u(GAP)).px(u(PAD)).py(u(GAP / 2.)).children(cards)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
            .into_any_element()
    }
}

impl Library {
    fn render_updates(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let list: Vec<_> = self.store.read(cx).updates().to_vec();
        let note: SharedString = match self.checking {
            Some((done, total)) => format!("checking {done} of {total}…").into(),
            None => format!("{} new chapters", list.len()).into(),
        };
        let busy = self.checking.is_some();
        let count = list.len();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(u(PAD))
                    .pt(u(28.))
                    .pb(u(12.))
                    .flex()
                    .items_end()
                    .gap_4()
                    .child(div().flex_1().min_w_0().child(heading("Updates", note)))
                    .child(small_button("ln-check", if busy { "Checking…" } else { "Check now" }, true).on_click(cx.listener(|this, _, _, cx| this.check(cx))))
                    .when(count > 0, |d| d.child(small_button("ln-clear", "Clear", false).on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.clear_updates(cx)))))),
            )
            .child(if count == 0 {
                notice("Nothing new", "Chapters that appear in your library novels land here. Press Check now to look.").into_any_element()
            } else {
                uniform_list(
                    "ln-updates",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        let list: Vec<_> = this.store.read(cx).updates().to_vec();
                        range
                            .filter_map(|i| list.get(i).cloned().map(|up| (i, up)))
                            .map(|(i, up)| {
                                div()
                                    .id(("ln-update", i))
                                    .mx(u(PAD))
                                    .h(u(56.))
                                    .px(u(12.))
                                    .rounded(u(8.))
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(crate::HOVER)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(rgb(TEXT)).child(up.novel.clone()))
                                            .child(div().truncate().text_size(u(13.)).text_color(rgb(MUTED)).child(up.chapter.clone())),
                                    )
                                    .child(div().flex_none().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(ago(up.at, now)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let up = this.store.read(cx).updates().get(i).cloned();
                                        if let Some(up) = up {
                                            cx.emit(ReadChapter { source: up.source, path: up.path, chapter: up.chapter_path });
                                        }
                                    }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
    }
}
