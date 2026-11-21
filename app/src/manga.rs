//! One series: details, then its chapters, in one scrolling list (some series have thousands).
//! Chapters can be searched and filtered by scanlator, which on multi-language sources means by language too.

use crate::u;
use crate::covers::{Covers, Fetch};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::store::{Store, default_hidden, now_ms, scanlator_counts, visible_chapters};
use crate::{ACCENT_TEXT, BODY, FAINT, LINE, MUTED, RULE, SURFACE, Source, TEXT, heading, pill};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ListAlignment,
    ListState, ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Window, div, img, list, prelude::FluentBuilder, relative, rgb,
};
use serde_json::{Value, json};
use std::sync::Arc;

const PAD: f32 = 24.;
/// The series panel's width beside the grid.
pub const PANEL: f32 = 440.;

pub enum MangaEvent {
    Back,
    Read { chapters: Arc<Vec<Value>>, index: usize, page: usize },
    Migrate,
}

#[derive(Clone, Copy)]
enum Item {
    Hero,
    Bar,
    Header,
    Row(usize),
    Empty,
}

pub struct MangaView {
    pub source: Source,
    cache: Entity<ImageCache>,
    store: Entity<Store>,
    covers: Covers,
    pub manga: Value,
    chapters: Arc<Vec<Value>>,
    /// Hidden scanlators. Until the user picks, only the main group shows.
    hidden: Vec<String>,
    chosen: bool,
    /// `menu_closed` stops the click that closed the menu from reopening it.
    menu: bool,
    menu_closed: Option<std::time::Instant>,
    /// Chapters after the scanlator filter, and the rows left after the search.
    visible: Arc<Vec<Value>>,
    rows: Vec<usize>,
    search: Entity<TextInput>,
    list: ListState,
    /// The list caches row heights, so re-measure after a zoom change.
    zoom_seen: f32,
    items: Vec<Item>,
    status: SharedString,
    /// Read state of each chapter in `visible`, and the bookmarked urls, from the saved record.
    read: Vec<bool>,
    bookmarks: std::collections::BTreeSet<String>,
    /// Folder names of the chapters on disk.
    downloaded: std::collections::HashSet<String>,
    /// The categories menu, and when it last closed so the click that closed it doesn't reopen it.
    cat_menu: bool,
    cat_closed: Option<std::time::Instant>,
    /// The chapter whose actions menu is open, and where.
    row_menu: Option<(usize, gpui::Point<gpui::Pixels>)>,
    /// The trackers drawer, over the panel while open.
    tracking: Option<Entity<crate::tracking::Tracking>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<MangaEvent> for MangaView {}

impl MangaView {
    pub fn new(source: Source, cache: Entity<ImageCache>, store: Entity<Store>, manga: Value, cx: &mut Context<Self>) -> Self {
        // Saved series show their stored chapters straight away until the update below lands.
        let saved = store.read(cx).get(&source.id, manga["url"].as_str().unwrap_or_default()).cloned();
        let search = cx.new(|cx| TextInput::new("Search chapters…", cx));
        let downloads = crate::downloads::get(cx);
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&downloads, |this, _, cx| this.refresh_downloaded(cx)),
        ];
        let call_source = source.clone();
        let request = json!({ "manga": manga, "chapters": [] });
        let installed = source.installed;
        cx.spawn(async move |this, cx| {
            if !installed {
                return; // extension's gone, so show what was saved
            }
            let result = call_source.call(cx, "update", request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(v) => {
                        this.manga = v["manga"].clone();
                        this.chapters = Arc::new(v["chapters"].as_array().cloned().unwrap_or_default());
                        if !this.chosen {
                            let reading = this.store.read(cx).get(&this.source.id, this.url()).and_then(|s| s.progress.as_ref().map(|p| p.chapter_url.clone()));
                            this.hidden = default_hidden(&this.chapters, reading.as_deref());
                        }
                        this.status = "".into();
                        this.save(cx, |_| {});
                        if crate::demo().is_some() && !this.chapters.is_empty() && !crate::demo_mode().starts_with("series") {
                            cx.emit(MangaEvent::Read { chapters: this.chapters.clone(), index: this.chapters.len() - 1, page: 0 });
                        }
                    }
                    Err(e) => this.status = e.into(),
                }
                cx.notify();
            });
        })
        .detach();
        let status = if !source.installed {
            format!("{} isn't installed. Showing what was saved.", source.name)
        } else if saved.is_some() {
            String::new()
        } else {
            "loading chapters…".into()
        };
        let downloaded = crate::downloads::downloaded(&source, manga["title"].as_str().unwrap_or_default());
        MangaView {
            source,
            cache,
            store,
            covers: Covers::default(),
            manga,
            chapters: Arc::new(saved.as_ref().map(|s| s.chapters.clone()).unwrap_or_default()),
            hidden: saved.as_ref().map(|s| s.hidden()).unwrap_or_default(),
            chosen: saved.as_ref().is_some_and(|s| s.scanlators_chosen),
            // NANO_DEMO_MODE=series-menu opens with the scanlator menu shown (for testing).
            menu: crate::demo_mode() == "series-menu",
            menu_closed: None,
            visible: Arc::default(),
            rows: vec![],
            search,
            zoom_seen: 0.,
            list: ListState::new(0, ListAlignment::Top, u(800.)),
            items: vec![],
            read: vec![],
            bookmarks: Default::default(),
            row_menu: None,
            cat_menu: false,
            cat_closed: None,
            downloaded,
            tracking: None,
            status: status.into(),
            _subs: subs,
        }
    }

    /// Saves details, chapters and the filter to the series' record. `force` creates the record.
    fn save(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut crate::store::Series)) {
        let (source, manga, chapters, hidden, chosen) =
            (self.source.clone(), self.manga.clone(), self.chapters.clone(), self.hidden.clone(), self.chosen);
        self.store.update(cx, |store, cx| {
            if store.get(&source.id, manga["url"].as_str().unwrap_or_default()).is_some() {
                store.update(&source.id, &source.name, &manga, cx, |s| {
                    s.manga = manga.clone();
                    s.chapters = chapters.to_vec();
                    s.hidden_scanlators = hidden;
                    s.scanlators_chosen = chosen;
                    f(s);
                });
            }
        });
    }

    fn toggle_library(&mut self, cx: &mut Context<Self>) {
        let (source, manga) = (self.source.clone(), self.manga.clone());
        let was = self.store.read(cx).get(&self.source.id, self.url()).is_some_and(|s| s.in_library);
        self.store.update(cx, |store, cx| store.update(&source.id, &source.name, &manga, cx, |s| s.in_library = true));
        self.save(cx, |s| {
            s.in_library = !was;
            s.added_at = now_ms();
        });
    }

    /// `None` toggles all of them: shows every one, or hides them all if they're already shown.
    fn toggle_scanlator(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        match name {
            None if self.hidden.is_empty() => self.hidden = scanlator_counts(&self.chapters).into_iter().map(|(g, _)| g).collect(),
            None => self.hidden.clear(),
            Some(name) => match self.hidden.iter().position(|h| *h == name) {
                Some(i) => _ = self.hidden.remove(i),
                None => self.hidden.push(name),
            },
        }
        self.chosen = true;
        self.save(cx, |_| {});
        cx.notify();
    }

    fn title(&self) -> String {
        self.manga["title"].as_str().unwrap_or_default().to_string()
    }

    fn refresh_downloaded(&mut self, cx: &mut Context<Self>) {
        self.downloaded = crate::downloads::downloaded(&self.source, &self.title());
        cx.notify();
    }

    fn job(&self, i: usize) -> crate::downloads::Job {
        crate::downloads::Job { source: self.source.clone(), title: self.title(), chapter: self.visible[i].clone() }
    }

    fn is_downloaded(&self, i: usize) -> bool {
        self.downloaded.contains(&crate::downloads::chapter_name(&self.visible[i]))
    }

    fn download(&mut self, rows: Vec<usize>, cx: &mut Context<Self>) {
        let jobs = rows.into_iter().map(|i| self.job(i)).collect();
        crate::downloads::get(cx).update(cx, |d, cx| d.enqueue(jobs, cx));
        self.row_menu = None;
        cx.notify();
    }

    /// Changes the saved record, creating it if needed.
    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut crate::store::Series)) {
        let (source, manga) = (self.source.clone(), self.manga.clone());
        self.store.update(cx, |store, cx| store.update(&source.id, &source.name, &manga, cx, |_| {}));
        self.save(cx, f);
        self.row_menu = None;
        crate::sync::run(self.store.clone(), cx);
        cx.notify();
    }

    fn open_tracking(&mut self, cx: &mut Context<Self>) {
        let key = (self.source.id.clone(), self.url().to_string());
        let (store, title) = (self.store.clone(), self.title());
        let view = cx.new(|cx| crate::tracking::Tracking::new(store, key, title, cx));
        self._subs.push(cx.subscribe(&view, |this, _, _: &crate::tracking::Close, cx| {
            this.tracking = None;
            cx.notify();
        }));
        self.tracking = Some(view);
        cx.notify();
    }

    fn url_of(&self, i: usize) -> String {
        self.visible[i]["url"].as_str().unwrap_or_default().to_string()
    }

    fn categories_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let all = self.store.read(cx).categories();
        let mine = self.store.read(cx).get(&self.source.id, self.url()).map(|s| s.categories.clone()).unwrap_or_default();
        let names: Vec<String> = all.iter().filter(|c| mine.contains(&c.order)).map(|c| c.name.clone()).collect();
        let label = match names.len() {
            0 => "No category".to_string(),
            1 => names[0].clone(),
            n => format!("{} + {}", names[0], n - 1),
        };
        let button = pill("categories", label, false).on_click(cx.listener(|this, _, _, cx| {
            let just_closed = this.cat_closed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300));
            this.cat_menu = !this.cat_menu && !just_closed;
            cx.notify();
        }));
        let rows = all.into_iter().map(|c| {
            let on = mine.contains(&c.order);
            let order = c.order;
            div()
                .id(("category", order as usize))
                .px(u(12.))
                .py(u(8.))
                .flex()
                .items_center()
                .gap(u(10.))
                .rounded(u(6.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(crate::HOVER)))
                .child(
                    div()
                        .flex_none()
                        .size(u(16.))
                        .rounded(u(4.))
                        .border_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(on, |d| d.bg(rgb(crate::ACCENT)).border_color(rgb(crate::ACCENT)).child(crate::check(12., crate::BG)))
                        .when(!on, |d| d.border_color(rgb(LINE))),
                )
                .child(div().flex_1().min_w_0().truncate().text_size(u(13.)).child(c.name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |s| {
                        if let Some(i) = s.categories.iter().position(|o| *o == order) {
                            s.categories.remove(i);
                        } else {
                            s.categories.push(order);
                        }
                    })
                }))
        });
        let empty = self.store.read(cx).categories().is_empty();
        let panel = div()
            .id("categories-menu")
            .occlude()
            .w(u(240.))
            .p(u(5.))
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .children(rows)
            .when(empty, |d| d.child(div().p(u(10.)).text_size(u(13.)).text_color(rgb(MUTED)).child("No categories yet. Add them in Settings.")))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.cat_menu = false;
                this.cat_closed = Some(std::time::Instant::now());
                cx.notify();
            }));
        div()
            .relative()
            .child(button)
            .when(self.cat_menu, |d| d.child(gpui::deferred(panel.absolute().top(u(50.)).left_0()).with_priority(2)))
            .into_any_element()
    }

    /// Unread chapters that aren't on disk or in the queue, oldest first so they finish in reading order.
    fn unread_not_downloaded(&self, cx: &App) -> Vec<usize> {
        let states = &crate::downloads::get(cx).read(cx).states;
        (0..self.visible.len())
            .rev()
            .filter(|&i| !self.read.get(i).copied().unwrap_or(false) && !self.is_downloaded(i) && !states.contains_key(&self.url_of(i)))
            .collect()
    }

    fn download_badge(&self, i: usize, cx: &App) -> Option<AnyElement> {
        use crate::downloads::State;
        let state = crate::downloads::get(cx).read(cx).states.get(&self.url_of(i)).cloned();
        let text = |t: String, color: u32| mono(div()).flex_none().text_size(u(11.)).text_color(rgb(color)).child(t).into_any_element();
        match state {
            Some(State::Running(done, total)) => Some(text(format!("{done}/{total}"), crate::ACCENT_TEXT)),
            Some(State::Queued) => Some(text("queued".into(), FAINT)),
            Some(State::Failed(_)) => Some(text("failed".into(), crate::ACCENT_TEXT)),
            None if self.is_downloaded(i) => Some(gpui::svg().path("icons/downloaded.svg").flex_none().size(u(13.)).text_color(rgb(FAINT)).into_any_element()),
            None => None,
        }
    }

    fn row_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (i, at) = self.row_menu?;
        let read = self.read.get(i).copied().unwrap_or(false);
        let bookmarked = self.bookmarks.contains(&self.url_of(i));
        let older = self.visible.len().saturating_sub(i + 1);
        let item = |id: &'static str, label: String| {
            div().id(id).px(u(12.)).py(u(7.)).rounded(u(6.)).text_size(u(13.)).cursor_pointer().hover(|s| s.bg(rgb(crate::HOVER))).child(label)
        };
        let menu = div()
            .id("chapter-actions")
            .occlude()
            .w(u(230.))
            .p(u(5.))
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .child(item("mark", if read { "Mark as unread".into() } else { "Mark as read".into() }).on_click(cx.listener(move |this, _, _, cx| {
                let url = this.url_of(i);
                this.edit(cx, |s| s.mark(&[url], !read));
            })))
            .when(older > 0, |d| {
                d.child(item("mark-older", format!("Mark the {older} before it as read")).on_click(cx.listener(move |this, _, _, cx| {
                    let urls: Vec<String> = (i + 1..this.visible.len()).map(|j| this.url_of(j)).collect();
                    this.edit(cx, |s| s.mark(&urls, true));
                })))
            })
            .child(item("bookmark", if bookmarked { "Remove bookmark".into() } else { "Bookmark".into() }).on_click(cx.listener(move |this, _, _, cx| {
                let url = this.url_of(i);
                this.edit(cx, |s| s.toggle_bookmark(&url));
            })))
            .map(|d| {
                let url = self.url_of(i);
                if crate::downloads::get(cx).read(cx).states.contains_key(&url) {
                    d.child(item("cancel-download", "Cancel download".into()).on_click(cx.listener(move |this, _, _, cx| {
                        let url = this.url_of(i);
                        crate::downloads::get(cx).update(cx, |d, cx| d.cancel(&url, cx));
                        this.row_menu = None;
                    })))
                } else if self.is_downloaded(i) {
                    d.child(item("delete-download", "Delete download".into()).on_click(cx.listener(move |this, _, _, cx| {
                        let job = this.job(i);
                        crate::downloads::delete(&job.source, &job.title, &job.chapter);
                        this.row_menu = None;
                        this.refresh_downloaded(cx);
                    })))
                } else if self.source.installed {
                    d.child(item("download", "Download".into()).on_click(cx.listener(move |this, _, _, cx| this.download(vec![i], cx))))
                } else {
                    d
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.row_menu = None;
                cx.notify();
            }));
        Some(gpui::deferred(gpui::anchored().position(at).snap_to_window_with_margin(u(8.)).child(menu)).with_priority(2).into_any_element())
    }

    fn read(&mut self, index: usize, page: usize, cx: &mut Context<Self>) {
        cx.emit(MangaEvent::Read { chapters: self.visible.clone(), index, page });
    }

    fn url(&self) -> &str {
        self.manga["url"].as_str().unwrap_or_default()
    }

    fn text(&self, key: &str) -> Option<SharedString> {
        self.manga[key].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string().into())
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.items.get(ix).copied() {
            Some(Item::Hero) => self.hero(cx),
            Some(Item::Bar) => self.bar(cx),
            Some(Item::Header) => div()
                .w_full()
                .px(u(PAD))
                .pt(u(14.))
                .child(
                    mono(div())
                        .w_full()
                        .text_size(u(11.))
                        .text_color(rgb(FAINT))
                        .flex()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(LINE))
                        .child(div().flex_1().child("CHAPTER"))
                        .child(div().child("DATE")),
                )
                .into_any_element(),
            Some(Item::Row(r)) => {
                let i = self.rows[r];
                let ch = &self.visible[i];
                let name: SharedString = ch["name"].as_str().unwrap_or("Chapter").to_string().into();
                let date = ch["dateUpload"].as_i64().filter(|ms| *ms > 0).map(ymd).unwrap_or_default();
                let group = ch["scanlator"].as_str().unwrap_or("").to_string();
                let read = self.read.get(i).copied().unwrap_or(false);
                let bookmarked = ch["url"].as_str().is_some_and(|u| self.bookmarks.contains(u));
                let open = self.row_menu.is_some_and(|(m, _)| m == i);
                div()
                    .w_full()
                    .px(u(PAD))
                    .child(
                        div()
                            .id(("chapter", i))
                            .group("chapter-row")
                            .when(open, |d| d.bg(rgb(SURFACE)))
                            .w_full()
                            .flex()
                            .items_center()
                            .py(u(9.))
                            .px(u(4.))
                            .border_b_1()
                            .border_color(rgb(RULE))
                            .hover(|s| s.bg(rgb(SURFACE)))
                            .cursor_pointer()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(u(6.))
                                            .when(bookmarked, |d| d.child(gpui::svg().path("icons/bookmark.svg").flex_none().size(u(12.)).text_color(rgb(crate::ACCENT))))
                                            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(rgb(if read { FAINT } else { crate::TEXT })).child(name)),
                                    )
                                    .when(!group.is_empty(), |d| d.child(div().truncate().text_size(u(12.)).text_color(rgb(FAINT)).child(group))),
                            )
                            .children(self.download_badge(i, cx))
                            .child(mono(div()).flex_none().text_color(rgb(if read { FAINT } else { MUTED })).child(date))
                            .child(
                                div()
                                    .id(("more", i))
                                    .flex_none()
                                    .w(u(24.))
                                    .h(u(24.))
                                    .rounded(u(6.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(MUTED))
                                    .invisible()
                                    .when(open, |d| d.visible())
                                    .group_hover("chapter-row", |s| s.visible())
                                    .hover(|s| s.bg(rgb(crate::HOVER)).text_color(rgb(crate::TEXT)))
                                    .child("⋯")
                                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.row_menu = if this.row_menu.is_some_and(|(m, _)| m == i) { None } else { Some((i, ev.position())) };
                                        cx.notify();
                                    })),
                            )
                            .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                this.row_menu = Some((i, ev.position));
                                cx.notify();
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| this.read(i, 0, cx))),
                    )
                    .into_any_element()
            }
            Some(Item::Empty) => div()
                .px(u(PAD))
                .py_6()
                .text_color(rgb(MUTED))
                .child(if self.status.is_empty() { "No chapters match." } else { "" })
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let groups = scanlator_counts(&self.chapters);
        let note = if self.visible.len() == self.chapters.len() {
            format!("{}", self.chapters.len())
        } else {
            format!("{} of {}", self.visible.len(), self.chapters.len())
        };
        let picker = (groups.len() > 1).then(|| self.scanlator_menu(groups, cx));
        div()
            .w_full()
            .px(u(PAD))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().w_full().flex().items_end().gap_4().child(div().flex_1().min_w_0().child(heading("Chapters", note))).children(picker))
                    .child(self.search.clone()),
            )
            .into_any_element()
    }

    fn scanlator_menu(&mut self, groups: Vec<(String, usize)>, cx: &mut Context<Self>) -> AnyElement {
        let shown: Vec<&String> = groups.iter().map(|(g, _)| g).filter(|g| !self.hidden.contains(g)).collect();
        let label = match shown.len() {
            n if n == groups.len() => "All scanlators".to_string(),
            0 => "No scanlators".to_string(),
            1 => shown[0].clone(),
            n => format!("{} + {}", shown[0], n - 1),
        };
        let button = div()
            .id("scanlators")
            .h(u(40.))
            .max_w(u(280.))
            .px(u(16.))
            .rounded(u(20.))
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .items_center()
            .gap(u(8.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(div().min_w_0().truncate().text_size(u(13.)).child(label))
            .child(div().flex_none().text_size(u(10.)).text_color(rgb(FAINT)).child("▼"))
            .on_click(cx.listener(|this, _, _, cx| {
                let just_closed = this.menu_closed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300));
                this.menu = !this.menu && !just_closed;
                cx.notify();
            }));
        let row = |id: SharedString, on: bool, name: String, count: Option<usize>| {
            div()
                .id(id)
                .px(u(14.))
                .py(u(8.))
                .flex()
                .items_center()
                .gap(u(10.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(crate::HOVER)))
                .child(
                    div()
                        .flex_none()
                        .size(u(16.))
                        .rounded(u(4.))
                        .border_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(u(11.))
                        .when(on, |d| d.bg(rgb(crate::ACCENT)).border_color(rgb(crate::ACCENT)).child(crate::check(12., crate::BG)))
                        .when(!on, |d| d.border_color(rgb(LINE))),
                )
                .child(div().flex_1().min_w_0().truncate().text_size(u(13.)).child(name))
                .children(count.map(|n| mono(div()).text_color(rgb(FAINT)).child(n.to_string())))
        };
        let all = groups.len() == shown.len();
        let mut items = vec![row("sc-all".into(), all, "All scanlators".into(), None)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_scanlator(None, cx)))
            .into_any_element()];
        for (i, (g, n)) in groups.iter().enumerate() {
            let name = g.clone();
            items.push(
                row(format!("sc-{i}").into(), !self.hidden.contains(g), g.clone(), Some(*n))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_scanlator(Some(name.clone()), cx)))
                    .into_any_element(),
            );
        }
        let panel = div()
            .id("scanlator-list")
            .mt(u(6.))
            .w(u(280.))
            .max_h(u(360.))
            .overflow_y_scroll()
            .py(u(6.))
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .occlude()
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = false;
                this.menu_closed = Some(std::time::Instant::now());
                cx.notify();
            }));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(button)
            .when(self.menu, |d| d.child(gpui::deferred(gpui::anchored().snap_to_window_with_margin(u(8.)).child(panel))))
            .into_any_element()
    }

    fn hero(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let url = self.manga["thumbnailUrl"].as_str().map(str::to_string);
        let fetch = self.source.installed.then(|| Fetch::Cover(self.source.clone()));
        let cover = self.covers.get(&self.cache, fetch, url.as_deref(), |m: &mut MangaView| &mut m.covers, cx);
        let saved = self.store.read(cx).get(&self.source.id, self.url()).cloned();
        let in_library = saved.as_ref().is_some_and(|s| s.in_library);
        let installed = self.source.installed;
        let resume = saved.as_ref().and_then(|s| {
            let p = s.progress.as_ref()?;
            Some((self.visible.iter().position(|c| c["url"] == p.chapter_url.as_str())?, p.page, p.chapter_name.clone()))
        });
        let status = match self.manga["status"].as_i64() {
            Some(1) => "ongoing",
            Some(2) => "completed",
            Some(3) => "licensed",
            Some(4) => "publishing finished",
            Some(5) => "cancelled",
            Some(6) => "on hiatus",
            _ => "",
        };
        let kicker = [self.source.name.as_str(), status, &self.status].iter().filter(|s| !s.is_empty()).map(|s| s.to_uppercase()).collect::<Vec<_>>().join(" · ");
        let people: SharedString = [self.text("author"), self.text("artist")]
            .into_iter()
            .flatten()
            .map(|s| s.to_string())
            .fold(Vec::<String>::new(), |mut v, s| {
                if !v.contains(&s) {
                    v.push(s)
                }
                v
            })
            .join(", ")
            .into();
        let tags: Vec<SharedString> = self
            .text("genre")
            .map(|g| g.split(',').map(|t| t.trim().to_string().into()).filter(|t: &SharedString| !t.is_empty()).collect())
            .unwrap_or_default();
        let title = self.text("title").unwrap_or_default();
        let has_chapters = !self.visible.is_empty();
        let to_download = self.unread_not_downloaded(cx).len();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(u(14.))
            .px(u(PAD))
            .pt(u(22.))
            .pb(u(28.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(mono(div()).flex_1().min_w_0().truncate().text_size(u(11.)).text_color(rgb(ACCENT_TEXT)).child(kicker))
                    .child(
                        div()
                            .id("close")
                            .flex_none()
                            .size(u(30.))
                            .rounded(u(15.))
                            .border_1()
                            .border_color(rgb(LINE))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(MUTED))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
                            .child("✕")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(MangaEvent::Back))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(u(16.))
                    .child(
                        div()
                            .flex_none()
                            .w(u(120.))
                            .h(u(180.))
                            .rounded(u(4.))
                            .overflow_hidden()
                            .bg(rgb(SURFACE))
                            .when_some(cover, |d, image| d.child(img(image).size_full().object_fit(ObjectFit::Cover))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(8.))
                            .child(
                                div()
                                    .font_family(crate::fonts::display())
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_size(u(28.))
                                    .line_height(relative(1.))
                                    .line_clamp(4)
                                    .text_ellipsis()
                                    .child(title.to_uppercase()),
                            )
                            .when(!people.is_empty(), |d| d.child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(people)))
                            .when(!tags.is_empty(), |d| {
                                d.child(div().flex().flex_wrap().gap(u(6.)).mt_1().children(tags.into_iter().map(|t| {
                                    div().px(u(9.)).py(u(2.)).rounded(u(11.)).border_1().border_color(rgb(LINE)).text_size(u(12.)).text_color(rgb(BODY)).child(t)
                                })))
                            }),
                    ),
            )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(u(8.))
                            .when(!installed, |d| d.child(pill("migrate", "Migrate to another source", true).on_click(cx.listener(|_, _, _, cx| cx.emit(MangaEvent::Migrate)))))
                            .when(installed, |d| d.map(|d| match resume {
                                Some((index, page, name)) => d.child(
                                    pill("continue", format!("Continue · {name}"), true).max_w_full().on_click(cx.listener(move |this, _, _, cx| this.read(index, page, cx))),
                                ),
                                None if has_chapters => d.child(pill("first", "Start reading", true).on_click(cx.listener(|this, _, _, cx| {
                                    let last = this.visible.len() - 1;
                                    this.read(last, 0, cx)
                                }))),
                                None => d,
                            }))
                            .when(installed && has_chapters, |d| d.child(pill("newest", "Newest chapter", false).on_click(cx.listener(|this, _, _, cx| this.read(0, 0, cx)))))
                            .when(installed && to_download > 0, |d| {
                                d.child(pill("download-unread", format!("Download {to_download} unread"), false).on_click(cx.listener(|this, _, _, cx| {
                                    let rows = this.unread_not_downloaded(cx);
                                    this.download(rows, cx)
                                })))
                            })
                            .child(
                                pill("library", if in_library { "In library" } else { "+ Add to library" }, false)
                                    .when(in_library, |d| d.gap_2().child(crate::check(16., TEXT)).flex_row_reverse())
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_library(cx))),
                            )
                            .when(in_library, |d| d.child(self.categories_button(cx)))
                            .when(in_library, |d| {
                                let tracked = saved.as_ref().is_some_and(|s| !s.tracks.is_empty());
                                d.child(
                                    pill("tracking", if tracked { "Tracked" } else { "Tracking" }, false)
                                        .when(tracked, |d| d.gap_2().child(crate::check(16., TEXT)).flex_row_reverse())
                                        .on_click(cx.listener(|this, _, _, cx| this.open_tracking(cx))),
                                )
                            })
                            .when(installed && saved.is_some(), |d| d.child(pill("migrate", "Migrate", false).on_click(cx.listener(|_, _, _, cx| cx.emit(MangaEvent::Migrate))))),
            )
            .when_some(self.text("description"), |d, t| {
                d.child(div().text_size(u(14.)).line_height(relative(1.55)).text_color(rgb(BODY)).child(crate::rich::render(&t)))
            })
            .into_any_element()
    }
}

fn mono(d: gpui::Div) -> gpui::Div {
    d.font_family(crate::fonts::mono()).text_size(u(12.))
}

impl Render for MangaView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        self.visible = Arc::new(visible_chapters(&self.chapters, &self.hidden));
        let saved = self.store.read(cx).get(&self.source.id, self.url());
        self.read = saved.map_or_else(|| vec![false; self.visible.len()], |s| s.read_flags(&self.visible));
        self.bookmarks = saved.map(|s| s.bookmarks.clone()).unwrap_or_default();
        let query = self.search.read(cx).text.trim().to_lowercase();
        self.rows = (0..self.visible.len())
            .filter(|&i| query.is_empty() || self.visible[i]["name"].as_str().unwrap_or("").to_lowercase().contains(&query))
            .collect();
        let mut items = vec![Item::Hero, Item::Bar, Item::Header];
        if self.rows.is_empty() {
            items.push(Item::Empty);
        }
        items.extend((0..self.rows.len()).map(Item::Row));
        let old = self.list.item_count();
        if old == 0 {
            self.list.reset(items.len());
        } else if items.len() != old {
            // Only the rows changed, so keep the scroll position.
            self.list.splice(3..old, items.len() - 3);
        }
        self.list.remeasure_items(0..3.min(items.len()));
        self.items = items;
        let this = cx.entity();
        let menu = self.row_menu(cx);
        div()
            .relative()
            .size_full()
            .pb(u(24.))
            .child(list(self.list.clone(), move |ix, _, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full())
            .children(menu)
            .children(self.tracking.clone())
    }
}

/// Epoch milliseconds to YYYY-MM-DD (UTC) without a date library.
pub fn ymd(ms: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_dates() {
        assert_eq!(super::ymd(0), "1970-01-01");
        assert_eq!(super::ymd(951_782_400_000), "2000-02-29");
        assert_eq!(super::ymd(1_763_337_600_000), "2025-11-17");
    }
}
