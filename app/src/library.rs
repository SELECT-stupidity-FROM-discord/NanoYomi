//! The library screen: what you're reading, your saved series, and their new chapters.

use crate::u;
use crate::browse::OpenManga;
use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover, grid};
use crate::images::ImageCache;
use crate::registry::Registry;
use crate::store::{Series, Store, ago, now_ms};
use crate::segmented::Segmented;
use gpui::AnimationExt;
use crate::{ACCENT, ACCENT_TEXT, FAINT, LINE, MUTED, RULE, SURFACE, Source, TEXT, heading, pill};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div, list,
    prelude::FluentBuilder, relative, rgb,
};
use serde_json::{Value, json};
use std::sync::Arc;

const PAD: f32 = 40.;
const GAP: f32 = 20.;
/// How many recent chapters the Updates page lists.
const UPDATES: usize = 100;

pub struct Resume {
    pub source: Source,
    pub manga: Value,
    pub chapters: Arc<Vec<Value>>,
    pub index: usize,
    pub page: usize,
}

pub struct GoBrowse;

#[derive(Clone, Copy)]
enum Item {
    Heading,
    Shelf,
    Controls,
    Empty,
    NoMatch,
    Row(usize),
    Line(usize),
    /// A day heading on the Updates page, then that day's series.
    Day(usize),
    /// A row of drops from one day, as a range into `drops`.
    Drops(usize, usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    New,
    Done,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    LastRead,
    MostNew,
    Title,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::LastRead => "Last read",
            Sort::MostNew => "Most new chapters",
            Sort::Title => "Title",
        }
    }

    const ALL: [Sort; 3] = [Sort::LastRead, Sort::MostNew, Sort::Title];
}

/// One series' chapters that came out on the same day.
struct Drop {
    at: i64,
    day: String,
    series: usize,
    chapters: Vec<usize>,
}

pub struct Library {
    /// The same data drives the Updates page, so new chapters are only checked once.
    pub updates_page: bool,
    page_seen: bool,
    store: Entity<Store>,
    registry: Entity<Registry>,
    cache: Entity<ImageCache>,
    covers: Covers,
    list: ListState,
    /// The list caches row heights, so re-measure after a zoom change.
    zoom_seen: f32,
    items: Vec<Item>,
    cols: usize,
    card_w: f32,
    width: f32,
    /// This frame's library, plus the (series, chapter) pairs for Latest Updates.
    /// The library first, then the series being read if it isn't saved, for the hero.
    series: Vec<Series>,
    library_len: usize,
    updates: Vec<(usize, usize)>,
    drops: Vec<Drop>,
    drop_w: f32,
    checking: Option<(usize, usize)>,
    checked: bool,
    filter: Filter,
    sort: Sort,
    as_list: bool,
    relayout: bool,
    filter_switch: Segmented,
    /// Categories, as tabs. `category` is the chosen one's order, or None for all.
    shelves: Vec<crate::store::Category>,
    category: Option<i64>,
    shelf_switch: Segmented,
    layout_switch: Segmented,
    sort_open: bool,
    sort_closed: Option<std::time::Instant>,
    kept: usize,
    /// Library indexes after the filter and sort.
    shown: Vec<usize>,
    /// Indexes into `series` for Continue reading.
    shelf: Vec<usize>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OpenManga> for Library {}
impl EventEmitter<Resume> for Library {}
impl EventEmitter<GoBrowse> for Library {}

impl Library {
    pub fn new(store: Entity<Store>, registry: Entity<Registry>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&registry, |this, registry, cx| {
                if registry.read(cx).ready && !this.checked {
                    this.checked = true;
                    this.check_for_updates(cx);
                }
                cx.notify();
            }),
        ];
        let as_list = std::env::var("NANO_LIBRARY").as_deref() == Ok("list");
        let filter_switch = Segmented::new(&["All", "New chapters", "Caught up"], 0);
        let layout_switch = Segmented::new(&["Grid", "List"], usize::from(as_list));
        Library {
            shelves: vec![],
            category: None,
            shelf_switch: Segmented::new(&["All"], 0),
            filter_switch,
            layout_switch,
            updates_page: false,
            page_seen: false,
            store,
            registry,
            cache,
            covers: Covers::default(),
            zoom_seen: 0.,
            list: ListState::new(0, ListAlignment::Top, u(800.)),
            items: vec![],
            cols: 1,
            card_w: CARD_W,
            width: 0.,
            series: vec![],
            library_len: 0,
            updates: vec![],
            drops: vec![],
            drop_w: 0.,
            checking: None,
            checked: false,
            filter: Filter::All,
            sort: Sort::LastRead,
            // NANO_LIBRARY=list starts in the list view (for testing).
            as_list,
            relayout: false,
            sort_open: false,
            sort_closed: None,
            kept: 0,
            shown: vec![],
            shelf: vec![],
            _subs: subs,
        }
    }

    // Checks once per launch. Might need a timer if Nano stays open for days.
    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        let targets: Vec<(Source, Value)> = {
            let registry = self.registry.read(cx);
            self.store.read(cx).library().iter().filter_map(|s| Some((registry.source(&s.source)?, s.manga.clone()))).collect()
        };
        if targets.is_empty() {
            return;
        }
        let total = targets.len();
        cx.spawn(async move |this, cx| {
            for (done, (source, manga)) in targets.into_iter().enumerate() {
                let _ = this.update(cx, |this, cx| {
                    this.checking = Some((done, total));
                    cx.notify();
                });
                let result = source.call(cx, "update", json!({ "manga": manga, "chapters": [], "needsDetails": false })).await;
                let Ok(v) = result else { continue };
                let chapters = v["chapters"].as_array().cloned().unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    let new = this.store.update(cx, |store, cx| {
                        if store.get(&source.id, manga["url"].as_str().unwrap_or_default()).is_none() {
                            return vec![];
                        }
                        let auto = store.prefs().auto_download;
                        store.update(&source.id, &source.name, &manga, cx, |s| {
                            let known: std::collections::HashSet<String> = s.chapters.iter().filter_map(|c| c["url"].as_str().map(str::to_string)).collect();
                            // Only when the list was known before, or a first check would download everything.
                            let fresh = if auto && s.in_library && !known.is_empty() { chapters.iter().filter(|c| c["url"].as_str().is_some_and(|u| !known.contains(u))).cloned().collect() } else { vec![] };
                            s.chapters = chapters;
                            // Within the scanlator filter, oldest first.
                            let visible = s.visible();
                            fresh.into_iter().filter(|c: &Value| visible.iter().any(|v| v["url"] == c["url"])).rev().collect::<Vec<Value>>()
                        })
                    });
                    if !new.is_empty() {
                        let title = manga["title"].as_str().unwrap_or_default().to_string();
                        let jobs = new.into_iter().map(|chapter| crate::downloads::Job { source: source.clone(), title: title.clone(), chapter }).collect();
                        crate::downloads::get(cx).update(cx, |d, cx| d.enqueue(jobs, cx));
                    }
                });
            }
            let _ = this.update(cx, |this, cx| {
                this.checking = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn resume(&mut self, ix: usize, chapter: Option<usize>, cx: &mut Context<Self>) {
        let s = &self.series[ix];
        let source = self.registry.read(cx).source_or_saved(&s.source, &s.source_name);
        let (index, page) = match (chapter, s.reading_index()) {
            (Some(c), _) => (c, 0),
            (None, Some(c)) => (c, s.progress.as_ref().map_or(0, |p| p.page)),
            (None, None) => (s.chapters.len().saturating_sub(1), 0),
        };
        // No extension means nothing to read, so open the series page where it can be migrated.
        if s.chapters.is_empty() || !source.installed {
            return cx.emit(OpenManga { source, manga: s.manga.clone() });
        }
        cx.emit(Resume { source, manga: s.manga.clone(), chapters: Arc::new(s.chapters.clone()), index, page });
    }

    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let s = &self.series[ix];
        let source = self.registry.read(cx).source_or_saved(&s.source, &s.source_name);
        cx.emit(OpenManga { source, manga: s.manga.clone() });
    }

    fn cover_of(&mut self, ix: usize, cx: &mut Context<Self>) -> Option<Arc<gpui::RenderImage>> {
        let s = &self.series[ix];
        let source = self.registry.read(cx).source(&s.source);
        let url = s.manga["thumbnailUrl"].as_str().map(str::to_string);
        self.covers.get(&self.cache, source.map(Fetch::Cover), url.as_deref(), |l: &mut Library| &mut l.covers, cx)
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.items.get(ix).copied() {
            Some(Item::Heading) => {
                let unread: usize = self.series[..self.library_len].iter().map(Series::unread).sum();
                let note = match self.checking {
                    Some((done, total)) => format!("{} SERIES · CHECKING {done}/{total}", self.library_len),
                    None => format!("{} SERIES · {unread} UNREAD", self.library_len),
                };
                if self.updates_page {
                    let fresh = self.updates.iter().filter(|(si, ci)| !self.series[*si].read_flags(&self.series[*si].chapters)[*ci]).count();
                    let note = match self.checking {
                        Some((done, total)) => format!("CHECKING {done}/{total}"),
                        None => format!("{fresh} NEW IN YOUR LIBRARY"),
                    };
                    return div().px(u(PAD)).pt(u(32.)).child(heading("Updates", note)).into_any_element();
                }
                div().px(u(PAD)).pt(u(32.)).child(heading("Library", note)).into_any_element()
            }
            Some(Item::Shelf) => self.shelf(cx),
            Some(Item::Controls) => self.controls(cx),
            Some(Item::NoMatch) => {
                let text = if self.updates_page { "No new chapters yet. Series you add to your library show up here when they update." } else { "Nothing here with this filter." };
                div().mx(u(PAD)).mt(u(24.)).p(u(40.)).rounded(u(12.)).border_1().border_color(rgb(LINE)).flex().justify_center().text_color(rgb(MUTED)).child(text).into_any_element()
            }
            Some(Item::Line(row)) => self.line(row, cx),
            Some(Item::Empty) => div()
                .px(u(PAD))
                .pt(u(24.))
                .flex()
                .flex_col()
                .items_start()
                .gap_4()
                .child(div().text_color(rgb(MUTED)).child("Nothing saved yet. Open a series in Browse and add it to your library."))
                .child(pill("go-browse", "Browse sources", true).on_click(cx.listener(|_, _, _, cx| cx.emit(GoBrowse))))
                .into_any_element(),
            Some(Item::Row(row)) => {
                let start = row * self.cols;
                let end = (start + self.cols).min(self.shown.len());
                let cards: Vec<AnyElement> = (start..end).map(|i| self.card(self.shown[i], cx)).collect();
                div().flex().gap(u(GAP)).px(u(PAD)).pt(u(24.)).children(cards).into_any_element()
            }
            Some(Item::Day(i)) => {
                let day = &self.drops[i].day;
                let now = now_ms();
                let label = if *day == crate::manga::ymd(now) {
                    "TODAY".to_string()
                } else if *day == crate::manga::ymd(now - 86_400_000) {
                    "YESTERDAY".to_string()
                } else {
                    day_label(self.drops[i].at)
                };
                div().px(u(PAD)).pt(u(28.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(label).into_any_element()
            }
            Some(Item::Drops(start, end)) => {
                let w = self.drop_w;
                let cards: Vec<AnyElement> = (start..end).map(|i| div().w(u(w)).flex_none().child(self.drop_card(i, cx)).into_any_element()).collect();
                div().w_full().px(u(PAD)).pb(u(12.)).flex().gap(u(12.)).children(cards).into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn section_title(text: &'static str) -> impl IntoElement {
        div().font_family(crate::fonts::mono()).text_size(u(13.)).font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(text)
    }

    fn shelf(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let avail = self.width / crate::zoom() - 2. * PAD;
        let n = ((avail + 14.) / 314.).floor().clamp(1., 3.) as usize;
        let w = (avail - 14. * (n - 1) as f32) / n as f32;
        let cards: Vec<AnyElement> = self.shelf.clone().into_iter().take(n).map(|ix| {
            let image = self.cover_of(ix, cx);
            let s = &self.series[ix];
            let p = s.progress.clone().unwrap_or_else(|| unreachable!());
            let page = if p.pages == 0 { 0. } else { ((p.page + 1) as f32 / p.pages as f32).min(1.) };
            div()
                .id(("shelf", ix))
                .w(u(w))
                .flex_none()
                .flex()
                .gap(u(14.))
                .p(u(12.))
                .rounded(u(12.))
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(SURFACE))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(crate::HOVER)))
                .child(div().flex_none().child(cover(image, s.title(), 74., 106., 0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(u(5.))
                        .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(s.title().to_string()))
                        .child(div().text_size(u(13.)).text_color(rgb(MUTED)).truncate().child(format!("{} · page {} of {}", p.chapter_name, p.page + 1, p.pages)))
                        .child(div().flex_1())
                        .child(div().h(u(3.)).rounded(u(2.)).bg(rgb(LINE)).child(div().h_full().rounded(u(2.)).w(relative(page)).bg(rgb(ACCENT))))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .gap_2()
                                .font_family(crate::fonts::mono())
                                .text_size(u(11.))
                                .text_color(rgb(FAINT))
                                .child(div().truncate().child(s.source_name.clone()))
                                .child(div().flex_none().child(ago(p.at, now_ms()))),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.resume(ix, None, cx)))
                .into_any_element()
        }).collect();
        div()
            .px(u(PAD))
            .pt(u(30.))
            .flex()
            .flex_col()
            .gap(u(14.))
            .child(Self::section_title("CONTINUE READING"))
            .child(div().flex().gap(u(14.)).children(cards))
            .into_any_element()
    }

    fn controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .px(u(PAD))
            .pt(u(36.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(u(10.))
            .child(if self.shelves.is_empty() {
                Self::section_title("EVERYTHING").into_any_element()
            } else {
                self.shelf_switch
                    .render("shelf", cx.listener(|this, i: &usize, _, cx| {
                        this.shelf_switch.pick(*i);
                        this.category = if *i == 0 { None } else { this.shelves.get(*i - 1).map(|c| c.order) };
                        this.relayout = true;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .child(div().ml_auto().flex().flex_wrap().gap(u(10.)).child(self.filter_switch.render("filter", cx.listener(|this, i: &usize, _, cx| {
                this.filter_switch.pick(*i);
                this.filter = [Filter::All, Filter::New, Filter::Done][*i];
                this.relayout = true;
                cx.notify();
            })))
            .child(self.sort_menu(cx))
            .child(self.layout_switch.render("layout", cx.listener(|this, i: &usize, _, cx| {
                this.layout_switch.pick(*i);
                this.as_list = *i == 1;
                this.relayout = true;
                cx.notify();
            }))))
            .into_any_element()
    }

    fn sort_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.sort_open;
        let button = div()
            .id("sort")
            .h(u(40.))
            .w(u(200.))
            .px(u(16.))
            .rounded(u(20.))
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .items_center()
            .gap(u(8.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(div().flex_1().min_w_0().truncate().text_size(u(13.)).text_color(rgb(crate::BODY)).child(self.sort.label()))
            .child(
                gpui::svg()
                    .path("icons/chevron.svg")
                    .flex_none()
                    .size(u(14.))
                    .text_color(rgb(MUTED))
                    .when(open, |d| d.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI)))),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                // The click that closes the menu also lands on the button, so don't reopen it.
                let just_closed = this.sort_closed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300));
                this.sort_open = !this.sort_open && !just_closed;
                cx.notify();
            }));
        let current = self.sort;
        let items = Sort::ALL.map(|sort| {
            let on = sort == current;
            div()
                .id(sort.label())
                .px(u(12.))
                .py(u(8.))
                .rounded(u(7.))
                .text_size(u(13.))
                .cursor_pointer()
                .text_color(rgb(if on { ACCENT_TEXT } else { crate::BODY }))
                .when(on, |d| d.bg(rgb(crate::HOVER)))
                .hover(|d| d.bg(rgb(crate::HOVER)).text_color(rgb(if on { ACCENT_TEXT } else { TEXT })))
                .child(sort.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sort = sort;
                    this.sort_open = false;
                    this.relayout = true;
                    cx.notify();
                }))
        });
        let panel = div()
            .id("sort-menu")
            .mt(u(6.))
            .w(u(200.))
            .p(u(5.))
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .occlude()
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.sort_open = false;
                this.sort_closed = Some(std::time::Instant::now());
                cx.notify();
            }));
        // Drawn after the cards so it sits on top of them.
        div()
            .flex_none()
            .relative()
            .child(button)
            .when(open, |d| {
                let panel = panel.absolute().left_0().with_animation("sort-in", gpui::Animation::new(std::time::Duration::from_millis(140)).with_easing(gpui::ease_out_quint()), |d, t| {
                    d.opacity(t).top(u(40. - 6. * (1. - t)))
                });
                d.child(gpui::deferred(panel).with_priority(1))
            })
            .into_any_element()
    }

    fn line(&mut self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let i = self.shown[row];
        let image = self.cover_of(i, cx);
        let s = &self.series[i];
        let chapter = s.progress.as_ref().map_or_else(|| format!("{} chapters", s.chapters.len()), |p| p.chapter_name.clone());
        let when = s.progress.as_ref().map_or(String::new(), |p| ago(p.at, now_ms()));
        let unread = s.unread();
        let row = div()
            .id(("line", i))
            .px(u(6.))
            .py(u(8.))
            .flex()
            .items_center()
            .gap(u(16.))
            .border_b_1()
            .border_color(rgb(RULE))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SURFACE)))
            .child(div().flex_none().child(cover(image, "", 40., 56., 0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(s.title().to_string()))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).truncate().child(chapter)),
            )
            .child(div().w(u(160.)).flex_none().truncate().text_size(u(13.)).text_color(rgb(MUTED)).child(s.source_name.clone()))
            .child(div().w(u(110.)).flex_none().text_size(u(13.)).text_color(rgb(MUTED)).child(when))
            .child(
                div()
                    .w(u(70.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .font_family(crate::fonts::mono())
                    .text_size(u(12.))
                    .text_color(rgb(ACCENT_TEXT))
                    .map(|d| if unread > 0 { d.child(format!("{unread} new")) } else { d.child(crate::check(16., ACCENT_TEXT)) }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(i, cx)));
        div().w_full().px(u(PAD)).child(row).into_any_element()
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let image = self.cover_of(i, cx);
        let s = &self.series[i];
        let title = s.title().to_string();
        let sub = match s.unread() {
            _ if s.caught_up() => format!("Caught up · {}", s.source_name),
            0 => s.source_name.clone(),
            n => format!("{n} new · {}", s.source_name),
        };
        div()
            .id(("lib", i))
            .w(u(self.card_w))
            .flex()
            .flex_col()
            .gap_2()
            .cursor_pointer()
            .child(card_cover(cover(image, &title, self.card_w, self.card_w * CARD_H / CARD_W, s.unread()), s.read_fraction(), s.caught_up()))
            .child(div().text_size(u(13.5)).font_weight(FontWeight::MEDIUM).truncate().child(title))
            .child(div().mt(u(-5.)).text_size(u(12.)).text_color(rgb(FAINT)).truncate().child(sub))
            .on_click(cx.listener(move |this, _, _, cx| this.open(i, cx)))
            .into_any_element()
    }

    fn drop_card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let si = self.drops[i].series;
        let chapters = self.drops[i].chapters.clone();
        let image = self.cover_of(si, cx);
        let s = &self.series[si];
        let read = s.read_flags(&s.chapters);
        let is_new = |ci: usize| !read[ci];
        let fresh: Vec<usize> = chapters.iter().copied().filter(|&ci| is_new(ci)).collect();
        // Start from the oldest new one, so nothing gets skipped.
        let open = fresh.iter().max().or(chapters.iter().max()).copied();
        let shown = 3;
        let lines: Vec<AnyElement> = chapters
            .iter()
            .take(shown)
            .map(|&ci| {
                let new = is_new(ci);
                div()
                    .flex()
                    .items_center()
                    .gap(u(8.))
                    .text_size(u(13.5))
                    .text_color(rgb(if new { crate::BODY } else { FAINT }))
                    .child(div().flex_none().size(u(5.)).rounded(u(3.)).bg(rgb(if new { ACCENT } else { LINE })))
                    .child(div().min_w_0().truncate().child(s.chapters[ci]["name"].as_str().unwrap_or("Chapter").to_string()))
                    .into_any_element()
            })
            .collect();
        let more = chapters.len().saturating_sub(shown);
        let badge = if fresh.is_empty() {
            div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("READ")
        } else {
            div()
                .h(u(22.))
                .px(u(9.))
                .rounded(u(11.))
                .flex()
                .items_center()
                .bg(rgb(ACCENT))
                .font_family(crate::fonts::mono())
                .text_size(u(11.))
                .text_color(rgb(crate::BG))
                .child(format!("{} NEW", fresh.len()))
        };
        let card = div()
            .id(("drop", i))
            .flex()
            .gap(u(16.))
            .p(u(12.))
            .rounded(u(12.))
            .border_1()
            .border_color(rgb(RULE))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SURFACE)))
            .when(fresh.is_empty(), |d| d.opacity(0.6))
            .child(div().flex_none().child(cover(image, "", 52., 74., 0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(5.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(u(10.))
                            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(s.title().to_string()))
                            .child(div().flex_none().text_size(u(12.)).text_color(rgb(FAINT)).child(s.source_name.clone())),
                    )
                    .children(lines)
                    .when(more > 0, |d| d.child(div().pl(u(13.)).text_size(u(12.5)).text_color(rgb(FAINT)).child(format!("+{more} more")))),
            )
            .child(div().flex_none().child(badge))
            .when_some(open, |d, ci| d.on_click(cx.listener(move |this, _, _, cx| this.resume(si, Some(ci), cx))));
        card.h_full().into_any_element()
    }
}

/// "SUN 16 NOV" for a date in milliseconds.
fn day_label(ms: i64) -> String {
    const DAYS: [&str; 7] = ["THU", "FRI", "SAT", "SUN", "MON", "TUE", "WED"];
    const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    let ymd = crate::manga::ymd(ms);
    let (month, day) = (ymd[5..7].parse::<usize>().unwrap_or(1), ymd[8..10].trim_start_matches('0').to_string());
    format!("{} {day} {}", DAYS[ms.div_euclid(86_400_000).rem_euclid(7) as usize], MONTHS[month - 1])
}

/// A cover with a READING or CAUGHT UP label and a thin progress line along the bottom.
fn card_cover(cover: AnyElement, read: Option<f32>, caught_up: bool) -> impl IntoElement {
    div().relative().child(cover).when_some(read, |d, f| {
        let label = if caught_up { "CAUGHT UP" } else { "READING" };
        d.when(caught_up, |d| d.child(div().absolute().inset_0().rounded(u(4.)).bg(gpui::rgba(0x0D0C0B99))))
            .child(
                div()
                    .absolute()
                    .top(u(8.))
                    .left(u(8.))
                    .h(u(24.))
                    .px(u(8.))
                    .rounded(u(12.))
                    .flex()
                    .items_center()
                    .bg(rgb(if caught_up { 0x0D0C0B } else { ACCENT }))
                    .font_family(crate::fonts::mono())
                    .text_size(u(10.5))
                    .text_color(rgb(if caught_up { TEXT } else { 0x0D0C0B }))
                    .child(label),
            )
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(u(3.))
                    .rounded_b(u(4.))
                    .bg(gpui::rgba(0x0D0C0B80))
                    .child(div().h_full().w(relative(f)).bg(rgb(ACCENT))),
            )
    })
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        let width = crate::content_width(window);
        self.width = width;
        (self.cols, self.card_w) = grid(width, PAD, GAP, CARD_W);
        let store = self.store.read(cx);
        self.series = store.library().into_iter().cloned().map(|mut s| {
            // Chapters after the scanlator filter. Not saved.
            s.chapters = s.visible();
            s.hidden_scanlators.clear();
            s
        }).collect();
        self.library_len = self.series.len();
        let reading: Vec<Series> = store.history().into_iter().take(3).cloned().map(|mut s| {
            s.chapters = s.visible();
            s.hidden_scanlators.clear();
            s
        }).collect();

        self.shelves = store.categories();
        if self.category.is_some_and(|o| !self.shelves.iter().any(|c| c.order == o)) {
            self.category = None;
        }
        let labels: Vec<String> = std::iter::once("All".to_string()).chain(self.shelves.iter().map(|c| c.name.clone())).collect();
        let at = self.category.and_then(|o| self.shelves.iter().position(|c| c.order == o)).map_or(0, |p| p + 1);
        self.shelf_switch.set(&labels.iter().map(String::as_str).collect::<Vec<_>>(), at);
        let mut shown: Vec<usize> = (0..self.library_len)
            .filter(|&i| self.category.is_none_or(|o| self.series[i].categories.contains(&o)))
            .filter(|&i| match self.filter {
                Filter::All => true,
                Filter::New => self.series[i].unread() > 0,
                Filter::Done => self.series[i].caught_up(),
            })
            .collect();
        match self.sort {
            Sort::LastRead => {}
            Sort::MostNew => shown.sort_by_key(|&i| std::cmp::Reverse(self.series[i].unread())),
            Sort::Title => shown.sort_by_cached_key(|&i| self.series[i].title().to_lowercase()),
        }
        self.shown = shown;

        let mut updates: Vec<(usize, usize, i64)> = self
            .series
            .iter()
            .enumerate()
            .flat_map(|(si, s)| {
                s.chapters.iter().take(UPDATES).enumerate().filter_map(move |(ci, c)| Some((si, ci, c["dateUpload"].as_i64().filter(|d| *d > 0)?)))
            })
            .collect();
        updates.sort_by_key(|u| -u.2);
        let updates: Vec<(usize, usize, i64)> = updates.into_iter().take(UPDATES).collect();
        self.updates = updates.iter().map(|&(s, c, _)| (s, c)).collect();
        // UTC days. Switch to local time if sources start giving hours.
        let mut drops: Vec<Drop> = vec![];
        for (si, ci, at) in updates {
            let day = crate::manga::ymd(at);
            match drops.iter_mut().find(|d| d.day == day && d.series == si) {
                Some(d) => d.chapters.push(ci),
                None => drops.push(Drop { at, day, series: si, chapters: vec![ci] }),
            }
        }
        // Chapter lists run newest first, so this puts each day's newest chapter on top.
        drops.iter_mut().for_each(|d| d.chapters.sort());
        self.drops = drops;
        // Something read from Browse without saving still belongs on the shelf.
        self.shelf = reading
            .into_iter()
            .map(|r| match self.series.iter().position(|x| x.source == r.source && x.url() == r.url()) {
                Some(i) => i,
                None => {
                    self.series.push(r);
                    self.series.len() - 1
                }
            })
            .collect();

        let mut items = vec![Item::Heading];
        if self.updates_page {
            if self.drops.is_empty() {
                items.push(Item::NoMatch);
            }
            let avail = self.width / crate::zoom() - 2. * PAD;
            let cols = ((avail + 12.) / 472.).floor().max(1.) as usize;
            self.drop_w = (avail - 12. * (cols - 1) as f32) / cols as f32;
            let mut start = 0;
            while start < self.drops.len() {
                let day = &self.drops[start].day;
                let end = start + self.drops[start..].iter().take_while(|d| &d.day == day).count();
                items.push(Item::Day(start));
                let mut row = start;
                while row < end {
                    items.push(Item::Drops(row, (row + cols).min(end)));
                    row += cols;
                }
                start = end;
            }
        } else {
            if !self.shelf.is_empty() {
                items.push(Item::Shelf);
            }
            if self.library_len == 0 {
                items.push(Item::Empty);
            } else {
                items.push(Item::Controls);
                if self.shown.is_empty() {
                    items.push(Item::NoMatch);
                } else if self.as_list {
                    items.extend((0..self.shown.len()).map(Item::Line));
                } else {
                    items.extend((0..self.shown.len().div_ceil(self.cols)).map(Item::Row));
                }
            }
        }
        if self.page_seen != self.updates_page {
            self.page_seen = self.updates_page;
            self.list.reset(items.len());
        }
        // Swap only what follows the controls, so changing them keeps your scroll position.
        let old = self.list.item_count();
        if self.relayout || items.len() != old {
            self.relayout = false;
            let keep = items.iter().position(|i| matches!(i, Item::Controls)).map_or(0, |c| c + 1);
            if keep == self.kept && keep <= old {
                self.list.splice(keep..old, items.len() - keep);
            } else {
                self.list.reset(items.len());
            }
            self.kept = keep;
        }
        self.items = items;

        let this = cx.entity();
        div().size_full().pb(u(24.)).child(
            list(self.list.clone(), move |ix, _window, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn day_labels_name_the_weekday() {
        assert_eq!(super::day_label(0), "THU 1 JAN");
        assert_eq!(super::day_label(1_704_067_200_000), "MON 1 JAN"); // 2024-01-01
        assert_eq!(super::day_label(1_731_715_200_000), "SAT 16 NOV"); // 2024-11-16
    }
}
