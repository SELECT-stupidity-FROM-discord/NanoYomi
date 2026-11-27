//! Light novel mode: LNReader's JavaScript plugins, with a library, history and extensions of their own.
//! Nothing here reads or writes manga data. Switching modes swaps the whole window content.

mod browse;
mod browser;
mod catalog;
pub mod js;
mod library;
mod novel;
mod palette;
pub mod plugins;
mod reader;
mod settings;
pub mod store;
pub mod text;

use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover};
use crate::images::ImageCache;
use crate::sidebar::{Dragged, NARROW, Tip, WIDE, item, ja, label};
use crate::store::Store;
use crate::{ACCENT, ACCENT_TEXT, CJK, FAINT, HOVER, LINE, MUTED, RULE, TEXT, u};
use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, Context, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Window, div, prelude::FluentBuilder, rgb,
};
use plugins::Plugins;
use std::path::PathBuf;
use std::sync::OnceLock;
use store::{LnStore, Novel};

pub const PAD: f32 = 40.;
pub const GAP: f32 = 24.;

/// Open a novel's page.
pub struct OpenNovel {
    pub source: String,
    pub path: String,
}

/// Open the reader on a chapter of a stored novel.
pub struct ReadChapter {
    pub source: String,
    pub path: String,
    pub chapter: String,
}

/// Asks the window to go back to manga.
pub struct SwitchMode;

static CHAPTER_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn chapter_file(source: &str, path: &str) -> Option<PathBuf> {
    CHAPTER_DIR.get().map(|d| d.join(format!("{:016x}.html", crate::fnv(&format!("{source}\n{path}")))))
}

pub fn clear_chapter_cache() {
    if let Some(dir) = CHAPTER_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Runs a plugin call. When a site answers with a Cloudflare check, solves it in a web view and tries once more.
pub(crate) async fn guarded<T, Fut: std::future::Future<Output = Result<T, String>>>(cx: &mut gpui::AsyncApp, mut call: impl FnMut() -> Fut) -> Result<T, String> {
    let first = call().await;
    let Some(url) = first.as_ref().err().and_then(|e| js::challenge_url(e)).map(str::to_string) else { return first };
    let _solving = crate::SOLVING.lock().await;
    // Another request may have passed the check while this one waited.
    let second = call().await;
    if !second.as_ref().err().is_some_and(|e| js::challenge_url(e).is_some()) {
        return second;
    }
    let solved = cx.update(|cx| crate::challenge::solve(cx, crate::challenge::Sink::Ln, url, crate::challenge::browser_user_agent(String::new())));
    if !solved.await.unwrap_or(false) {
        return Err("Verification was cancelled".into());
    }
    let third = call().await;
    // The check passed in a web view but plain HTTP is still turned away, so use the web view for this site.
    let Some(again) = third.as_ref().err().and_then(|e| js::challenge_url(e)).map(str::to_string) else { return third };
    browser::route(&again);
    call().await.map_err(|e| if js::challenge_url(&e).is_some() { "The site still wants a check. Try again in a moment.".to_string() } else { e })
}

/// How to fetch a novel's cover: through its site while the plugin is installed, else only from disk.
pub(crate) fn cover_fetch(plugins: &Plugins, source: &str) -> Option<Fetch> {
    plugins.entry(source).map(|e| Fetch::Web(e.site.clone()))
}

/// A cover with its title and a line or two under it. The caller adds the click.
pub(crate) fn card(id: impl Into<gpui::ElementId>, image: Option<std::sync::Arc<gpui::RenderImage>>, title: &str, w: f32, badge: usize, lines: Vec<String>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(u(w))
        .flex()
        .flex_col()
        .gap(u(4.))
        .cursor_pointer()
        .child(div().mb(u(4.)).child(cover(image, title, w, w * CARD_H / CARD_W, badge)))
        .child(div().text_size(u(13.5)).font_weight(FontWeight::MEDIUM).truncate().child(title.to_string()))
        .children(lines.into_iter().map(|l| div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).truncate().child(l)))
}

pub(crate) fn notice(title: &str, hint: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .pb(u(80.))
        .child(div().text_size(u(48.)).text_color(rgb(MUTED)).child("( o_O )"))
        .child(div().text_size(u(18.)).font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
        .child(div().max_w(u(560.)).text_color(rgb(MUTED)).child(hint.into()))
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Library,
    Updates,
    History,
    Browse,
    Extensions,
    Settings,
}

pub struct LnApp {
    store: Entity<LnStore>,
    plugins: Entity<Plugins>,
    main: Entity<Store>,
    cache: Entity<ImageCache>,
    screen: Screen,
    library: Entity<library::Library>,
    browse: Entity<browse::Browse>,
    catalog: Option<Entity<catalog::Catalog>>,
    settings: Option<Entity<settings::Settings>>,
    novel: Option<(Entity<novel::NovelView>, Subscription)>,
    /// The panel that's sliding shut, kept until it's out of sight.
    closing: Option<Entity<novel::NovelView>>,
    /// Bumped each time the panel opens or closes so its slide starts over.
    panel_moves: usize,
    reader: Option<(Entity<reader::LnReader>, Subscription)>,
    icons: Covers,
    /// Bumped when the sidebar opens or closes, so its slide starts over.
    sidebar_moves: usize,
    was_narrow: Option<bool>,
    palette: Option<palette::LnPalette>,
    /// Hidden web views that make requests for sites that won't answer plain HTTP.
    browsers: browser::Views,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SwitchMode> for LnApp {}

impl LnApp {
    pub fn new(data_dir: PathBuf, main: Entity<Store>, cache: Entity<ImageCache>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dir = data_dir.join("ln");
        let _ = std::fs::create_dir_all(&dir);
        let _ = CHAPTER_DIR.set(dir.join("chapters"));
        let store = cx.new(|_| LnStore::load(dir.join("library.json")));
        let plugins = cx.new(|cx| Plugins::new(dir.join("plugins"), cx));
        let repos = store.read(cx).prefs().repos.clone();
        plugins.update(cx, |p, cx| p.refresh_index(repos, cx));
        let library = cx.new(|cx| library::Library::new(store.clone(), plugins.clone(), cache.clone(), cx));
        let browse = cx.new(|cx| browse::Browse::new(plugins.clone(), cache.clone(), cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut jobs = browser::queue();
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt;
            while let Some(job) = jobs.next().await {
                let Ok(ready) = this.update_in(cx, |this, window, _| this.browsers.ensure(&job.origin, window)) else { break };
                let Some(ready) = ready else {
                    browser::fail(job.id, "The system web view won't start");
                    continue;
                };
                let mut waited = 0;
                while !ready.load(std::sync::atomic::Ordering::Relaxed) && waited < 300 {
                    cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                    waited += 1;
                }
                if !ready.load(std::sync::atomic::Ordering::Relaxed) {
                    browser::fail(job.id, "The site's page didn't load");
                    continue;
                }
                let _ = this.update(cx, |this, _| this.browsers.run(&job.origin, &job.script));
            }
        })
        .detach();
        let subs = vec![
            cx.subscribe_in(&library, window, |this, _, ev: &OpenNovel, window, cx| this.open_novel(&ev.source, &ev.path, window, cx)),
            cx.subscribe_in(&library, window, |this, _, ev: &ReadChapter, window, cx| this.read_stored(ev, window, cx)),
            cx.subscribe_in(&library, window, |this, _, _: &library::GoBrowse, _, cx| this.go(Screen::Browse, cx)),
            cx.subscribe_in(&browse, window, |this, _, ev: &OpenNovel, window, cx| this.open_novel(&ev.source, &ev.path, window, cx)),
            cx.observe(&plugins, |_, _, cx| cx.notify()),
            cx.observe(&main, |_, _, cx| cx.notify()),
        ];
        LnApp { store, plugins, main, cache, screen: Screen::Library, library, browse, catalog: None, settings: None, novel: None, closing: None, panel_moves: 0, reader: None, icons: Covers::default(), browsers: Default::default(), sidebar_moves: 0, was_narrow: None, palette: None, focus, _subs: subs }
    }

    fn close_novel(&mut self, cx: &mut Context<Self>) {
        let Some((view, _)) = self.novel.take() else { return };
        self.closing = Some(view);
        self.panel_moves += 1;
        let moves = self.panel_moves;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::SLIDE).await;
            let _ = this.update(cx, |this, cx| {
                if this.panel_moves == moves {
                    this.closing = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        self.close_novel(cx);
        self.library.update(cx, |l, cx| l.show(screen == Screen::Updates, screen == Screen::History, cx));
        match screen {
            Screen::Browse => self.browse.update(cx, |b, cx| b.activate(cx)),
            Screen::Extensions if self.catalog.is_none() => {
                let (plugins, store, cache) = (self.plugins.clone(), self.store.clone(), self.cache.clone());
                self.catalog = Some(cx.new(|cx| catalog::Catalog::new(plugins, store, cache, cx)));
            }
            Screen::Settings if self.settings.is_none() => {
                let store = self.store.clone();
                self.settings = Some(cx.new(|cx| settings::Settings::new(store, cx)));
            }
            _ => {}
        }
        cx.notify();
    }

    fn open_novel(&mut self, source: &str, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (store, plugins, cache) = (self.store.clone(), self.plugins.clone(), self.cache.clone());
        let view = cx.new(|cx| novel::NovelView::new(source, path, store, plugins, cache, cx));
        let sub = cx.subscribe_in(&view, window, |this, view, ev: &novel::NovelEvent, window, cx| match ev {
            novel::NovelEvent::Back => this.close_novel(cx),
            novel::NovelEvent::Read(index) => {
                let n = view.read(cx).novel().clone();
                this.open_reader(n, *index, window, cx);
            }
        });
        if self.novel.is_none() {
            self.panel_moves += 1;
        }
        self.closing = None;
        self.novel = Some((view, sub));
        cx.notify();
    }

    fn read_stored(&mut self, ev: &ReadChapter, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.store.read(cx).get(&ev.source, &ev.path).cloned() else { return };
        let index = n.chapters.iter().position(|c| c.path == ev.chapter).unwrap_or_else(|| n.resume_index());
        self.open_reader(n, index, window, cx);
    }

    fn open_reader(&mut self, novel: Novel, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.plugins.update(cx, |p, _| p.source(&novel.source));
        let store = self.store.clone();
        let view = cx.new(|cx| reader::LnReader::new(novel, index, source, store, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, _: &reader::Close, window, cx| {
            this.reader = None;
            this.focus.focus(window, cx);
            cx.notify();
        });
        self.reader = Some((view, sub));
        cx.notify();
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = self.main.read(cx).prefs().sidebar_collapsed;
        // ⌘\ flips the same preference from the window, so the slide starts from whichever side changed it.
        if self.was_narrow.is_some_and(|w| w != narrow) {
            self.sidebar_moves += 1;
        }
        self.was_narrow = Some(narrow);
        let browsing = self.screen == Screen::Browse;
        let current = self.browse.read(cx).source_id().map(str::to_string);
        let order = self.store.read(cx).prefs().source_order.clone();
        let mut installed = self.plugins.read(cx).installed.clone();
        // Sources you haven't placed keep their order, after the ones you have.
        installed.sort_by_key(|e| order.iter().position(|o| *o == e.id).unwrap_or(usize::MAX));
        let nav = |screen: Screen, jp: &'static str, en: &'static str, cx: &mut Context<Self>| {
            let on = self.screen == screen;
            item(en, ja(jp, on, narrow), en.into(), on, narrow).on_click(cx.listener(move |this, _, _, cx| this.go(screen, cx)))
        };
        let ids: Vec<String> = installed.iter().map(|e| e.id.clone()).collect();
        let rows: Vec<_> = installed
            .iter()
            .map(|e| {
                let on = browsing && current.as_deref() == Some(e.id.as_str());
                let initial = e.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                let url = Some(e.icon_url.as_str()).filter(|u| !u.is_empty());
                let icon = self.icons.get(&self.cache, Some(Fetch::Icon), url, |a: &mut LnApp| &mut a.icons, cx);
                let short = match (narrow, icon) {
                    (true, icon) => div()
                        .flex_none()
                        .size(u(24.))
                        .rounded(u(12.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if on { ACCENT } else { LINE }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(u(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if on { TEXT } else { MUTED }))
                        .map(|d| match icon {
                            Some(image) => d.child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Cover)),
                            None => d.child(initial),
                        })
                        .into_any_element(),
                    (false, Some(image)) => div()
                        .flex_none()
                        .size(u(18.))
                        .rounded(u(4.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if on { ACCENT } else { LINE }))
                        .child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Cover))
                        .into_any_element(),
                    (false, None) => div().flex_none().size(u(6.)).rounded(u(3.)).bg(rgb(if on { ACCENT } else { LINE })).into_any_element(),
                };
                let lang = e.lang.trim_matches('\u{200e}');
                let name: SharedString = if lang.is_empty() || lang == "English" || lang == "all" { e.name.clone() } else { format!("{} · {lang}", e.name) }.into();
                let id = e.id.clone();
                let (grab, target, line, here, all) = (e.id.clone(), e.id.clone(), ids.clone(), e.id.clone(), ids.clone());
                item(SharedString::from(format!("side-{}", e.id)), short, name.clone(), on, narrow)
                    .group("source-row")
                    .when(!narrow, |d| d.child(crate::sidebar::grip()))
                    .on_drag(Dragged(grab), move |_, _, _, cx| cx.new(|_| Tip(name.clone())))
                    .drag_over::<Dragged>(move |d, from, _, _| {
                        let at = |id: &str| line.iter().position(|i| i == id);
                        if from.0 == here {
                            d
                        } else if at(&from.0) < at(&here) {
                            d.border_b_2().border_color(rgb(ACCENT))
                        } else {
                            d.border_t_2().border_color(rgb(ACCENT))
                        }
                    })
                    .on_drop(cx.listener(move |this, from: &Dragged, _, cx| {
                        let mut ids = all.clone();
                        let (Some(a), Some(b)) = (ids.iter().position(|i| *i == from.0), ids.iter().position(|i| *i == target)) else { return };
                        let moved = ids.remove(a);
                        ids.insert(b, moved);
                        this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.source_order = ids));
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.go(Screen::Browse, cx);
                        this.browse.update(cx, |b, cx| b.select(&id, cx));
                    }))
            })
            .collect();
        let search = div()
            .id("ln-side-search")
            .flex_none()
            .h(u(34.))
            .px(u(10.))
            .rounded(u(8.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(crate::SURFACE))
            .flex()
            .items_center()
            .gap(u(8.))
            .when(narrow, |d| d.justify_center().px_0())
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER)))
            .child(crate::search_icon())
            .when(!narrow, |d| {
                d.child(div().flex_1().child("Search")).child(
                    div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).px(u(5.)).border_1().border_color(rgb(LINE)).rounded(u(4.)).child(if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl K" }),
                )
            })
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)));
        let brand = div()
            .flex()
            .items_center()
            .justify_between()
            .when(narrow, |d| d.flex_col().gap(u(10.)))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .when(!narrow, |d| d.px(u(8.)))
                    .child(div().font_family(CJK).font_weight(FontWeight::BLACK).text_size(u(22.)).text_color(rgb(ACCENT)).child("読"))
                    .when(!narrow, |d| d.child(div().font_family(crate::fonts::display()).font_weight(FontWeight::BLACK).text_size(u(24.)).child("NANO")))
                    .when(!narrow, |d| d.child(div().font_family(crate::fonts::mono()).text_size(u(10.5)).text_color(rgb(ACCENT_TEXT)).child("LN"))),
            )
            .child(
                div()
                    .id("ln-collapse")
                    .size(u(28.))
                    .rounded(u(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .child(gpui::svg().path("icons/sidebar.svg").size(u(16.)).text_color(rgb(FAINT)))
                    .tooltip(move |_, cx| {
                        let key = if cfg!(target_os = "macos") { "⌘\\" } else { "Ctrl \\" };
                        cx.new(|_| Tip(format!("{} sidebar ({key})", if narrow { "Expand" } else { "Collapse" }).into())).into()
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.main.update(cx, |s, cx| {
                            let on = !s.prefs().sidebar_collapsed;
                            s.edit_prefs(cx, |p| p.sidebar_collapsed = on)
                        });
                    })),
            );
        let (from, to) = if narrow { (WIDE, NARROW) } else { (NARROW, WIDE) };
        // The first frame starts settled, so launching doesn't play the slide.
        let from = if self.sidebar_moves == 0 { to } else { from };
        div()
            .flex_none()
            .overflow_hidden()
            .h_full()
            .bg(rgb(0x11100E))
            .border_r_1()
            .border_color(rgb(RULE))
            .flex()
            .flex_col()
            .gap(u(18.))
            .px(u(12.))
            .pt(u(18.))
            .pb(u(14.))
            .child(brand)
            .child(search)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(u(2.))
                    .when(!narrow, |d| d.child(label("READING")))
                    .child(nav(Screen::Library, "書庫", "Library", cx))
                    .child(nav(Screen::Updates, "新着", "Updates", cx))
                    .child(nav(Screen::History, "履歴", "History", cx)),
            )
            .child(if rows.is_empty() {
                div().flex_1().into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(!narrow, |d| d.child(label("SOURCES")))
                    .child(div().id("ln-sources").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(u(2.)).children(rows))
                    .into_any_element()
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(u(2.))
                    .child(nav(Screen::Extensions, "拡張", "Extensions", cx))
                    .child(nav(Screen::Settings, "設定", "Settings", cx))
                    .child(item("ln-mode", ja("切替", false, narrow), "Switch to Manga".into(), false, narrow).on_click(cx.listener(|_, _, _, cx| cx.emit(SwitchMode)))),
            )
            .with_animation(("ln-sidebar", self.sidebar_moves), Animation::new(crate::SLIDE).with_easing(gpui::ease_out_quint()), move |d, t| {
                let w = from + (to - from) * t;
                crate::set_sidebar_width(w);
                d.w(u(w))
            })
    }
}

impl Render for LnApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((reader, _)) = &self.reader {
            return div().size_full().child(reader.clone()).into_any_element();
        }
        let screen: AnyElement = match self.screen {
            Screen::Library | Screen::Updates | Screen::History => self.library.clone().into_any_element(),
            Screen::Browse => self.browse.clone().into_any_element(),
            Screen::Extensions => self.catalog.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
            Screen::Settings => self.settings.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
        };
        // The novel slides open beside the grid, so you keep your place in it.
        let open = self.novel.is_some();
        let panel = self.novel.as_ref().map(|(v, _)| v.clone()).or_else(|| self.closing.clone()).map(|view| {
            let (from, to) = if open { (0., crate::manga::PANEL) } else { (crate::manga::PANEL, 0.) };
            div()
                .flex_none()
                .relative()
                .h_full()
                .overflow_hidden()
                .border_l_1()
                .border_color(rgb(RULE))
                .bg(rgb(0x11100E))
                // A fixed width pinned to the right, so it slides in whole instead of rewrapping.
                .child(div().absolute().top_0().bottom_0().right_0().w(u(crate::manga::PANEL)).child(view))
                .with_animation(("ln-panel", self.panel_moves), Animation::new(crate::SLIDE).with_easing(gpui::ease_out_quint()), move |d, t| {
                    let w = from + (to - from) * t;
                    crate::set_panel_width(w);
                    d.w(u(w))
                })
        });
        if panel.is_none() {
            crate::set_panel_width(0.);
        }
        div()
            .size_full()
            .relative()
            .flex()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &crate::ShowLibrary, _, cx| this.go(Screen::Library, cx)))
            .on_action(cx.listener(|this, _: &crate::ShowBrowse, _, cx| this.go(Screen::Browse, cx)))
            .on_action(cx.listener(|this, _: &crate::ShowExtensions, _, cx| this.go(Screen::Extensions, cx)))
            .on_action(cx.listener(|this, _: &crate::ShowHistory, _, cx| this.go(Screen::History, cx)))
            .on_action(cx.listener(|this, _: &crate::ShowSettings, _, cx| this.go(Screen::Settings, cx)))
            .on_action(cx.listener(|this, _: &crate::Search, window, cx| this.open_palette(window, cx)))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_novel(cx);
                }
            }))
            .child(self.sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(screen))
            .children(panel)
            .children(self.palette(cx))
            .into_any_element()
    }
}
