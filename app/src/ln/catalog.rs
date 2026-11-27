//! The plugins in your repos, by language, with install, update and remove. Only LNReader plugins show up here.

use super::PAD;
use super::plugins::{Entry, Plugins};
use super::store::LnStore;
use crate::browse::segment;
use crate::catalog::small_button;
use crate::covers::{Covers, Fetch};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::{ACCENT_TEXT, FAINT, HOVER, LINE, MUTED, SURFACE, heading, u};
use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription,
    Window, div, prelude::FluentBuilder, rgb, uniform_list,
};
use std::ops::Range;

/// What a row offers.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Install,
    Update,
    Remove,
    Busy,
}

const INSTALLED: &str = "Installed";

pub struct Catalog {
    plugins: Entity<Plugins>,
    store: Entity<LnStore>,
    cache: Entity<ImageCache>,
    icons: Covers,
    search: Entity<TextInput>,
    rows: Vec<(Entry, Action)>,
    _subs: Vec<Subscription>,
}

impl Catalog {
    pub fn new(plugins: Entity<Plugins>, store: Entity<LnStore>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search plugins…", cx));
        let subs = vec![cx.observe(&plugins, |_, _, cx| cx.notify()), cx.observe(&store, |_, _, cx| cx.notify()), cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify())];
        Catalog { plugins, store, cache, icons: Covers::default(), search, rows: vec![], _subs: subs }
    }

    fn row(&mut self, i: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (entry, action) = self.rows[i].clone();
        let icon = self.icons.get(&self.cache, Some(Fetch::Icon), Some(entry.icon_url.as_str()).filter(|u| !u.is_empty()), |c: &mut Catalog| &mut c.icons, cx);
        let initial = entry.name.chars().next().unwrap_or('?').to_uppercase().to_string();
        let (label, primary) = match action {
            Action::Install => ("Install", true),
            Action::Update => ("Update", true),
            Action::Remove => ("Remove", false),
            Action::Busy => ("Working…", false),
        };
        let id = entry.id.clone();
        div()
            .id(("ln-plugin", i))
            .mx(u(PAD))
            .h(u(60.))
            .px(u(12.))
            .rounded(u(8.))
            .flex()
            .items_center()
            .gap_4()
            .hover(|s| s.bg(rgb(HOVER)))
            .child(
                div()
                    .flex_none()
                    .size(u(36.))
                    .rounded(u(8.))
                    .overflow_hidden()
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(MUTED))
                    .map(|d| match icon {
                        Some(image) => d.child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Cover)),
                        None => d.child(initial),
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(format!("{}  {}", entry.name, entry.version)))
                    .child(div().truncate().text_size(u(12.5)).text_color(rgb(FAINT)).child(format!("{} · {}", entry.lang.trim_matches('\u{200e}'), entry.site.trim_start_matches("https://").trim_end_matches('/')))),
            )
            .child(small_button(("ln-plugin-act", i), label, primary).on_click(cx.listener(move |this, _, _, cx| {
                let entry = this.rows.iter().find(|(e, _)| e.id == id).map(|(e, a)| (e.clone(), *a));
                let Some((entry, action)) = entry else { return };
                match action {
                    Action::Install | Action::Update => this.plugins.update(cx, |p, cx| p.install(entry, cx)),
                    Action::Remove => this.plugins.update(cx, |p, cx| p.uninstall(&entry.id, cx)),
                    Action::Busy => {}
                }
            })))
            .into_any_element()
    }
}

impl Render for Catalog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = self.store.read(cx).prefs().language.clone();
        let query = self.search.read(cx).text.trim().to_lowercase();
        let p = self.plugins.read(cx);
        let action = |id: &str| {
            if p.busy.contains(id) {
                Action::Busy
            } else if p.update_for(id).is_some() {
                Action::Update
            } else if p.is_installed(id) {
                Action::Remove
            } else {
                Action::Install
            }
        };
        let mut langs: Vec<(String, usize)> = vec![];
        for e in &p.index {
            match langs.iter_mut().find(|(l, _)| *l == e.lang) {
                Some((_, n)) => *n += 1,
                None => langs.push((e.lang.clone(), 1)),
            }
        }
        langs.sort_by_key(|(l, n)| (l != "English", std::cmp::Reverse(*n), l.clone()));
        let source: Vec<&Entry> = if language == INSTALLED {
            p.installed.iter().map(|i| p.update_for(&i.id).unwrap_or(i)).collect()
        } else {
            p.index.iter().filter(|e| e.lang == language).collect()
        };
        let mut rows: Vec<(Entry, Action)> =
            source.into_iter().filter(|e| query.is_empty() || e.name.to_lowercase().contains(&query) || e.site.to_lowercase().contains(&query)).map(|e| (e.clone(), action(&e.id))).collect();
        rows.sort_by_key(|(e, a)| (*a == Action::Install, e.name.to_lowercase()));
        let note: SharedString = if p.loading || !p.status.is_empty() {
            p.status.clone()
        } else {
            format!("{} available · {} installed", p.index.len(), p.installed.len()).into()
        };
        let (loading, failed) = (p.loading, !p.status.is_empty() && !p.loading);
        let updates = p.installed.iter().filter(|i| p.update_for(&i.id).is_some()).count();
        self.rows = rows;
        let count = self.rows.len();
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
                    .flex_wrap()
                    .items_end()
                    .gap_4()
                    .child(div().flex_none().max_w_full().child(heading("Extensions", note)))
                    .child(
                        div()
                            .ml_auto()
                            .flex()
                            .gap_2()
                            .items_center()
                            .when(updates > 0, |d| {
                                d.child(small_button("ln-update-all", format!("Update {updates}"), true).on_click(cx.listener(|this, _, _, cx| {
                                    let p = this.plugins.read(cx);
                                    let list: Vec<Entry> = p.installed.iter().filter_map(|i| p.update_for(&i.id).cloned()).collect();
                                    this.plugins.update(cx, |p, cx| list.into_iter().for_each(|e| p.install(e, cx)));
                                })))
                            })
                            .child(small_button("ln-refresh", if loading { "Loading…" } else { "Refresh" }, false).on_click(cx.listener(|this, _, _, cx| {
                                let repos = this.store.read(cx).prefs().repos.clone();
                                this.plugins.update(cx, |p, cx| p.refresh_index(repos, cx));
                            }))),
                    )
                    .child(div().w(u(300.)).max_w_full().child(self.search.clone())),
            )
            .child(
                div()
                    .id("ln-langs")
                    .px(u(PAD))
                    .pb(u(12.))
                    .flex()
                    .flex_wrap()
                    .gap(u(4.))
                    .child(segment("ln-lang-installed", INSTALLED, language == INSTALLED).on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.language = INSTALLED.into()));
                    })))
                    .children(langs.into_iter().take(40).map(|(l, n)| {
                        let on = l == language;
                        let name = l.clone();
                        segment(SharedString::from(format!("ln-lang-{l}")), format!("{} · {n}", l.trim_matches('\u{200e}')), on).on_click(cx.listener(move |this, _, _, cx| {
                            let name = name.clone();
                            this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.language = name));
                        }))
                    })),
            )
            .children(failed.then(|| div().px(u(PAD)).pb(u(8.)).text_color(rgb(ACCENT_TEXT)).child("Couldn't load the plugin list. Check your connection, then press Refresh.")))
            .child(
                uniform_list(
                    "ln-plugins",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        let n = this.rows.len();
                        range.filter(|i| *i < n).map(|i| this.row(i, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
    }
}
