//! Library, Updates and History, laid out like manga's.

use super::plugins::Plugins;
use super::store::LnStore;
use super::{OpenNovel, ReadChapter, cover_fetch};
use crate::catalog::small_button;
use crate::covers::{CARD_H, CARD_W, Covers, cover, grid};
use crate::images::ImageCache;
use crate::library::{card_cover, day_label};
use crate::segmented::Segmented;
use crate::store::{ago, now_ms};
use crate::{ACCENT, ACCENT_TEXT, BODY, FAINT, HOVER, LINE, MUTED, RULE, SURFACE, TEXT, heading, pill, u};
use gpui::{
    AnimationExt, AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Render, RenderImage,
    StatefulInteractiveElement, Styled, Subscription, Window, div, list, prelude::FluentBuilder, relative, rgb, uniform_list,
};
use std::ops::Range;
use std::sync::Arc;

const PAD: f32 = 40.;
const GAP: f32 = 20.;
const HISTORY_GAP: f32 = 24.;

pub struct GoBrowse;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Library,
    Updates,
    History,
}

#[derive(Clone, Copy)]
enum Item {
    Heading,
    Shelf,
    Controls,
    Empty,
    NoMatch,
    Row(usize),
    Line(usize),
    /// A day heading on the Updates page, then that day's novels.
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

/// What a card needs, without carrying the chapter list around.
#[derive(Clone)]
struct Row {
    source: String,
    path: String,
    name: String,
    cover: String,
    source_name: String,
    unread: usize,
    total: usize,
    read: Option<f32>,
    caught_up: bool,
    /// The chapter you stopped at: its path, its name, how far down you were, and when.
    last: Option<(String, String, f32, i64)>,
}

/// One novel's new chapters that came out on the same day.
struct Drop {
    at: i64,
    day: String,
    source: String,
    path: String,
    name: String,
    source_name: String,
    cover: String,
    /// Name, path, and whether it's still unread.
    chapters: Vec<(String, String, bool)>,
}

pub struct Library {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    cache: Entity<ImageCache>,
    covers: Covers,
    page: Page,
    page_seen: Option<Page>,
    list: ListState,
    /// The list caches row heights, so re-measure after a zoom change.
    zoom_seen: f32,
    items: Vec<Item>,
    cols: usize,
    card_w: f32,
    width: f32,
    rows: Vec<Row>,
    /// Indexes into `rows` after the filter and sort.
    shown: Vec<usize>,
    shelf: Vec<Row>,
    history: Vec<Row>,
    drops: Vec<Drop>,
    drop_w: f32,
    unread_total: usize,
    fresh_total: usize,
    /// How many novels the update check has done, and how many it will do.
    checking: Option<(usize, usize)>,
    checked: bool,
    filter: Filter,
    sort: Sort,
    as_list: bool,
    relayout: bool,
    filter_switch: Segmented,
    layout_switch: Segmented,
    sort_open: bool,
    sort_closed: Option<std::time::Instant>,
    kept: usize,
    /// Bumped whenever the store changes, so the rows are only rebuilt when something did.
    data_gen: u64,
    built: Option<(u64, Page, Filter, Sort)>,
    _sub: Subscription,
}

impl EventEmitter<OpenNovel> for Library {}
impl EventEmitter<ReadChapter> for Library {}
impl EventEmitter<GoBrowse> for Library {}

impl Library {
    pub fn new(store: Entity<LnStore>, plugins: Entity<Plugins>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |this, _, cx| {
            this.data_gen += 1;
            cx.notify();
        });
        Library {
            store,
            plugins,
            cache,
            covers: Covers::default(),
            page: Page::Library,
            page_seen: None,
            list: ListState::new(0, ListAlignment::Top, u(800.)),
            zoom_seen: 0.,
            items: vec![],
            cols: 1,
            card_w: CARD_W,
            width: 0.,
            rows: vec![],
            shown: vec![],
            shelf: vec![],
            history: vec![],
            drops: vec![],
            drop_w: 0.,
            unread_total: 0,
            fresh_total: 0,
            checking: None,
            checked: false,
            filter: Filter::All,
            sort: Sort::LastRead,
            as_list: false,
            relayout: false,
            filter_switch: Segmented::new(&["All", "New chapters", "Caught up"], 0),
            layout_switch: Segmented::new(&["Grid", "List"], 0),
            sort_open: false,
            sort_closed: None,
            kept: 0,
            data_gen: 0,
            built: None,
            _sub: sub,
        }
    }

    pub fn show(&mut self, updates: bool, history: bool, cx: &mut Context<Self>) {
        self.page = if updates {
            Page::Updates
        } else if history {
            Page::History
        } else {
            Page::Library
        };
        cx.notify();
    }

    /// Asks every library novel's source for new chapters.
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

    fn image(&mut self, source: &str, url: &str, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let how = cover_fetch(self.plugins.read(cx), source);
        self.covers.get(&self.cache, how, Some(url).filter(|u| !u.is_empty()), |l: &mut Library| &mut l.covers, cx)
    }

    fn open(&mut self, source: String, path: String, cx: &mut Context<Self>) {
        cx.emit(OpenNovel { source, path });
    }

    /// Opens the reader where you stopped, or the novel's page if you haven't started.
    fn resume(&mut self, r: &Row, cx: &mut Context<Self>) {
        match &r.last {
            Some((chapter, ..)) => cx.emit(ReadChapter { source: r.source.clone(), path: r.path.clone(), chapter: chapter.clone() }),
            None => cx.emit(OpenNovel { source: r.source.clone(), path: r.path.clone() }),
        }
    }

    fn section_title(text: &'static str) -> impl IntoElement {
        div().font_family(crate::fonts::mono()).text_size(u(13.)).font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(text)
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.items.get(ix).copied() {
            Some(Item::Heading) => {
                if self.page == Page::Updates {
                    let note = match self.checking {
                        Some((done, total)) => format!("CHECKING {done}/{total}"),
                        None => format!("{} NEW IN YOUR LIBRARY", self.fresh_total),
                    };
                    let busy = self.checking.is_some();
                    let any = !self.drops.is_empty();
                    return div()
                        .px(u(PAD))
                        .pt(u(32.))
                        .flex()
                        .flex_wrap()
                        .items_end()
                        .gap_4()
                        .child(div().flex_1().min_w_0().child(heading("Updates", note)))
                        .child(small_button("ln-check", if busy { "Checking…" } else { "Check now" }, true).on_click(cx.listener(|this, _, _, cx| this.check(cx))))
                        .when(any, |d| d.child(small_button("ln-clear", "Clear", false).on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.clear_updates(cx))))))
                        .into_any_element();
                }
                let note = match self.checking {
                    Some((done, total)) => format!("{} NOVELS · CHECKING {done}/{total}", self.rows.len()),
                    None => format!("{} NOVELS · {} UNREAD", self.rows.len(), self.unread_total),
                };
                div().px(u(PAD)).pt(u(32.)).child(heading("Library", note)).into_any_element()
            }
            Some(Item::Shelf) => self.shelf(cx),
            Some(Item::Controls) => self.controls(cx),
            Some(Item::NoMatch) => {
                let text = if self.page == Page::Updates { "No new chapters yet. Novels you add to your library show up here when they update." } else { "Nothing here with this filter." };
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
                .child(div().text_color(rgb(MUTED)).child("Nothing saved yet. Open a novel in Browse and add it to your library."))
                .child(pill("ln-go-browse", "Browse sources", true).on_click(cx.listener(|_, _, _, cx| cx.emit(GoBrowse))))
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

    fn shelf(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let avail = self.width / crate::zoom() - 2. * PAD;
        let n = ((avail + 14.) / 314.).floor().clamp(1., 3.) as usize;
        let w = (avail - 14. * (n - 1) as f32) / n as f32;
        let cards: Vec<AnyElement> = self
            .shelf
            .clone()
            .into_iter()
            .take(n)
            .enumerate()
            .map(|(ix, r)| {
                let image = self.image(&r.source, &r.cover, cx);
                let Some((_, chapter, scroll, at)) = r.last.clone() else { return div().into_any_element() };
                let (open, go) = (r.clone(), r.clone());
                div()
                    .id(("ln-shelf", ix))
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
                    .hover(|d| d.bg(rgb(HOVER)))
                    .child(
                        div().id(("ln-shelf-cover", ix)).flex_none().child(cover(image, &r.name, 74., 106., 0)).on_click(cx.listener(move |this, _, _, cx| {
                            this.open(open.source.clone(), open.path.clone(), cx);
                            cx.stop_propagation();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(5.))
                            .child(
                                div()
                                    .id(("ln-shelf-title", ix))
                                    .self_start()
                                    .max_w_full()
                                    .text_size(u(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(r.name.clone())
                                    .on_click({
                                        let r = r.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.open(r.source.clone(), r.path.clone(), cx);
                                            cx.stop_propagation();
                                        })
                                    }),
                            )
                            .child(div().text_size(u(13.)).text_color(rgb(MUTED)).truncate().child(chapter))
                            .child(div().flex_1())
                            .child(div().h(u(3.)).rounded(u(2.)).bg(rgb(LINE)).child(div().h_full().rounded(u(2.)).w(relative(scroll.clamp(0., 1.))).bg(rgb(ACCENT))))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .font_family(crate::fonts::mono())
                                    .text_size(u(11.))
                                    .text_color(rgb(FAINT))
                                    .child(div().truncate().child(r.source_name.clone()))
                                    .child(div().flex_none().child(ago(at, now_ms()))),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.resume(&go, cx)))
                    .into_any_element()
            })
            .collect();
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
            .child(Self::section_title("EVERYTHING"))
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .flex_wrap()
                    .gap(u(10.))
                    .child(self.filter_switch.render(
                        "ln-filter",
                        cx.listener(|this, i: &usize, _, cx| {
                            this.filter_switch.pick(*i);
                            this.filter = [Filter::All, Filter::New, Filter::Done][*i];
                            this.relayout = true;
                            cx.notify();
                        }),
                    ))
                    .child(self.sort_menu(cx))
                    .child(self.layout_switch.render(
                        "ln-layout",
                        cx.listener(|this, i: &usize, _, cx| {
                            this.layout_switch.pick(*i);
                            this.as_list = *i == 1;
                            this.relayout = true;
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element()
    }

    fn sort_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.sort_open;
        let button = div()
            .id("ln-sort")
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
            .child(div().flex_1().min_w_0().truncate().text_size(u(13.)).text_color(rgb(BODY)).child(self.sort.label()))
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
                .text_color(rgb(if on { ACCENT_TEXT } else { BODY }))
                .when(on, |d| d.bg(rgb(HOVER)))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(if on { ACCENT_TEXT } else { TEXT })))
                .child(sort.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sort = sort;
                    this.sort_open = false;
                    this.relayout = true;
                    cx.notify();
                }))
        });
        let panel = div()
            .id("ln-sort-menu")
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
        div()
            .flex_none()
            .relative()
            .child(button)
            .when(open, |d| {
                let panel = panel.absolute().left_0().with_animation("ln-sort-in", gpui::Animation::new(std::time::Duration::from_millis(140)).with_easing(gpui::ease_out_quint()), |d, t| {
                    d.opacity(t).top(u(40. - 6. * (1. - t)))
                });
                d.child(gpui::deferred(panel).with_priority(1))
            })
            .into_any_element()
    }

    fn line(&mut self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let i = self.shown[row];
        let r = self.rows[i].clone();
        let image = self.image(&r.source, &r.cover, cx);
        let chapter = r.last.as_ref().map_or_else(|| format!("{} chapters", r.total), |l| l.1.clone());
        let when = r.last.as_ref().map_or(String::new(), |l| ago(l.3, now_ms()));
        let unread = r.unread;
        let line = div()
            .id(("ln-line", i))
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
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(r.name.clone()))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).truncate().child(chapter)),
            )
            .child(div().w(u(160.)).flex_none().truncate().text_size(u(13.)).text_color(rgb(MUTED)).child(r.source_name.clone()))
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
            .on_click(cx.listener(move |this, _, _, cx| this.open(r.source.clone(), r.path.clone(), cx)));
        div().w_full().px(u(PAD)).child(line).into_any_element()
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let r = self.rows[i].clone();
        let image = self.image(&r.source, &r.cover, cx);
        let sub = match r.unread {
            _ if r.caught_up => format!("Caught up · {}", r.source_name),
            0 => r.source_name.clone(),
            n => format!("{n} new · {}", r.source_name),
        };
        div()
            .id(("ln-lib", i))
            .w(u(self.card_w))
            .flex()
            .flex_col()
            .gap_2()
            .cursor_pointer()
            .child(card_cover(cover(image, &r.name, self.card_w, self.card_w * CARD_H / CARD_W, r.unread), r.read, r.caught_up))
            .child(div().text_size(u(13.5)).font_weight(FontWeight::MEDIUM).truncate().child(r.name.clone()))
            .child(div().mt(u(-5.)).text_size(u(12.)).text_color(rgb(FAINT)).truncate().child(sub))
            .on_click(cx.listener(move |this, _, _, cx| this.open(r.source.clone(), r.path.clone(), cx)))
            .into_any_element()
    }

    fn drop_card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let (source, path, name, source_name, cover_url) = {
            let d = &self.drops[i];
            (d.source.clone(), d.path.clone(), d.name.clone(), d.source_name.clone(), d.cover.clone())
        };
        let chapters = self.drops[i].chapters.clone();
        let image = self.image(&source, &cover_url, cx);
        let fresh: Vec<&(String, String, bool)> = chapters.iter().filter(|c| c.2).collect();
        // Start from the oldest new one, so nothing gets skipped. The list runs newest first.
        let open = fresh.last().copied().or(chapters.last()).map(|c| c.1.clone());
        let shown = 3;
        let lines: Vec<AnyElement> = chapters
            .iter()
            .take(shown)
            .map(|(name, _, new)| {
                div()
                    .flex()
                    .items_center()
                    .gap(u(8.))
                    .text_size(u(13.5))
                    .text_color(rgb(if *new { BODY } else { FAINT }))
                    .child(div().flex_none().size(u(5.)).rounded(u(3.)).bg(rgb(if *new { ACCENT } else { LINE })))
                    .child(div().min_w_0().truncate().child(name.clone()))
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
        div()
            .id(("ln-drop", i))
            .h_full()
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
                            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(div().flex_none().text_size(u(12.)).text_color(rgb(FAINT)).child(source_name)),
                    )
                    .children(lines)
                    .when(more > 0, |d| d.child(div().pl(u(13.)).text_size(u(12.5)).text_color(rgb(FAINT)).child(format!("+{more} more")))),
            )
            .child(div().flex_none().child(badge))
            .on_click(cx.listener(move |_, _, _, cx| match &open {
                Some(chapter) => cx.emit(ReadChapter { source: source.clone(), path: path.clone(), chapter: chapter.clone() }),
                None => cx.emit(OpenNovel { source: source.clone(), path: path.clone() }),
            }))
            .into_any_element()
    }

    fn row_of(n: &super::store::Novel) -> Row {
        let started = n.progress.is_some() || !n.read.is_empty();
        let total = n.chapters.len();
        let unread = n.unread();
        Row {
            source: n.source.clone(),
            path: n.path.clone(),
            name: n.name.clone(),
            cover: n.cover.clone(),
            source_name: n.source_name.clone(),
            unread,
            total,
            read: (started && total > 0).then(|| (total - unread) as f32 / total as f32),
            caught_up: total > 0 && unread == 0,
            last: n.progress.as_ref().map(|p| (p.chapter.clone(), p.name.clone(), p.scroll, p.at)),
        }
    }

    fn history_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.history.len();
        let cols = self.cols;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(u(PAD)).pt(u(28.)).pb(u(12.)).child(heading("History", format!("{count} novels"))))
            .children((count == 0).then(|| div().px(u(PAD)).pt(u(20.)).text_color(rgb(MUTED)).child("Nothing read yet.")))
            .child(
                uniform_list(
                    "ln-history",
                    count.div_ceil(cols),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        let now = now_ms();
                        range
                            .map(|row| {
                                let cards: Vec<AnyElement> = (row * cols..((row + 1) * cols).min(this.history.len())).map(|i| this.history_card(i, now, cx)).collect();
                                div().flex().gap(u(HISTORY_GAP)).px(u(PAD)).py(u(HISTORY_GAP / 2.)).children(cards)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
            .into_any_element()
    }

    fn history_card(&mut self, i: usize, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let r = self.history[i].clone();
        let image = self.image(&r.source, &r.cover, cx);
        let Some((_, chapter, scroll, at)) = r.last.clone() else { return div().into_any_element() };
        let w = self.card_w;
        let (title, go) = (r.clone(), r.clone());
        div()
            .id(("ln-history", i))
            .w(u(w))
            .flex()
            .flex_col()
            .gap(u(4.))
            .cursor_pointer()
            .child(div().mb(u(4.)).child(cover(image, &r.name, w, w * CARD_H / CARD_W, 0)))
            .child(
                div()
                    .id(("ln-history-title", i))
                    .self_start()
                    .max_w_full()
                    .text_size(u(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(r.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open(title.source.clone(), title.path.clone(), cx);
                        cx.stop_propagation();
                    })),
            )
            .child(div().text_size(u(13.)).text_color(rgb(0xC9C1B3)).truncate().child(chapter))
            .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).truncate().child(format!("{}% · {}", (scroll * 100.).round() as u32, ago(at, now))))
            .on_click(cx.listener(move |this, _, _, cx| this.resume(&go, cx)))
            .into_any_element()
    }
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.checked && !self.store.read(cx).library().is_empty() {
            self.checked = true;
            cx.defer_in(window, |this, _, cx| this.check(cx));
        }
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        let width = crate::content_width(window);
        self.width = width;
        (self.cols, self.card_w) = grid(width, PAD, if self.page == Page::History { HISTORY_GAP } else { GAP }, CARD_W);
        let key = (self.data_gen, self.page, self.filter, self.sort);
        if self.built != Some(key) {
            let store = self.store.read(cx);
            self.rows = store.library().into_iter().map(Self::row_of).collect();
            self.unread_total = self.rows.iter().map(|r| r.unread).sum();
            self.history = store.history().into_iter().map(Self::row_of).collect();
            self.shelf = self.history.iter().take(3).cloned().collect();
            let mut shown: Vec<usize> = (0..self.rows.len())
                .filter(|&i| match self.filter {
                    Filter::All => true,
                    Filter::New => self.rows[i].unread > 0,
                    Filter::Done => self.rows[i].caught_up,
                })
                .collect();
            match self.sort {
                Sort::LastRead => {}
                Sort::MostNew => shown.sort_by_key(|&i| std::cmp::Reverse(self.rows[i].unread)),
                Sort::Title => shown.sort_by_cached_key(|&i| self.rows[i].name.to_lowercase()),
            }
            self.shown = shown;
            // UTC days, like manga's.
            let mut drops: Vec<Drop> = vec![];
            for up in store.updates() {
                let day = crate::manga::ymd(up.at);
                let stored = store.get(&up.source, &up.path);
                let new = stored.is_none_or(|n| !n.read.contains(&up.chapter_path));
                let line = (up.chapter.clone(), up.chapter_path.clone(), new);
                match drops.iter_mut().find(|d| d.day == day && d.source == up.source && d.path == up.path) {
                    Some(d) => d.chapters.push(line),
                    None => drops.push(Drop {
                        at: up.at,
                        day,
                        source: up.source.clone(),
                        path: up.path.clone(),
                        name: up.novel.clone(),
                        source_name: stored.map(|n| n.source_name.clone()).unwrap_or_default(),
                        cover: stored.map(|n| n.cover.clone()).unwrap_or_default(),
                        chapters: vec![line],
                    }),
                }
            }
            self.fresh_total = drops.iter().map(|d| d.chapters.iter().filter(|c| c.2).count()).sum();
            self.drops = drops;
            self.built = Some(key);
        }
        if self.page == Page::History {
            self.page_seen = Some(Page::History);
            return self.history_page(cx).into_any_element();
        }

        let mut items = vec![Item::Heading];
        if self.page == Page::Updates {
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
            if self.rows.is_empty() {
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
        if self.page_seen != Some(self.page) {
            self.page_seen = Some(self.page);
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
        div().size_full().pb(u(24.)).child(list(self.list.clone(), move |ix, _window, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full()).into_any_element()
    }
}
