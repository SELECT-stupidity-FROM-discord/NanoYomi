//! Reader look and the chapter cache, laid out like manga's Settings.

use super::PAD;
use super::store::{LnStore, Prefs};
use crate::catalog::small_button;
use crate::{FAINT, MUTED, RULE, heading, u};
use gpui::{Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div, rgb};

pub struct Settings {
    store: Entity<LnStore>,
    cache_cleared: bool,
    _sub: Subscription,
}

impl Settings {
    pub fn new(store: Entity<LnStore>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Settings { store, cache_cleared: false, _sub: sub }
    }
}

fn label(text: &'static str) -> impl IntoElement {
    div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(text)
}

/// A title and note on the left, controls on the right.
fn row(title: &'static str, note: &'static str, first: bool, controls: gpui::Div) -> gpui::Div {
    div()
        .w_full()
        .py(u(14.))
        .when_first(first)
        .border_b_1()
        .border_color(rgb(RULE))
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(u(4.))
                .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(note)),
        )
        .child(controls.flex_none().flex().items_center().gap(u(6.)))
}

trait FirstRow {
    fn when_first(self, first: bool) -> Self;
}

impl FirstRow for gpui::Div {
    fn when_first(self, first: bool) -> Self {
        if first { self.border_t_1() } else { self }
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.store.read(cx).prefs().clone();
        let store = self.store.clone();
        let stepper = |id: &'static str, value: String, minus: fn(&mut Prefs), plus: fn(&mut Prefs)| {
            let (a, b) = (store.clone(), store.clone());
            div()
                .child(small_button((id, 0usize), "−", false).on_click(move |_, _, cx| a.update(cx, |s, cx| s.edit_prefs(cx, minus))))
                .child(div().w(u(64.)).flex().justify_center().font_family(crate::fonts::mono()).child(value))
                .child(small_button((id, 1usize), "+", false).on_click(move |_, _, cx| b.update(cx, |s, cx| s.edit_prefs(cx, plus))))
        };
        let text = row(
            "Text size",
            "How big chapter text is. The A− and A+ buttons in the reader change it too.",
            true,
            stepper("ln-fs", format!("{}", prefs.font_size), |p| p.font_size = (p.font_size - 1.).max(12.), |p| p.font_size = (p.font_size + 1.).min(40.)),
        );
        let spacing = row(
            "Line spacing",
            "The space between lines.",
            false,
            stepper("ln-lh", format!("{:.1}", prefs.line_height), |p| p.line_height = (p.line_height - 0.1).max(1.2), |p| p.line_height = (p.line_height + 0.1).min(2.4)),
        );
        let width = row(
            "Column width",
            "How wide the text column is. Arrows scroll, left and right change chapter, Esc goes back.",
            false,
            stepper("ln-w", format!("{}", prefs.width), |p| p.width = (p.width - 40.).max(400.), |p| p.width = (p.width + 40.).min(1200.)),
        );
        let cleared = self.cache_cleared;
        let cache = row(
            "Saved chapters",
            "Chapters you've opened are kept on disk so they open fast and offline. Clear to fetch them again.",
            true,
            div().child(small_button("ln-cache", if cleared { "Cleared" } else { "Clear" }, false).on_click(cx.listener(|this, _, _, cx| {
                super::clear_chapter_cache();
                this.cache_cleared = true;
                cx.notify();
            }))),
        );
        div()
            .id("ln-settings")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(u(PAD))
            .pt(u(28.))
            .pb(u(40.))
            .child(heading("Settings", ""))
            .child(label("READING"))
            .child(text)
            .child(spacing)
            .child(width)
            .child(label("STORAGE"))
            .child(cache)
    }
}
