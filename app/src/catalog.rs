//! The Extensions screen: your repos, what's installed, then what the repos offer, filterable by name and language.

use crate::u;
use crate::covers::{Covers, Fetch, grid};
use crate::extensions::{self, Ext};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::registry::Registry;
use crate::segmented::Segmented;
use crate::store::Store;
use crate::{ACCENT_TEXT, BG, FAINT, HOVER, LINE, MUTED, RULE, SURFACE, TEXT, heading};
use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState,
    ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription,
    Window, div, img, list, prelude::FluentBuilder, rgb,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    registry: Entity<Registry>,
    store: Entity<Store>,
    cache: Entity<ImageCache>,
    search: Entity<TextInput>,
    repo_input: Entity<TextInput>,
    adding: bool,
    tabs: Segmented,
    repo_error: Option<String>,
    icons: Covers,
    list: ListState,
    /// The list caches row heights, so re-measure after a zoom change.
    zoom_seen: f32,
    items: Vec<Item>,
    installed: Vec<Ext>,
    available: Vec<Ext>,
    installed_open: bool,
    available_open: bool,
    cols: usize,
    card_w: f32,
    lang: Option<String>,
    /// An extension's settings drawer, while open.
    settings: Option<(Entity<crate::ext_settings::ExtSettings>, Subscription)>,
    nsfw: bool,
    data_gen: u64,
    index_key: Option<(usize, usize)>,
    index_len: usize,
    expanded: Vec<Ext>,
    known_pkgs: HashSet<String>,
    top_langs: Vec<String>,
    built_list: Option<(u64, String, Option<String>, bool)>,
    _subs: Vec<Subscription>,
}

impl Catalog {
    pub fn new(registry: Entity<Registry>, store: Entity<Store>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let repos = store.read(cx).repos().to_vec();
        registry.update(cx, |r, cx| r.refresh_index(repos, cx));
        let search = cx.new(|cx| TextInput::new("Search extensions…", cx));
        let repo_input = cx.new(|cx| {
            let mut i = TextInput::new("Paste a repo address, like …/index.min.json", cx);
            i.icon = false;
            i
        });
        let subs = vec![
            cx.observe(&registry, |this, _, cx| {
                this.data_gen += 1;
                cx.notify();
            }),
            cx.observe(&store, |this, _, cx| {
                this.data_gen += 1;
                cx.notify();
            }),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&repo_input, |this, _, _: &InputEvent, cx| this.add_repo(cx)),
        ];
        Catalog {
            registry,
            store,
            cache,
            search,
            repo_input,
            adding: false,
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
            settings: None,
            nsfw: false,
            data_gen: 0,
            index_key: None,
            index_len: 0,
            expanded: vec![],
            known_pkgs: HashSet::new(),
            top_langs: vec![],
            built_list: None,
            _subs: subs,
        }
    }

    fn add_repo(&mut self, cx: &mut Context<Self>) {
        let input = self.repo_input.read(cx).text.trim().to_string();
        if input.is_empty() || self.adding {
            return;
        }
        self.adding = true;
        self.repo_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { extensions::add_repo(&input) }).await;
            let _ = this.update(cx, |this, cx| {
                this.adding = false;
                match result {
                    Ok(repo) => {
                        this.store.update(cx, |s, cx| s.add_repo(repo, cx));
                        this.repo_input.update(cx, |i, _| i.text.clear());
                        this.refresh(cx);
                    }
                    Err(e) => this.repo_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let repos = self.store.read(cx).repos().to_vec();
        self.registry.update(cx, |r, cx| r.refresh_index(repos, cx));
    }

    fn repos(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let repos = self.store.read(cx).repos().to_vec();
        let errors = self.registry.read(cx).index_errors.clone();
        let rows = repos.into_iter().map(|repo| {
            let url = repo.url.clone();
            let error = errors.iter().find(|e| e.starts_with(&format!("{}: ", repo.name))).cloned();
            div()
                .py(u(10.))
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
                        .gap(u(2.))
                        .child(div().flex().items_baseline().gap_3().child(div().font_weight(FontWeight::SEMIBOLD).child(repo.name.clone())).child(
                            div().min_w_0().truncate().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(repo.url.trim_end_matches("/index.json").to_string()),
                        ))
                        .child(div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(format!("signed by {}…", &repo.key[..16])))
                        .children(error.map(|e| div().text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(e))),
                )
                .child(small_button(SharedString::from(format!("rm-repo-{url}")), "Remove", false).on_click(cx.listener(move |this, _, _, cx| {
                    let url = url.clone();
                    this.store.update(cx, |s, cx| s.remove_repo(&url, cx));
                    this.refresh(cx);
                })))
        });
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
                    .child(if self.adding {
                        small_button("add-repo", "Adding…", false).opacity(0.6)
                    } else {
                        small_button("add-repo", "Add repo", true).on_click(cx.listener(|this, _, _, cx| this.add_repo(cx)))
                    }),
            )
            .child(div().pt(u(8.)).text_size(u(13.)).text_color(rgb(if self.repo_error.is_some() { ACCENT_TEXT } else { FAINT })).child(
                self.repo_error.clone().unwrap_or_else(|| "The same repo addresses Mihon uses. Nano only installs extensions signed with the key a repo had when you added it.".into()),
            ))
            .child(div().mt(u(20.)).flex().flex_col().border_t_1().border_color(rgb(RULE)).children(rows))
            .into_any_element()
    }

    /// Every language the extension offers has its own settings, enabled ones first.
    fn open_settings(&mut self, pkg: &str, site: Option<&str>, cx: &mut Context<Self>) {
        let registry = self.registry.read(cx);
        let enabled = registry.enabled_languages(pkg, self.store.read(cx));
        let mut infos: Vec<_> = registry.sources.iter().filter(|s| s.pkg == pkg && site.is_none_or(|n| extensions::site_of(&s.name) == n)).cloned().collect();
        infos.sort_by_key(|s| (!enabled.contains(&s.lang), s.lang.clone()));
        let sources: Vec<_> = infos.iter().filter_map(|s| registry.source(&s.id)).collect();
        if sources.is_empty() {
            return;
        }
        let store = self.store.clone();
        let (pkg, shown) = (pkg.to_string(), enabled);
        let view = cx.new(|cx| crate::ext_settings::ExtSettings::new(sources, shown, pkg, store, cx));
        let sub = cx.subscribe(&view, |this, _, _: &crate::ext_settings::Close, cx| {
            this.settings = None;
            cx.notify();
        });
        self.settings = Some((view, sub));
        cx.notify();
    }

    fn add(&mut self, e: &Ext, cx: &mut Context<Self>) {
        let installed = self.registry.read(cx).installed.contains(&e.pkg);
        if let Some(site) = &e.site {
            let mut on = if installed { self.store.read(cx).sites(&e.pkg).cloned().unwrap_or_else(|| e.sites.clone()) } else { vec![] };
            on.push(site.clone());
            self.store.update(cx, |s, cx| s.set_sites(&e.pkg, on, cx));
        }
        if !installed {
            self.registry.update(cx, |r, cx| r.install(e.clone(), cx));
        }
    }

    fn remove(&mut self, e: &Ext, cx: &mut Context<Self>) {
        if let Some(site) = &e.site {
            let mut on = self.store.read(cx).sites(&e.pkg).cloned().unwrap_or_else(|| e.sites.clone());
            on.retain(|s| s != site);
            if !on.is_empty() {
                self.store.update(cx, |s, cx| s.set_sites(&e.pkg, on, cx));
                return;
            }
        }
        self.registry.update(cx, |r, cx| r.uninstall(&e.pkg, cx));
    }

    fn icon(&mut self, e: &Ext, size: f32, cx: &mut Context<Self>) -> AnyElement {
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
        let registry = self.registry.read(cx);
        let langs = registry.languages(&e.pkg);
        let multi = langs.len() > 1;
        // Drop the stack trace the host adds.
        let error = registry.errors.get(&e.pkg).map(|err| format!("Couldn't load: {}", err.lines().next().unwrap_or(err)));
        let have = registry.versions.get(&e.pkg).cloned().unwrap_or_else(|| e.version.clone());
        let update = (!e.key.is_empty() && have != e.version).then(|| {
            let id = SharedString::from(format!("up-{}", row_key(&e)));
            if registry.busy.contains(&e.pkg) {
                small_button(id, "Updating…", false).opacity(0.6)
            } else {
                let ext = e.clone();
                small_button(id, "Update", true).on_click(cx.listener(move |this, _, _, cx| this.registry.update(cx, |r, cx| r.install(ext.clone(), cx))))
            }
        });
        let (pkg_settings, site_settings) = (e.pkg.clone(), e.site.clone());
        let settings = (!registry.sources.iter().all(|s| s.pkg != e.pkg)).then(|| {
            small_button(SharedString::from(format!("set-{}", row_key(&e))), "Settings", false).on_click(cx.listener(move |this, _, _, cx| this.open_settings(&pkg_settings, site_settings.as_deref(), cx)))
        });
        let row = e.clone();
        let remove = small_button(SharedString::from(format!("rm-{}", row_key(&e))), "Remove", false).on_click(cx.listener(move |this, _, _, cx| this.remove(&row, cx)));
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
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(8.))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_3()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(e.name.clone()))
                                    .child(
                                        div()
                                            .font_family(crate::fonts::mono())
                                            .text_size(u(12.))
                                            .text_color(rgb(FAINT))
                                            .child(if multi { format!("v{have}") } else { format!("{} · v{have}", e.langs.join(" ")) }),
                                    ),
                            )
                            .children(error.map(|err| div().text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(err))),
                    )
                    .children(update)
                    .children(settings)
                    .child(remove),
            )
            .into_any_element()
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let e = self.available[i].clone();
        let icon = self.icon(&e, 40., cx);
        let busy = self.registry.read(cx).busy.contains(&e.pkg);
        let id = SharedString::from(format!("add-{}", row_key(&e)));
        let button = if busy {
            small_button(id, "Installing…", false).opacity(0.6)
        } else {
            let ext = e.clone();
            small_button(id, "Install", true).on_click(cx.listener(move |this, _, _, cx| this.add(&ext, cx)))
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(e.name.clone()))
                            .when(e.nsfw, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px(u(5.))
                                        .rounded(u(4.))
                                        .border_1()
                                        .border_color(rgb(LINE))
                                        .font_family(crate::fonts::mono())
                                        .text_size(u(10.))
                                        .text_color(rgb(MUTED))
                                        .child("18+"),
                                )
                            }),
                    )
                    .child(div().truncate().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(format!("{} · v{}", e.langs.join(" "), e.version))),
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
                let cards: Vec<AnyElement> =
                    (r * self.cols..((r + 1) * self.cols).min(self.available.len())).map(|i| self.card(i, cx)).collect();
                div().flex().gap(u(GAP)).px(u(PAD)).pb(u(GAP)).children(cards).into_any_element()
            }
            Some(Item::Empty) => {
                let text = if self.store.read(cx).repos().is_empty() {
                    "Add a repo in the Repos tab to see its extensions. Nano doesn't come with one."
                } else {
                    "No extensions match."
                };
                div().px(u(PAD)).py_4().text_color(rgb(MUTED)).child(text).into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

pub fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(u(26.))
        .px(u(10.))
        .rounded(u(13.))
        .flex()
        .items_center()
        .border_1()
        .font_family(crate::fonts::mono())
        .text_size(u(12.))
        .cursor_pointer()
        .when(on, |d| d.bg(rgb(TEXT)).border_color(rgb(TEXT)).text_color(rgb(BG)))
        .when(!on, |d| d.border_color(rgb(LINE)).text_color(rgb(MUTED)).hover(|s| s.bg(rgb(HOVER))))
        .child(label.into())
}

pub fn small_button(id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(u(30.))
        .px(u(14.))
        .rounded(u(15.))
        .flex()
        .items_center()
        .text_size(u(13.))
        .cursor_pointer()
        .border_1()
        .border_color(rgb(LINE))
        .text_color(rgb(if primary { ACCENT_TEXT } else { MUTED }))
        .font_weight(if primary { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
        .hover(|s| s.bg(rgb(HOVER)))
        .child(label.into())
}

fn row_key(e: &Ext) -> String {
    format!("{}-{}", e.pkg, e.site.as_deref().unwrap_or(""))
}

impl Render for Catalog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        let query = self.search.read(cx).text.trim().to_lowercase();
        let index_opt = self.registry.read(cx).index.clone();
        let next_key = index_opt.as_ref().map(|idx| (Arc::as_ptr(idx) as usize, idx.len()));
        if self.index_key != next_key {
            self.index_key = next_key;
            self.data_gen += 1;
            if let Some(index) = index_opt.as_deref() {
                self.index_len = index.len();
                self.expanded = index.iter().flat_map(Ext::rows).collect();
                self.known_pkgs = index.iter().map(|e| e.pkg.clone()).collect();
                let mut counts: HashMap<&str, usize> = HashMap::new();
                for l in index.iter().flat_map(|e| e.langs.iter()).filter(|l| *l != "all") {
                    *counts.entry(l.as_str()).or_default() += 1;
                }
                let mut top: Vec<(&str, usize)> = counts.into_iter().collect();
                top.sort_by_key(|(l, n)| (std::cmp::Reverse(*n), *l));
                self.top_langs = top.into_iter().take(12).map(|(l, _)| l.to_string()).collect();
            } else {
                self.index_len = 0;
                self.expanded.clear();
                self.known_pkgs.clear();
                self.top_langs.clear();
            }
        }
        let list_key = (self.data_gen, query, self.lang.clone(), self.nsfw);
        if self.built_list.as_ref() != Some(&list_key) {
            let query = &list_key.1;
            let registry = self.registry.read(cx);
            let store = self.store.read(cx);
            let flags = store.prefs().nsfw;
            let (hide, mixed) = (flags.get(crate::store::Area::Extensions), flags.mixed);
            let flagged = |e: &Ext| hide && (e.nsfw || (mixed && e.mixed));
            let is_on = |r: &Ext| registry.installed.contains(&r.pkg) && r.site.as_ref().is_none_or(|s| store.sites(&r.pkg).is_none_or(|on| on.contains(s)));
            let matches = |e: &Ext| {
                (query.is_empty() || e.name.to_lowercase().contains(query))
                    && list_key.2.as_ref().is_none_or(|l| e.langs.iter().any(|x| x == l || x == "all"))
                    && (list_key.3 || !e.nsfw) && !flagged(e)
            };
            self.installed.clear();
            self.available.clear();
            for r in &self.expanded {
                if is_on(r) {
                    if !flagged(r) {
                        self.installed.push(r.clone());
                    }
                } else if matches(r) {
                    self.available.push(r.clone());
                }
            }
            let mut orphans: Vec<&String> = registry.installed.iter().filter(|p| !self.known_pkgs.contains(*p)).collect();
            orphans.sort();
            for pkg in orphans {
                let name = registry.sources.iter().find(|s| &s.pkg == pkg).map_or_else(|| pkg.rsplit('.').next().unwrap_or(pkg).to_string(), |s| s.name.clone());
                let langs = registry.languages(pkg);
                self.installed.push(Ext::orphan(pkg, &name, registry.versions.get(pkg).map_or("", |v| v.as_str()), langs));
            }
            self.built_list = Some(list_key);
        }
        (self.cols, self.card_w) = grid(crate::content_width(window), PAD, GAP, CARD_MIN);
        let (lang, nsfw) = (self.lang.clone(), self.nsfw);
        let top = &self.top_langs;

        let repos = self.store.read(cx).repos().len();
        let note = match repos {
            0 => format!("{} installed · no repos yet", self.installed.len()),
            1 => format!("{} installed · {} in 1 repo", self.installed.len(), self.index_len),
            n => format!("{} installed · {} in {n} repos", self.installed.len(), self.index_len),
        };

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
                        .children(top.iter().map(|l| {
                            let on = lang.as_deref() == Some(l.as_str());
                            let l_click = l.clone();
                            chip(SharedString::from(format!("filter-{l}")), l.clone(), on).on_click(cx.listener(move |this, _, _, cx| {
                                this.lang = if on { None } else { Some(l_click.clone()) };
                                cx.notify();
                            }))
                        }))
                        .child(div().w(u(12.)))
                        .child(chip("nsfw", "show 18+", nsfw).on_click(cx.listener(|this, _, _, cx| {
                            this.nsfw = !this.nsfw;
                            cx.notify();
                        }))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(list(self.list.clone(), move |ix, _, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full()),
                )
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
                    .child(self.tabs.render("catalog-tabs", cx.listener(|this, i: &usize, _, cx| {
                        this.tabs.pick(*i);
                        cx.notify();
                    }))),
            )
            .child(body)
            .children(self.settings.as_ref().map(|(v, _)| v.clone()))
    }
}
