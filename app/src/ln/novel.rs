//! A novel's page: its details, a library button, and the chapters.

use super::plugins::{Details, Plugins, Source};
use super::store::{LnStore, Novel};
use super::cover_fetch;
use crate::covers::Covers;
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::{ACCENT_TEXT, BODY, FAINT, HOVER, LINE, MUTED, RULE, SURFACE, TEXT, heading, pill, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ObjectFit, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Window, div, img, list, prelude::FluentBuilder, relative, rgb,
};

const PAD: f32 = 24.;
const DATE_W: f32 = 96.;

fn mono(d: gpui::Div) -> gpui::Div {
    d.font_family(crate::fonts::mono()).text_size(u(12.))
}

pub enum NovelEvent {
    Back,
    /// Read the chapter at this index of the novel's list, oldest first.
    Read(usize),
}

pub struct NovelView {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    cache: Entity<ImageCache>,
    source: Option<Source>,
    novel: Novel,
    loading: bool,
    error: Option<SharedString>,
    covers: Covers,
    list: ListState,
    order: Vec<usize>,
    zoom_seen: f32,
    search: Entity<TextInput>,
    /// The chapter whose actions menu is open, and where it opened.
    row_menu: Option<(usize, gpui::Point<gpui::Pixels>)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<NovelEvent> for NovelView {}

fn fill(into: &mut String, from: String) {
    if !from.is_empty() {
        *into = from;
    }
}

impl NovelView {
    pub fn new(source: &str, path: &str, store: Entity<LnStore>, plugins: Entity<Plugins>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let src = plugins.update(cx, |p, _| p.source(source));
        let name = plugins.read(cx).entry(source).map_or(source.to_string(), |e| e.name.clone());
        let stored = store.read(cx).get(source, path).cloned();
        let novel = stored.unwrap_or_else(|| Novel { source: source.into(), source_name: name, path: path.into(), ..Default::default() });
        let search = cx.new(|cx| TextInput::new("Search chapters…", cx));
        let sub = cx.observe(&store, |this, store, cx| {
            if let Some(s) = store.read(cx).get(&this.novel.source, &this.novel.path) {
                this.novel.in_library = s.in_library;
                this.novel.read = s.read.clone();
                this.novel.progress = s.progress.clone();
            }
            cx.notify();
        });
        let typed = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        let mut this = NovelView { store, plugins, cache, source: src, novel, loading: false, error: None, covers: Covers::default(), list: ListState::new(0, ListAlignment::Top, u(800.)), order: vec![], zoom_seen: crate::zoom(), search, row_menu: None, _subs: vec![sub, typed] };
        this.refresh(cx);
        this
    }

    pub fn novel(&self) -> &Novel {
        &self.novel
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            self.error = Some(format!("{} isn't installed. Install it in Extensions to refresh this novel.", self.novel.source_name).into());
            return;
        };
        self.loading = true;
        self.error = None;
        let path = self.novel.path.clone();
        cx.spawn(async move |this, cx| {
            let result = super::guarded(cx, || source.novel(&path)).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(d) => this.apply(d, cx),
                    Err(e) => this.error = Some(e.lines().next().unwrap_or("The source failed").to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, d: Details, cx: &mut Context<Self>) {
        let n = &mut self.novel;
        fill(&mut n.name, d.name);
        fill(&mut n.cover, d.cover);
        fill(&mut n.summary, d.summary);
        fill(&mut n.author, d.author);
        fill(&mut n.status, d.status);
        fill(&mut n.genres, d.genres);
        // A plugin that finds no chapters shouldn't wipe the ones already known.
        let found = (!d.chapters.is_empty()).then_some(d.chapters);
        if let Some(chapters) = &found {
            n.chapters = chapters.clone();
        }
        let stored = self.store.read(cx).get(&n.source, &n.path).is_some();
        if stored {
            let (novel, found) = (n.clone(), found);
            self.store.update(cx, |s, cx| {
                if let Some(chapters) = found {
                    s.found_chapters(&novel.source, &novel.path, chapters, cx);
                }
                s.remember(novel, cx);
            });
        }
    }

    /// Reading and library marks need the novel saved first.
    fn ensure_stored(&mut self, cx: &mut Context<Self>) {
        if self.store.read(cx).get(&self.novel.source, &self.novel.path).is_none() {
            let n = self.novel.clone();
            self.store.update(cx, |s, cx| s.remember(n, cx));
        }
    }

    fn toggle_library(&mut self, cx: &mut Context<Self>) {
        self.ensure_stored(cx);
        let (source, path, on) = (self.novel.source.clone(), self.novel.path.clone(), !self.novel.in_library);
        self.store.update(cx, |s, cx| s.set_in_library(&source, &path, on, cx));
    }

    fn mark(&mut self, paths: Vec<String>, read: bool, cx: &mut Context<Self>) {
        self.ensure_stored(cx);
        self.row_menu = None;
        let (source, path) = (self.novel.source.clone(), self.novel.path.clone());
        self.store.update(cx, |s, cx| {
            s.edit(&source, &path, cx, |n| {
                for p in paths {
                    if read {
                        n.read.insert(p);
                    } else {
                        n.read.remove(&p);
                    }
                }
            })
        });
        cx.notify();
    }
}

impl NovelView {
    fn hero(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let how = cover_fetch(self.plugins.read(cx), &self.novel.source);
        let url = Some(self.novel.cover.clone()).filter(|c| !c.is_empty());
        let image = self.covers.get(&self.cache, how, url.as_deref(), |v: &mut NovelView| &mut v.covers, cx);
        let n = &self.novel;
        let kicker = [n.source_name.as_str(), n.status.as_str()].iter().filter(|s| !s.is_empty()).map(|s| s.to_uppercase()).collect::<Vec<_>>().join(" · ");
        let tags: Vec<SharedString> = n.genres.split(',').map(|t| t.trim().to_string().into()).filter(|t: &SharedString| !t.is_empty()).collect();
        let label = match &n.progress {
            Some(p) => format!("Continue · {}", p.name),
            None if n.read.is_empty() => "Start reading".to_string(),
            None => "Read next".to_string(),
        };
        let newest = n.chapters.len().saturating_sub(1);
        let (start, in_library, has_chapters) = (n.resume_index(), n.in_library, !n.chapters.is_empty());
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(u(14.))
            .px(u(PAD))
            .pt(u(22.))
            .pb(u(20.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().min_w_0().truncate().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(ACCENT_TEXT)).child(kicker))
                    .child(
                        div()
                            .id("ln-close")
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
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NovelEvent::Back))),
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
                            .when_some(image, |d, image| d.child(img(image).size_full().object_fit(ObjectFit::Cover))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(8.))
                            .child(div().font_family(crate::fonts::display()).font_weight(FontWeight::EXTRA_BOLD).text_size(u(28.)).line_height(relative(1.)).line_clamp(4).text_ellipsis().child(n.name.to_uppercase()))
                            .when(!n.author.is_empty(), |d| d.child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(n.author.clone())))
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
                    .when(has_chapters, |d| {
                        d.child(pill("ln-read", label, true).max_w_full().on_click(cx.listener(move |this, _, _, cx| {
                            this.ensure_stored(cx);
                            cx.emit(NovelEvent::Read(start));
                        })))
                    })
                    .when(has_chapters, |d| {
                        d.child(pill("ln-newest", "Newest chapter", false).on_click(cx.listener(move |this, _, _, cx| {
                            this.ensure_stored(cx);
                            cx.emit(NovelEvent::Read(newest));
                        })))
                    })
                    .child(
                        pill("ln-lib", if in_library { "In library" } else { "+ Add to library" }, false)
                            .when(in_library, |d| d.gap_2().child(crate::check(16., TEXT)).flex_row_reverse())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_library(cx))),
                    ),
            )
            .children(self.error.clone().map(|e| div().text_color(rgb(ACCENT_TEXT)).child(e)))
            .when(!n.summary.is_empty(), |d| d.child(div().text_size(u(14.)).line_height(relative(1.55)).text_color(rgb(BODY)).child(crate::rich::render(&n.summary))))
            .into_any_element()
    }

    fn bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let total = self.novel.chapters.len();
        let note: SharedString = if self.loading && total == 0 {
            "loading…".into()
        } else if self.order.len() == total {
            format!("{total}").into()
        } else {
            format!("{} of {total}", self.order.len()).into()
        };
        let oldest_first = self.store.read(cx).prefs().oldest_first;
        let sort = div()
            .id("ln-sort-chapters")
            .h(u(40.))
            .px(u(16.))
            .rounded(u(20.))
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .items_center()
            .text_size(u(13.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(if oldest_first { "Oldest first" } else { "Newest first" })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.oldest_first = !oldest_first));
                this.list.remeasure();
            }));
        div()
            .w_full()
            .px(u(PAD))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().w_full().flex().items_end().gap_4().child(div().flex_1().min_w_0().child(heading("Chapters", note))).child(sort))
                    .child(self.search.clone()),
            )
            .into_any_element()
    }

    fn header(&mut self) -> AnyElement {
        div()
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
                    .px(u(4.))
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(div().flex_1().child("CHAPTER"))
                    .child(div().w(u(DATE_W)).child("DATE"))
                    .child(div().w(u(24.))),
            )
            .into_any_element()
    }

    fn row(&mut self, r: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(i) = self.order.get(r).copied() else { return div().into_any_element() };
        let Some(c) = self.novel.chapters.get(i).cloned() else { return div().into_any_element() };
        let read = self.novel.read.contains(&c.path);
        let here = self.novel.progress.as_ref().filter(|p| p.chapter == c.path && !read).map(|p| format!("{}% read", (p.scroll * 100.).round() as u32));
        let open = self.row_menu.is_some_and(|(m, _)| m == i);
        div()
            .w_full()
            .px(u(PAD))
            .child(
                div()
                    .id(("ln-chapter", i))
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
                            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(rgb(if read { FAINT } else { TEXT })).child(c.name.clone()))
                            .children(here.map(|h| div().truncate().text_size(u(12.)).text_color(rgb(ACCENT_TEXT)).child(h))),
                    )
                    .child(mono(div()).flex_none().w(u(DATE_W)).truncate().text_color(rgb(if read { FAINT } else { MUTED })).child(c.release.clone()))
                    .child(
                        div()
                            .id(("ln-more", i))
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
                            .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ensure_stored(cx);
                        cx.emit(NovelEvent::Read(i));
                    })),
            )
            .into_any_element()
    }

    fn row_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (i, at) = self.row_menu?;
        let path = self.novel.chapters.get(i)?.path.clone();
        let read = self.novel.read.contains(&path);
        // The list runs oldest first, so the ones before it are the older chapters.
        let older = i;
        let item = |id: &'static str, label: String| div().id(id).px(u(12.)).py(u(7.)).rounded(u(6.)).text_size(u(13.)).cursor_pointer().hover(|s| s.bg(rgb(HOVER))).child(label);
        let menu = div()
            .id("ln-chapter-actions")
            .occlude()
            .w(u(230.))
            .p(u(5.))
            .rounded(u(10.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .child(item("ln-mark", if read { "Mark as unread".into() } else { "Mark as read".into() }).on_click(cx.listener(move |this, _, _, cx| this.mark(vec![path.clone()], !read, cx))))
            .when(older > 0, |d| {
                d.child(item("ln-mark-older", format!("Mark the {older} before it as read")).on_click(cx.listener(move |this, _, _, cx| {
                    let paths: Vec<String> = this.novel.chapters[..i].iter().map(|c| c.path.clone()).collect();
                    this.mark(paths, true, cx);
                })))
            })
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.row_menu = None;
                cx.notify();
            }));
        Some(gpui::deferred(gpui::anchored().position(at).snap_to_window_with_margin(u(8.)).child(menu)).with_priority(2).into_any_element())
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match ix {
            0 => self.hero(cx),
            1 => self.bar(cx),
            2 => self.header(),
            _ if self.order.is_empty() => div().px(u(PAD)).py_6().text_color(rgb(MUTED)).child(if self.loading { "" } else { "No chapters match." }).into_any_element(),
            _ => self.row(ix - 3, cx),
        }
    }
}

impl Render for NovelView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).text.trim().to_lowercase();
        let oldest_first = self.store.read(cx).prefs().oldest_first;
        let mut order: Vec<usize> = (0..self.novel.chapters.len()).filter(|&i| query.is_empty() || self.novel.chapters[i].name.to_lowercase().contains(&query)).collect();
        if !oldest_first {
            order.reverse();
        }
        self.order = order;
        if self.zoom_seen != crate::zoom() {
            self.zoom_seen = crate::zoom();
            self.list.remeasure();
        }
        let total = 3 + self.order.len().max(1);
        let old = self.list.item_count();
        if old == 0 {
            self.list.reset(total);
        } else if total != old {
            self.list.splice(3..old, total - 3);
        }
        // The top three change height as details arrive.
        self.list.remeasure_items(0..3.min(total));
        let menu = self.row_menu(cx);
        let this = cx.entity();
        div().relative().size_full().pb(u(24.)).child(list(self.list.clone(), move |ix, _, cx| this.update(cx, |this, cx| this.item(ix, cx))).size_full()).children(menu)
    }
}
