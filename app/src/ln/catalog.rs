//! The Extensions screen, laid out like manga's.

use super::plugins::{Entry, Plugins};
use super::store::LnStore;
use crate::catalog::{chip, small_button};
use crate::covers::{Covers, Fetch, grid};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::segmented::Segmented;
use crate::{ACCENT_TEXT, FAINT, MUTED, RULE, SURFACE, heading, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ObjectFit, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Window, div, img, list, prelude::FluentBuilder, rgb,
};
use std::collections::HashMap;

const PAD: f32 = 40.;
const GAP: f32 = 16.;
const CARD_MIN: f32 = 300.;

#[derive(Clone, Copy)]
enum Section {
    Installed,
    Available,
}

#[derive(Clone, Copy)]
enum Item {
    Label(Section),
    Installed(usize),
    Row(usize),
    Empty,
}

pub struct Catalog {
    plugins: Entity<Plugins>,
    store: Entity<LnStore>,
    cache: Entity<ImageCache>,
    search: Entity<TextInput>,
    repo_input: Entity<TextInput>,
    tabs: Segmented,
    repo_error: Option<String>,
    icons: Covers,
    list: ListState,
    /// The list caches row heights, so re-measure after a zoom change.
    zoom_seen: f32,
    items: Vec<Item>,
    installed: Vec<Entry>,
    available: Vec<Entry>,
    installed_open: bool,
    available_open: bool,
    cols: usize,
    card_w: f32,
    lang: Option<String>,
    _subs: Vec<Subscription>,
}

impl Catalog {
    pub fn new(plugins: Entity<Plugins>, store: Entity<LnStore>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search extensions…", cx));
        let repo_input = cx.new(|cx| {
            let mut i = TextInput::new("Paste a repo address, like …/plugins.min.json", cx);
            i.icon = false;
            i
        });
        let subs = vec![
            cx.observe(&plugins, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&repo_input, |this, _, _: &InputEvent, cx| this.add_repo(cx)),
        ];
        Catalog {
            plugins,
            store,
            cache,
            search,
            repo_input,
            tabs: Segmented::new(&["Extensions", "Repos"], 0),
            repo_error: None,
            icons: Covers::default(),
            zoom_seen: 0.,
            list: ListState::new(0, ListAlignment::Top, u(600.)),
            items: vec![],
            installed: vec![],
            available: vec![],
            installed_open: true,
            available_open: true,
            cols: 1,
            card_w: CARD_MIN,
            lang: None,
            _subs: subs,
        }
    }

    fn add_repo(&mut self, cx: &mut Context<Self>) {
        let url = self.repo_input.read(cx).text.trim().to_string();
        if url.is_empty() {
            return;
        }
        if !url.starts_with("https://") {
            self.repo_error = Some("A repo address starts with https://".into());
            cx.notify();
            return;
        }
        self.repo_error = None;
        self.store.update(cx, |s, cx| {
            s.edit_prefs(cx, |p| {
                if !p.repos.contains(&url) {
                    p.repos.push(url.clone())
                }
            })
        });
        self.repo_input.update(cx, |i, _| i.text.clear());
        self.refresh(cx);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let repos = self.store.read(cx).prefs().repos.clone();
        self.plugins.update(cx, |p, cx| p.refresh_index(repos, cx));
    }

    fn repos(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let repos = self.store.read(cx).prefs().repos.clone();
        let status = self.plugins.read(cx).status.clone();
        let rows = repos.into_iter().map(|url| {
            let host = url.trim_start_matches("https://").split('/').next().unwrap_or("").to_string();
            let rm = url.clone();
            div()
                .py(u(10.))
                .flex()
                .items_center()
                .gap_4()
                .border_b_1()
                .border_color(rgb(RULE))
                .child(
                    div().flex_1().min_w_0().flex().flex_col().gap(u(2.)).child(
                        div().flex().items_baseline().gap_3().child(div().font_weight(FontWeight::SEMIBOLD).child(host)).child(
                            div().min_w_0().truncate().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(url.clone()),
                        ),
                    ),
                )
                .child(small_button(SharedString::from(format!("rm-repo-{url}")), "Remove", false).on_click(cx.listener(move |this, _, _, cx| {
                    let rm = rm.clone();
                    this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.repos.retain(|r| *r != rm)));
                    this.refresh(cx);
                })))
        });
        let note: SharedString = match (&self.repo_error, status.is_empty()) {
            (Some(e), _) => e.clone().into(),
            (None, false) => status,
            (None, true) => "The same plugin lists LNReader uses. Nano runs the plugins in each list you add.".into(),
        };
        let bad = self.repo_error.is_some() || !self.plugins.read(cx).status.is_empty();
        div()
            .w_full()
            .px(u(PAD))
            .pt(u(24.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().max_w(u(560.)).child(self.repo_input.clone()))
                    .child(small_button("add-repo", "Add repo", true).on_click(cx.listener(|this, _, _, cx| this.add_repo(cx)))),
            )
            .child(div().pt(u(8.)).text_size(u(13.)).text_color(rgb(if bad { ACCENT_TEXT } else { FAINT })).child(note))
            .child(div().mt(u(20.)).flex().flex_col().border_t_1().border_color(rgb(RULE)).children(rows))
            .into_any_element()
    }

    fn icon(&mut self, e: &Entry, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let url = Some(e.icon_url.as_str()).filter(|u| !u.is_empty());
        let image = self.icons.get(&self.cache, Some(Fetch::Icon), url, |c: &mut Catalog| &mut c.icons, cx);
        div()
            .size(u(size))
            .flex_none()
            .rounded(u(size / 4.))
            .overflow_hidden()
            .bg(rgb(SURFACE))
            .flex()
            .items_center()
            .justify_center()
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(MUTED))
            .map(|d| match image {
                Some(image) => d.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => d.child(e.name.chars().next().unwrap_or('?').to_uppercase().to_string()),
            })
            .into_any_element()
    }

    fn installed_row(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let e = self.installed[i].clone();
        let icon = self.icon(&e, 44., cx);
        let p = self.plugins.read(cx);
        let have = e.version.clone();
        let update = p.update_for(&e.id).cloned().map(|next| {
            let id = SharedString::from(format!("up-{}", e.id));
            if p.busy.contains(&e.id) {
                small_button(id, "Updating…", false).opacity(0.6)
            } else {
                small_button(id, "Update", true).on_click(cx.listener(move |this, _, _, cx| {
                    let next = next.clone();
                    this.plugins.update(cx, |p, cx| p.install(next, cx))
                }))
            }
        });
        let id = e.id.clone();
        let remove = small_button(SharedString::from(format!("rm-{}", e.id)), "Remove", false).on_click(cx.listener(move |this, _, _, cx| {
            let id = id.clone();
            this.plugins.update(cx, |p, cx| p.uninstall(&id, cx))
        }));
        div()
            .w_full()
            .px(u(PAD))
            .child(
                div()
                    .w_full()
                    .py(u(14.))
                    .flex()
                    .gap_4()
                    .border_b_1()
                    .border_color(rgb(RULE))
                    .child(icon)
                    .child(
                        div().flex_1().min_w_0().flex().flex_col().gap(u(8.)).child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap_3()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(e.name.clone()))
                                .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(format!("{} · v{have}", lang_of(&e)))),
                        ),
                    )
                    .children(update)
                    .child(remove),
            )
            .into_any_element()
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let e = self.available[i].clone();
        let icon = self.icon(&e, 40., cx);
        let busy = self.plugins.read(cx).busy.contains(&e.id);
        let id = SharedString::from(format!("add-{}", e.id));
        let button = if busy {
            small_button(id, "Installing…", false).opacity(0.6)
        } else {
            let entry = e.clone();
            small_button(id, "Install", true).on_click(cx.listener(move |this, _, _, cx| {
                let entry = entry.clone();
                this.plugins.update(cx, |p, cx| p.install(entry, cx))
            }))
        };
        div()
            .w(u(self.card_w))
            .p(u(12.))
            .flex()
            .items_center()
            .gap_3()
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(RULE))
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(2.))
                    .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(e.name.clone()))
                    .child(div().truncate().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(format!("{} · v{}", lang_of(&e), e.version))),
            )
            .child(button)
            .into_any_element()
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.items.get(ix).copied() {
            Some(Item::Label(section)) => {
                let (name, count, open) = match section {
                    Section::Installed => ("INSTALLED", self.installed.len(), self.installed_open),
                    Section::Available => ("AVAILABLE", self.available.len(), self.available_open),
                };
                div()
                    .id(name)
                    .px(u(PAD))
                    .pt(u(24.))
                    .pb(u(10.))
                    .flex()
                    .items_center()
                    .gap(u(6.))
                    .cursor_pointer()
                    .child(
                        gpui::svg()
                            .path("icons/chevron.svg")
                            .flex_none()
                            .size(u(14.))
                            .text_color(rgb(FAINT))
                            .when(!open, |d| d.with_transformation(gpui::Transformation::rotate(gpui::radians(-std::f32::consts::FRAC_PI_2)))),
                    )
                    .child(div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(format!("{name} · {count}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match section {
                            Section::Installed => this.installed_open = !this.installed_open,
                            Section::Available => this.available_open = !this.available_open,
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Some(Item::Installed(i)) => self.installed_row(i, cx),
            Some(Item::Row(r)) => {
                let cards: Vec<AnyElement> = (r * self.cols..((r + 1) * self.cols).min(self.available.len())).map(|i| self.card(i, cx)).collect();
                div().flex().gap(u(GAP)).px(u(PAD)).pb(u(GAP)).children(cards).into_any_element()
            }
            Some(Item::Empty) => {
                let text = if self.store.read(cx).prefs().repos.is_empty() {
                    "Add a repo in the Repos tab to see its extensions."
                } else if self.plugins.read(cx).loading {
                    "Loading the plugin list…"
                } else {
                    "No extensions match."
                };
                div().px(u(PAD)).py_4().text_color(rgb(MUTED)).child(text).into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

/// The language a plugin is written for, without the invisible direction mark some lists include.
fn lang_of(e: &Entry) -> String {
    e.lang.trim_matches('\u{200e}').to_string()
}

impl Render for Catalog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        let query = self.search.read(cx).text.trim().to_lowercase();
        let p = self.plugins.read(cx);
        let lang = self.lang.clone();
        let matches = |e: &Entry| (query.is_empty() || e.name.to_lowercase().contains(&query) || e.site.to_lowercase().contains(&query)) && lang.as_ref().is_none_or(|l| e.lang == *l);
        let mut installed: Vec<Entry> = p.installed.iter().filter(|e| query.is_empty() || e.name.to_lowercase().contains(&query)).cloned().collect();
        installed.sort_by_key(|e| e.name.to_lowercase());
        let mut available: Vec<Entry> = p.index.iter().filter(|e| !p.is_installed(&e.id) && matches(e)).cloned().collect();
        available.sort_by_key(|e| e.name.to_lowercase());
        (self.cols, self.card_w) = grid(crate::content_width(window), PAD, GAP, CARD_MIN);

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &p.index {
            *counts.entry(e.lang.as_str()).or_default() += 1;
        }
        let mut top: Vec<(&str, usize)> = counts.into_iter().collect();
        top.sort_by_key(|(l, n)| (*l != "English", std::cmp::Reverse(*n), *l));
        let top: Vec<String> = top.into_iter().take(12).map(|(l, _)| l.to_string()).collect();

        let note: SharedString = if p.loading {
            "loading the plugin list…".into()
        } else {
            format!("{} installed · {} in {} repos", p.installed.len(), p.index.len(), self.store.read(cx).prefs().repos.len()).replace("1 repos", "1 repo").into()
        };
        self.installed = installed;
        self.available = available;

        let mut items = vec![];
        if !self.installed.is_empty() {
            items.push(Item::Label(Section::Installed));
            if self.installed_open {
                items.extend((0..self.installed.len()).map(Item::Installed));
            }
        }
        items.push(Item::Label(Section::Available));
        if self.available_open {
            if self.available.is_empty() {
                items.push(Item::Empty);
            }
            items.extend((0..self.available.len().div_ceil(self.cols)).map(Item::Row));
        }
        if items.len() != self.list.item_count() {
            self.list.reset(items.len());
        }
        self.items = items;
        let this = cx.entity();
        let on_repos = self.tabs.selected == 1;

        let body = if on_repos {
            div().id("repos-scroll").flex_1().min_h_0().overflow_y_scroll().child(self.repos(cx)).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px(u(PAD))
                        .pt(u(16.))
                        .flex()
                        .flex_wrap()
                        .gap(u(6.))
                        .child(chip("lang-any", "all languages", lang.is_none()).on_click(cx.listener(|this, _, _, cx| {
                            this.lang = None;
                            cx.notify();
                        })))
                        .children(top.into_iter().map(|l| {
                            let on = lang.as_deref() == Some(l.as_str());
                            let shown = l.trim_matches('\u{200e}').to_string();
                            chip(SharedString::from(format!("filter-{l}")), shown, on).on_click(cx.listener(move |this, _, _, cx| {
                                this.lang = if on { None } else { Some(l.clone()) };
                                cx.notify();
                            }))
                        })),
                )
                .child(div().flex_1().min_h_0().child(list(self.list.clone(), move |ix, _, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full()))
                .into_any_element()
        };

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(u(PAD))
                    .pt(u(28.))
                    .flex()
                    .items_end()
                    .gap_4()
                    .child(div().flex_1().child(heading("Extensions", note)))
                    .when(!on_repos, |d| d.child(div().w(u(320.)).child(self.search.clone())))
                    .child(self.tabs.render(
                        "ln-catalog-tabs",
                        cx.listener(|this, i: &usize, _, cx| {
                            this.tabs.pick(*i);
                            cx.notify();
                        }),
                    )),
            )
            .child(body)
    }
}
