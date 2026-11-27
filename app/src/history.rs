//! Recently read series as covers, with the chapter and page you stopped at.

use crate::u;
use crate::browse::OpenManga;
use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover, grid};
use crate::images::ImageCache;
use crate::library::Resume;
use crate::registry::Registry;
use crate::store::{Series, Store, ago, now_ms};
use crate::{FAINT, MUTED, heading};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, rgb, uniform_list,
};
use std::ops::Range;
use std::sync::Arc;

const PAD: f32 = 40.;
const GAP: f32 = 24.;

pub struct History {
    store: Entity<Store>,
    registry: Entity<Registry>,
    cache: Entity<ImageCache>,
    covers: Covers,
    rows: Vec<Series>,
    cols: usize,
    card_w: f32,
    _sub: Subscription,
}

impl EventEmitter<Resume> for History {}
impl EventEmitter<OpenManga> for History {}

impl History {
    pub fn new(store: Entity<Store>, registry: Entity<Registry>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        History { store, registry, cache, covers: Covers::default(), rows: vec![], cols: 1, card_w: CARD_W, _sub: sub }
    }

    fn card(&mut self, i: usize, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let source = self.registry.read(cx).source(&self.rows[i].source);
        let url = self.rows[i].manga["thumbnailUrl"].as_str().map(str::to_string);
        let image = self.covers.get(&self.cache, source.map(Fetch::Cover), url.as_deref(), |h: &mut History| &mut h.covers, cx);
        let s = &self.rows[i];
        let Some(p) = s.progress.clone() else { return div().into_any_element() };
        let w = self.card_w;
        div()
            .id(("history", i))
            .group("history-card")
            .w(u(w))
            .flex()
            .flex_col()
            .gap(u(4.))
            .cursor_pointer()
            .child(
                div()
                    .relative()
                    .mb(u(4.))
                    .child(cover(image, s.title(), w, w * CARD_H / CARD_W, 0))
                    .child(
                        crate::remove_history_button(("history-rm", i), "history-card", true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                let (source, url) = {
                                    let s = &this.rows[i];
                                    (s.source.clone(), s.url().to_string())
                                };
                                this.store.update(cx, |store, cx| {
                                    store.remove_from_history(&source, &url, cx)
                                });
                            })),
                    ),
            )
            .child(
                div()
                    .id(("history-title", i))
                    .self_start()
                    .max_w_full()
                    .text_size(u(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(s.title().to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let s = &this.rows[i];
                        let source = this.registry.read(cx).source_or_saved(&s.source, &s.source_name);
                        cx.emit(OpenManga { source, manga: s.manga.clone() });
                        cx.stop_propagation();
                    })),
            )
            .child(div().text_size(u(13.)).text_color(rgb(0xC9C1B3)).truncate().child(p.chapter_name.clone()))
            .child(
                div()
                    .font_family(crate::fonts::mono())
                    .text_size(u(12.))
                    .text_color(rgb(FAINT))
                    .truncate()
                    .child(format!("p.{} / {} · {}", p.page + 1, p.pages, ago(p.at, now))),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let s = &this.rows[i];
                let (Some(source), Some(index)) = (this.registry.read(cx).source(&s.source), s.reading_index()) else { return };
                let page = s.progress.as_ref().map_or(0, |p| p.page);
                cx.emit(Resume { source, manga: s.manga.clone(), chapters: Arc::new(s.chapters.clone()), index, page });
            }))
            .into_any_element()
    }
}

impl Render for History {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let undo_count = store.history_undo().len();
        self.rows = store
            .history()
            .into_iter()
            .filter(|s| !store.hides_series(crate::store::Area::History, s))
            .cloned()
            .map(|mut s| {
                // Chapters after the scanlator filter. Not saved.
                s.chapters = s.visible();
                s.hidden_scanlators.clear();
                s
            })
            .collect();
        (self.cols, self.card_w) = grid(crate::content_width(window), PAD, GAP, CARD_W);
        let (count, cols) = (self.rows.len(), self.cols);
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
                    .items_center()
                    .justify_between()
                    .gap(u(16.))
                    .child(heading("History", format!("{count} series")))
                    .when(count > 0, |d| {
                        d.child(
                            crate::catalog::small_button("clear-history", "Clear all", false)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.store.update(cx, |s, cx| s.clear_history(cx));
                                })),
                        )
                    }),
            )
            .when(undo_count > 0, |d| {
                d.child(div().px(u(PAD)).mb(u(8.)).child(crate::history_undo_banner(
                    "hist",
                    self.store.clone(),
                    undo_count,
                )))
            })
            .children((count == 0).then(|| {
                div()
                    .px(u(PAD))
                    .pt(u(20.))
                    .text_color(rgb(MUTED))
                    .child("Nothing read yet.")
            }))
            .child(
                uniform_list(
                    "history",
                    count.div_ceil(cols),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        let now = now_ms();
                        range
                            .map(|row| {
                                let cards: Vec<AnyElement> =
                                    (row * cols..((row + 1) * cols).min(this.rows.len())).map(|i| this.card(i, now, cx)).collect();
                                div().flex().gap(u(GAP)).px(u(PAD)).py(u(GAP / 2.)).children(cards)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
    }
}
