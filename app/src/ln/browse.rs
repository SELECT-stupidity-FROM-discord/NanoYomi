//! One source's popular, latest or search results as a cover grid that loads more as you scroll.

use super::plugins::{Item, Plugins, Source};
use super::store::LnStore;
use super::{GAP, OpenNovel, PAD, card, cover_fetch, notice};
use crate::catalog::small_button;
use crate::covers::{CARD_W, Covers, grid};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::segmented::Segmented;
use crate::{ACCENT_TEXT, heading, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::FluentBuilder, rgb, uniform_list,
};
use std::ops::Range;

#[derive(Clone, Copy, PartialEq)]
enum Listing {
    Popular,
    Latest,
    Search,
}

pub struct Browse {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    cache: Entity<ImageCache>,
    covers: Covers,
    search: Entity<TextInput>,
    switch: Segmented,
    source: Option<Source>,
    listing: Listing,
    query: String,
    items: Vec<Item>,
    page: u32,
    has_next: bool,
    loading: bool,
    failed: bool,
    /// Nothing is fetched until the screen has been opened once.
    active: bool,
    /// Bumped on a source or listing change so late pages get dropped.
    generation: u64,
    status: SharedString,
    card_w: f32,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OpenNovel> for Browse {}

impl Browse {
    pub fn new(store: Entity<LnStore>, plugins: Entity<Plugins>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search this source…", cx));
        let subs = vec![
            cx.subscribe(&search, |this, input, _: &InputEvent, cx| {
                this.query = input.read(cx).text.trim().to_string();
                let listing = if this.query.is_empty() { Listing::Popular } else { Listing::Search };
                this.show(listing, cx);
            }),
            cx.observe(&plugins, |this, _, cx| this.check_source(cx)),
        ];
        let mut this = Browse {
            store,
            plugins,
            cache,
            covers: Covers::default(),
            search,
            switch: Segmented::new(&["Popular", "Latest"], 0),
            source: None,
            listing: Listing::Popular,
            query: String::new(),
            items: vec![],
            page: 0,
            has_next: false,
            loading: false,
            failed: false,
            active: false,
            generation: 0,
            status: "".into(),
            card_w: CARD_W,
            _subs: subs,
        };
        this.check_source(cx);
        this
    }

    /// Picks a source when there's none, and drops one that was uninstalled.
    fn check_source(&mut self, cx: &mut Context<Self>) {
        let first = {
            let p = self.plugins.read(cx);
            let gone = self.source.as_ref().is_some_and(|s| !p.is_installed(&s.id));
            if self.source.is_some() && !gone {
                return;
            }
            self.source = None;
            p.installed.iter().find(|e| e.lang == "English").or(p.installed.first()).map(|e| e.id.clone())
        };
        match first {
            Some(id) => self.select(&id, cx),
            None => {
                self.items.clear();
                cx.notify();
            }
        }
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source.as_ref().map(|s| s.id.as_str())
    }

    /// Called when the screen is shown, which is when the first page loads.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.source.is_some() && self.items.is_empty() && self.page == 0 {
            self.load_next_page(cx);
        }
    }

    pub fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.active = true;
        self.source = self.plugins.update(cx, |p, _| p.source(id));
        let listing = if self.listing == Listing::Search && self.query.is_empty() { Listing::Popular } else { self.listing };
        self.show(listing, cx);
    }

    fn show(&mut self, listing: Listing, cx: &mut Context<Self>) {
        self.listing = listing;
        self.generation += 1;
        self.items.clear();
        self.page = 0;
        self.has_next = false;
        self.loading = false;
        self.failed = false;
        self.load_next_page(cx);
    }

    fn load_next_page(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else { return };
        if !self.active || self.loading || self.failed || (self.page > 0 && !self.has_next) {
            return;
        }
        self.loading = true;
        self.status = "loading…".into();
        cx.notify();
        let (page, generation, listing, query) = (self.page + 1, self.generation, self.listing, self.query.clone());
        cx.spawn(async move |this, cx| {
            let result = super::guarded(cx, || async {
                match listing {
                    Listing::Popular => source.popular(page, false).await,
                    Listing::Latest => source.popular(page, true).await,
                    Listing::Search => source.search(&query, page).await,
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(list) => {
                        this.page = page;
                        // Sites repeat their last page past the end, so a page of nothing new is the end.
                        let fresh: Vec<Item> = list.into_iter().filter(|i| !this.items.iter().any(|o| o.path == i.path)).collect();
                        this.has_next = !fresh.is_empty();
                        this.items.extend(fresh);
                        this.status = if this.items.is_empty() { "nothing found".into() } else { "".into() };
                    }
                    Err(e) => {
                        this.status = e.lines().next().unwrap_or("The source failed").to_string().into();
                        this.failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(source) = self.source.clone() else { return div().into_any_element() };
        let item = self.items[i].clone();
        let how = cover_fetch(self.plugins.read(cx), &source.id);
        let image = self.covers.get(&self.cache, how, Some(item.cover.as_str()).filter(|c| !c.is_empty()), |b: &mut Browse| &mut b.covers, cx);
        let in_library = self.store.read(cx).get(&source.id, &item.path).is_some_and(|n| n.in_library);
        let lines = if in_library { vec!["In library".to_string()] } else { vec![] };
        card(("ln-browse", i), image, &item.name, self.card_w, 0, lines)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let (Some(s), Some(item)) = (this.source.as_ref(), this.items.get(i)) {
                    cx.emit(OpenNovel { source: s.id.clone(), path: item.path.clone() });
                }
            }))
            .into_any_element()
    }
}

impl Render for Browse {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (cols, card_w) = grid(crate::content_width(window), PAD, GAP, CARD_W);
        self.card_w = card_w;
        let rows = self.items.len().div_ceil(cols);
        let title = self.source.as_ref().map_or("Browse".to_string(), |s| s.name.clone());
        let note: SharedString = if self.source.is_none() {
            "no sources installed — add some in Extensions".into()
        } else if !self.status.is_empty() && !self.failed {
            self.status.clone()
        } else {
            format!("{} novels", self.items.len()).into()
        };
        let at = if self.listing == Listing::Latest { 1 } else { 0 };
        self.switch.set(&["Popular", "Latest"], at);
        let broken = self.failed && self.items.is_empty();
        let empty = self.source.is_some() && !self.loading && !self.failed && self.page > 0 && self.items.is_empty();
        let retry = |cx: &mut Context<Self>| {
            small_button("ln-retry", "Try again", true).on_click(cx.listener(|this, _, _, cx| {
                this.failed = false;
                this.load_next_page(cx);
            }))
        };
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
                    .child(div().flex_none().max_w_full().child(heading(&title, note)))
                    .child(div().ml_auto().flex().gap_2().child(self.switch.render("ln-listing", cx.listener(|this, i: &usize, _, cx| {
                        this.switch.pick(*i);
                        this.show(if *i == 1 { Listing::Latest } else { Listing::Popular }, cx);
                    }))))
                    .child(div().w(u(300.)).max_w_full().child(self.search.clone())),
            )
            .when(self.failed && !broken, |d| d.child(div().px(u(PAD)).pt(u(16.)).flex().items_center().gap_3().child(div().text_color(rgb(ACCENT_TEXT)).child(self.status.clone())).child(retry(cx))))
            .child(if broken {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .pb(u(80.))
                    .child(div().text_size(u(48.)).text_color(rgb(crate::MUTED)).child("( x_x )"))
                    .child(div().text_size(u(18.)).child("The shelf won't open"))
                    .child(div().max_w(u(560.)).text_color(rgb(ACCENT_TEXT)).child(self.status.clone()))
                    .child(div().pt(u(12.)).child(retry(cx)))
                    .into_any_element()
            } else if empty || self.source.is_none() {
                let hint = if self.source.is_none() { "Install a plugin in Extensions, then pick it here.".to_string() } else { format!("{title} has nothing here. Try another listing.") };
                notice("The shelf is empty", hint).into_any_element()
            } else {
                uniform_list(
                    "ln-browse-grid",
                    rows,
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        if range.end + 2 >= rows {
                            cx.defer_in(window, |this, _, cx| this.load_next_page(cx));
                        }
                        range
                            .map(|row| {
                                let cards: Vec<AnyElement> = (row * cols..((row + 1) * cols).min(this.items.len())).map(|i| this.card(i, cx)).collect();
                                div().flex().gap(u(GAP)).px(u(PAD)).py(u(GAP / 2.)).children(cards)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
    }
}
