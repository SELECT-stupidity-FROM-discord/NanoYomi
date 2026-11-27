//! Reader look, plugin repos and the chapter cache. Nothing here touches manga settings.

use super::PAD;
use super::plugins::{DEFAULT_REPO, Plugins};
use super::store::LnStore;
use crate::catalog::small_button;
use crate::input::{InputEvent, TextInput};
use crate::{ACCENT_TEXT, FAINT, LINE, MUTED, heading, u};
use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription,
    Window, div, rgb,
};

pub struct Settings {
    plugins: Entity<Plugins>,
    store: Entity<LnStore>,
    repo: Entity<TextInput>,
    note: SharedString,
    _subs: Vec<Subscription>,
}

impl Settings {
    pub fn new(plugins: Entity<Plugins>, store: Entity<LnStore>, cx: &mut Context<Self>) -> Self {
        let repo = cx.new(|cx| TextInput::new("https://…/plugins.min.json", cx));
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&repo, |this, input, _: &InputEvent, cx| {
                let url = input.read(cx).text.trim().to_string();
                if !url.starts_with("https://") {
                    this.note = "A repo address starts with https://".into();
                } else {
                    this.store.update(cx, |s, cx| {
                        s.edit_prefs(cx, |p| {
                            if !p.repos.contains(&url) {
                                p.repos.push(url.clone())
                            }
                        })
                    });
                    this.refresh(cx);
                    this.note = "".into();
                    input.update(cx, |i, cx| {
                        i.text.clear();
                        cx.notify();
                    });
                }
                cx.notify();
            }),
        ];
        Settings { plugins, store, repo, note: "".into(), _subs: subs }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let repos = self.store.read(cx).prefs().repos.clone();
        self.plugins.update(cx, |p, cx| p.refresh_index(repos, cx));
    }

    fn section(title: &'static str) -> gpui::Div {
        div().flex().flex_col().gap(u(10.)).pt(u(24.)).child(div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(title))
    }

    fn stepper(id: &'static str, label: &'static str, value: String, cx: &mut Context<Self>, minus: fn(&mut super::store::Prefs), plus: fn(&mut super::store::Prefs)) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(u(160.)).child(label))
            .child(small_button((id, 0usize), "−", false).on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.edit_prefs(cx, minus)))))
            .child(div().w(u(64.)).flex().justify_center().font_family(crate::fonts::mono()).child(value))
            .child(small_button((id, 1usize), "+", false).on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.edit_prefs(cx, plus)))))
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = self.store.read(cx).prefs().clone();
        div()
            .id("ln-settings")
            .size_full()
            .overflow_y_scroll()
            .px(u(PAD))
            .pt(u(28.))
            .pb(u(48.))
            .child(heading("Settings", "light novels"))
            .child(
                Self::section("READER")
                    .child(Self::stepper("fs", "Text size", format!("{}", prefs.font_size), cx, |p| p.font_size = (p.font_size - 1.).max(12.), |p| p.font_size = (p.font_size + 1.).min(40.)))
                    .child(Self::stepper("lh", "Line spacing", format!("{:.1}", prefs.line_height), cx, |p| p.line_height = (p.line_height - 0.1).max(1.2), |p| p.line_height = (p.line_height + 0.1).min(2.4)))
                    .child(Self::stepper("w", "Column width", format!("{}", prefs.width), cx, |p| p.width = (p.width - 40.).max(400.), |p| p.width = (p.width + 40.).min(1200.)))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child("In the reader: arrows scroll, left and right change chapter, Esc goes back.")),
            )
            .child(
                Self::section("PLUGIN REPOS")
                    .children(prefs.repos.iter().enumerate().map(|(i, r)| {
                        let url = r.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().flex_1().min_w_0().truncate().font_family(crate::fonts::mono()).text_size(u(13.)).child(r.clone()))
                            .child(small_button(("ln-repo-rm", i), "Remove", false).on_click(cx.listener(move |this, _, _, cx| {
                                let url = url.clone();
                                this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.repos.retain(|r| *r != url)));
                                this.refresh(cx);
                            })))
                    }))
                    .child(div().w(u(520.)).max_w_full().child(self.repo.clone()))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child("Press Enter to add a repo. Plugins that need AES or Protobuf aren't supported yet."))
                    .children((!self.note.is_empty()).then(|| div().text_color(rgb(ACCENT_TEXT)).child(self.note.clone())))
                    .child(
                        div().flex().gap_2().child(small_button("ln-repo-default", "Use LNReader's list", false).on_click(cx.listener(|this, _, _, cx| {
                            this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.repos = vec![DEFAULT_REPO.into()]));
                            this.refresh(cx);
                        }))),
                    ),
            )
            .child(
                Self::section("STORAGE").child(div().flex().items_center().gap_3().child(div().w(u(260.)).child("Saved chapters")).child(small_button("ln-cache", "Clear", false).on_click(cx.listener(|_, _, _, _| super::clear_chapter_cache())))),
            )
            .child(div().pt(u(24.)).h(u(1.)).border_t_1().border_color(rgb(LINE)).font_weight(FontWeight::NORMAL))
    }
}
