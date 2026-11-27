//! Reading a chapter as text. It remembers how far down you are, and opens the next chapter at the end.

use super::chapter_file;
use super::plugins::Source;
use super::store::{LnStore, Novel, Progress};
use super::text;
use crate::catalog::small_button;
use crate::rich::{Block, highlights, is_rtl_block, render_rtl_block};
use crate::store::now_ms;
use crate::{BG, BODY, FAINT, LINE, MUTED, ACCENT_TEXT, RULE, TEXT, u};
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Task, Window, div, point, px, relative, rgb,
};
use std::time::Duration;

pub struct Close;

enum State {
    Loading,
    Failed(SharedString),
    Ready(Vec<Block>),
}

pub struct LnReader {
    store: Entity<LnStore>,
    source: Option<Source>,
    novel: Novel,
    index: usize,
    state: State,
    scroll: ScrollHandle,
    /// Bumped on each chapter so a slow load can't land in the wrong one.
    generation: u64,
    /// The fraction last written to the store, so nothing is saved while you're not moving.
    saved: f32,
    focus: FocusHandle,
    _saver: Task<()>,
}

impl EventEmitter<Close> for LnReader {}

impl LnReader {
    pub fn new(novel: Novel, index: usize, source: Option<Source>, store: Entity<LnStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        if store.read(cx).get(&novel.source, &novel.path).is_none() {
            let n = novel.clone();
            store.update(cx, |s, cx| s.remember(n, cx));
        }
        let saver = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            if this.update(cx, |this, cx| this.save(cx)).is_err() {
                break;
            }
        });
        let index = index.min(novel.chapters.len().saturating_sub(1));
        let mut this = LnReader { store, source, novel, index, state: State::Loading, scroll: ScrollHandle::new(), generation: 0, saved: -1., focus, _saver: saver };
        let at = this.novel.progress.as_ref().filter(|p| this.novel.chapters.get(index).is_some_and(|c| c.path == p.chapter)).map_or(0., |p| p.scroll);
        this.load(index, at, cx);
        this
    }

    fn fraction(&self) -> f32 {
        let max = f32::from(self.scroll.max_offset().y);
        if max <= 0. { 0. } else { (-f32::from(self.scroll.offset().y) / max).clamp(0., 1.) }
    }

    /// Writes where you are into the store, and marks the chapter read near its end.
    fn save(&mut self, cx: &mut Context<Self>) {
        let (State::Ready(_), Some(ch)) = (&self.state, self.novel.chapters.get(self.index)) else { return };
        let at = self.fraction();
        if (at - self.saved).abs() < 0.005 {
            return;
        }
        self.saved = at;
        let (source, path, ch) = (self.novel.source.clone(), self.novel.path.clone(), ch.clone());
        self.store.update(cx, |s, cx| {
            s.edit(&source, &path, cx, |n| {
                n.progress = Some(Progress { chapter: ch.path.clone(), name: ch.name.clone(), scroll: at, at: now_ms() });
                if at >= 0.95 {
                    n.read.insert(ch.path);
                }
            })
        });
    }

    fn load(&mut self, index: usize, restore: f32, cx: &mut Context<Self>) {
        let Some(ch) = self.novel.chapters.get(index).cloned() else {
            self.state = State::Failed("This novel has no chapters yet".into());
            return;
        };
        self.index = index;
        self.state = State::Loading;
        self.saved = -1.;
        self.generation += 1;
        let generation = self.generation;
        let source = self.source.clone();
        let file = chapter_file(&self.novel.source, &ch.path);
        let next = self.novel.chapters.get(index + 1).map(|c| c.path.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let cached = match file.clone() {
                Some(f) => cx.background_executor().spawn(async move { std::fs::read_to_string(f).ok() }).await,
                None => None,
            };
            let html = match (cached, &source) {
                (Some(h), _) => Ok(h),
                (None, Some(s)) => super::guarded(cx, || s.chapter(&ch.path)).await,
                (None, None) => Err("This source isn't installed, and the chapter wasn't saved. Install it in Extensions.".to_string()),
            };
            if let (Ok(h), Some(f)) = (&html, file) {
                let h = h.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = std::fs::create_dir_all(f.parent().unwrap());
                        let _ = std::fs::write(f, h);
                    })
                    .await;
            }
            let blocks = html.map(|h| text::parse(&h));
            let ok = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.state = match blocks {
                        Ok(b) if b.is_empty() => State::Failed("The source sent an empty chapter. It may want a sign-in, or the site changed.".into()),
                        Ok(b) => State::Ready(b),
                        Err(e) => State::Failed(e.lines().next().unwrap_or("The source failed").to_string().into()),
                    };
                    cx.notify();
                    matches!(this.state, State::Ready(_))
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            // Layout has to happen before the saved position means anything.
            for _ in 0..30 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let placed = this.update(cx, |this, cx| {
                    let max = this.scroll.max_offset().y;
                    if f32::from(max) <= 0. && restore > 0. {
                        return false;
                    }
                    this.scroll.set_offset(point(px(0.), -(max * restore)));
                    cx.notify();
                    true
                });
                if placed.unwrap_or(true) {
                    break;
                }
            }
            // The next chapter is fetched ahead, so turning the page is instant.
            if let (Some(next), Some(source)) = (next, source) {
                let file = chapter_file(&source.id, &next);
                if let Some(f) = file.filter(|f| !f.exists())
                    && let Ok(h) = source.chapter(&next).await
                {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = std::fs::create_dir_all(f.parent().unwrap());
                            let _ = std::fs::write(f, h);
                        })
                        .await;
                }
            }
        })
        .detach();
    }

    fn go(&mut self, to: usize, cx: &mut Context<Self>) {
        if to >= self.novel.chapters.len() || to == self.index {
            return;
        }
        // Leaving forward counts as finishing.
        if to > self.index {
            let (source, path, done) = (self.novel.source.clone(), self.novel.path.clone(), self.novel.chapters[self.index].path.clone());
            self.store.update(cx, |s, cx| {
                s.edit(&source, &path, cx, |n| {
                    n.read.insert(done);
                })
            });
        } else {
            self.save(cx);
        }
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.load(to, 0., cx);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.save(cx);
        cx.emit(Close);
    }

    fn scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let max = f32::from(self.scroll.max_offset().y);
        let y = (f32::from(self.scroll.offset().y) - delta).clamp(-max, 0.);
        self.scroll.set_offset(point(px(0.), px(y)));
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let page = f32::from(self.scroll.bounds().size.height) * 0.9;
        let k = &ev.keystroke;
        match k.key.as_str() {
            "escape" => self.close(cx),
            "space" | "pagedown" => self.scroll_by(if k.modifiers.shift { -page } else { page }, cx),
            "pageup" => self.scroll_by(-page, cx),
            "down" => self.scroll_by(60., cx),
            "up" => self.scroll_by(-60., cx),
            "home" => self.scroll_by(-1e9, cx),
            "end" => self.scroll_by(1e9, cx),
            "right" => self.go(self.index + 1, cx),
            "left" => self.go(self.index.saturating_sub(1), cx),
            _ => return,
        }
        let _ = window;
        cx.stop_propagation();
    }

    fn resize(&mut self, by: f32, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.font_size = (p.font_size + by).clamp(12., 40.)));
    }
}

impl Render for LnReader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.store.read(cx).prefs().clone();
        let chapter = self.novel.chapters.get(self.index).map(|c| c.name.clone()).unwrap_or_default();
        let count = self.novel.chapters.len();
        let at = self.index;
        let nav = |cx: &mut Context<Self>| {
            div()
                .pt(u(40.))
                .flex()
                .justify_center()
                .gap_3()
                .child(small_button("ln-prev", "← Previous", false).on_click(cx.listener(move |this, _, _, cx| this.go(at.saturating_sub(1), cx))))
                .child(small_button("ln-next", "Next chapter →", true).on_click(cx.listener(move |this, _, _, cx| this.go(at + 1, cx))))
        };
        let body = match &self.state {
            State::Loading => div().pt(u(120.)).flex().justify_center().text_color(rgb(MUTED)).child("Loading the chapter…").into_any_element(),
            State::Failed(e) => div()
                .pt(u(120.))
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(div().max_w(u(520.)).text_color(rgb(ACCENT_TEXT)).child(e.clone()))
                .child(small_button("ln-again", "Try again", true).on_click(cx.listener(move |this, _, _, cx| this.load(at, 0., cx))))
                .into_any_element(),
            State::Ready(blocks) => {
                let (size, gap) = (prefs.font_size, prefs.font_size * 0.8);
                div()
                    .flex()
                    .flex_col()
                    .gap(u(gap))
                    .text_size(u(size))
                    .line_height(relative(prefs.line_height))
                    .text_color(rgb(BODY))
                    .child(div().pb(u(8.)).font_family(crate::fonts::display()).font_weight(FontWeight::EXTRA_BOLD).text_size(u(size * 1.7)).line_height(relative(1.15)).text_color(rgb(TEXT)).child(chapter.to_uppercase()))
                    .children(blocks.iter().enumerate().map(|(i, b)| {
                        if b.text == "* * *" {
                            return div().flex().justify_center().text_color(rgb(FAINT)).child("* * *").into_any_element();
                        }
                        if is_rtl_block(&b.text) {
                            return div().child(render_rtl_block(i, b)).into_any_element();
                        }
                        div().child(StyledText::new(SharedString::from(b.text.clone())).with_highlights(highlights(b))).into_any_element()
                    }))
                    .child(nav(cx))
                    .into_any_element()
            }
        };
        div()
            .id("ln-reader")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .flex_none()
                    .h(u(52.))
                    .px(u(16.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(RULE))
                    .child(small_button("ln-close", "← Back", false).on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_size(u(13.5)).font_weight(FontWeight::MEDIUM).child(self.novel.name.clone()))
                            .child(div().truncate().text_size(u(12.)).text_color(rgb(FAINT)).child(format!("{chapter}  ·  {} of {count}", at + 1))),
                    )
                    .child(small_button("ln-smaller", "A−", false).on_click(cx.listener(|this, _, _, cx| this.resize(-1., cx))))
                    .child(small_button("ln-bigger", "A+", false).on_click(cx.listener(|this, _, _, cx| this.resize(1., cx))))
                    .child(div().w(u(1.)).h(u(20.)).bg(rgb(LINE)))
                    .child(small_button("ln-top-prev", "‹", false).on_click(cx.listener(move |this, _, _, cx| this.go(at.saturating_sub(1), cx))))
                    .child(small_button("ln-top-next", "›", false).on_click(cx.listener(move |this, _, _, cx| this.go(at + 1, cx)))),
            )
            .child(
                div()
                    .id("ln-reader-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().w_full().flex().justify_center().px(u(24.)).pt(u(40.)).pb(u(120.)).child(div().w(u(prefs.width)).max_w_full().child(body))),
            )
    }
}
