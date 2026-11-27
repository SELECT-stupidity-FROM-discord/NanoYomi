//! Only pages near the reading position stay decoded, in both modes.
//! Pages come through the extension, so its own image handling (decryption, unscrambling) applies.

use crate::images::{self, ImageCache, Picture, picture_size};
use crate::page_cache;
use crate::store::{Progress, Store, now_ms};
use crate::{BG, FAINT, HOVER, LINE, MUTED, SURFACE, Source, TEXT, drop_images, pill};
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, ScrollEvent, ScrollStrategy, SharedString, Size, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window, canvas, div, img, list, prelude::FluentBuilder, px, relative, rgb, rgba,
    uniform_list,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Webtoons read better as a column than stretched across a wide window.
const COLUMN_MAX: f32 = 900.;
const KEY_STEP: f32 = 120.;
const WHEEL_STEP: f32 = 80.;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Paged,
    Vertical,
}

impl Mode {
    /// How many pages to keep decoded behind and ahead.
    fn window(self) -> (usize, usize) {
        match self {
            Mode::Paged => (1, 3),
            Mode::Vertical => (2, 5), // strips are shorter than pages and scroll faster
        }
    }
}

pub enum ReaderEvent {
    Close,
}

pub struct Reader {
    source: Source,
    cache: Entity<ImageCache>,
    store: Entity<Store>,
    manga: Value,
    chapters: Arc<Vec<Value>>,
    /// Lists are newest first, so the next chapter is `chapter - 1`.
    chapter: usize,
    pages: Vec<Value>,
    page: usize,
    mode: Mode,
    /// Paged mode turns right to left: the left arrow and left side go forward.
    rtl: bool,
    strip: ListState,
    inflight: HashSet<usize>,
    /// Pages that failed. They're retried when the reader moves.
    failed: HashSet<usize>,
    /// Bumped on chapter or mode change so late results get dropped.
    generation: u64,
    viewport: Size<Pixels>,
    scale: f32,
    status: SharedString,
    focus: FocusHandle,
    /// The overlay shows until `chrome_until`. `chrome_gen` cancels old hide timers.
    chrome: bool,
    chrome_until: Instant,
    chrome_gen: u64,
    scrubbing: bool,
    chapter_menu: bool,
    chapter_scroll: UniformListScrollHandle,
    glide: f32,
    glide_at: Instant,
    entered_fullscreen: bool,
    refit: bool,
    decoded_for: Size<Pixels>,
}

impl EventEmitter<ReaderEvent> for Reader {}

impl Reader {
    pub fn new(
        source: Source,
        cache: Entity<ImageCache>,
        store: Entity<Store>,
        manga: Value,
        chapters: Arc<Vec<Value>>,
        index: usize,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let vertical = crate::demo_mode() == "vertical" || store.read(cx).prefs().vertical_reader;
        let mode = if vertical { Mode::Vertical } else { Mode::Paged };
        let rtl = store.read(cx).prefs().right_to_left;
        let mut this = Reader {
            source,
            cache,
            store,
            manga,
            chapters,
            chapter: index,
            pages: vec![],
            page: 0,
            mode,
            rtl,
            strip: ListState::new(0, ListAlignment::Top, px(1500.)),
            inflight: HashSet::new(),
            failed: HashSet::new(),
            generation: 0,
            viewport: crate::titlebar::viewport(window),
            scale: window.scale_factor(),
            status: "".into(),
            focus,
            chrome: false,
            chrome_until: Instant::now(),
            chrome_gen: 0,
            scrubbing: false,
            chapter_menu: false,
            chapter_scroll: UniformListScrollHandle::new(),
            glide: 0.,
            glide_at: Instant::now(),
            entered_fullscreen: false,
            refit: false,
            decoded_for: crate::titlebar::viewport(window),
        };
        if this.store.read(cx).prefs().reader_fullscreen && !window.is_fullscreen() {
            window.toggle_fullscreen();
            this.entered_fullscreen = true;
            this.refit = true;
        }
        this.show_chrome(Duration::from_secs(1), cx);
        this.open_chapter(index, page, cx);
        if crate::demo().is_some() {
            this.run_demo(window, cx);
        }
        this
    }

    fn show_chrome(&mut self, for_: Duration, cx: &mut Context<Self>) {
        self.chrome = true;
        self.chrome_until = Instant::now() + for_;
        self.schedule_hide(cx);
    }

    pub fn leave_fullscreen(&mut self, window: &mut Window) {
        if self.entered_fullscreen && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.entered_fullscreen = false;
    }

    fn glide_by(&mut self, dy: f32, cx: &mut Context<Self>) {
        if self.glide == 0. {
            self.glide_at = Instant::now();
        }
        self.glide += dy;
        cx.notify();
    }

    fn extend_chrome(&mut self, cx: &mut Context<Self>) {
        if self.chrome {
            self.chrome_until = self.chrome_until.max(Instant::now()) + Duration::from_secs(5);
            self.schedule_hide(cx);
        }
    }

    fn toggle_chrome(&mut self, cx: &mut Context<Self>) {
        if self.chrome {
            self.chrome = false;
            self.chrome_gen += 1;
            cx.notify();
        } else {
            self.show_chrome(Duration::from_secs(5), cx);
        }
    }

    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        self.chrome_gen += 1;
        let (generation, until) = (self.chrome_gen, self.chrome_until);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(until.saturating_duration_since(Instant::now())).await;
            let _ = this.update(cx, |this, cx| {
                if this.chrome_gen == generation {
                    this.chrome = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn key(&self, i: usize) -> String {
        format!("page:{:?}:{}#{i}", self.mode, self.chapters[self.chapter]["url"].as_str().unwrap_or_default())
    }

    fn column_width(&self) -> f32 {
        f32::from(self.viewport.width).min(COLUMN_MAX)
    }

    /// `usize::MAX` opens the chapter at its last page.
    fn open_chapter(&mut self, index: usize, start: usize, cx: &mut Context<Self>) {
        self.release_all(None, cx);
        self.chapter = index;
        self.pages.clear();
        self.page = 0;
        self.inflight.clear();
        self.generation += 1;
        self.glide = 0.;
        self.strip.reset(0);
        self.status = "Loading chapter…".into();
        cx.notify();
        let generation = self.generation;
        let source = self.source.clone();
        let chapter = self.chapters[index].clone();
        let title = self.manga["title"].as_str().unwrap_or_default().to_string();
        let url = chapter["url"].as_str().unwrap_or_default();
        let unread = self.store.read(cx)
        .get(&self.source.id, self.manga["url"].as_str().unwrap_or_default())
        .is_some_and(|s| s.marks.get(url) == Some(&false));
        let start = if unread { 0 } else { start };
        cx.spawn(async move |this, cx| {
            // A downloaded chapter reads from disk, with no network at all.
            let local = { let (source, chapter) = (source.clone(), chapter.clone()); cx.background_executor().spawn(async move { crate::downloads::local_pages(&source, &title, &chapter) }).await };
            let result = match local {
                Some(files) => Ok(Value::Array(files.into_iter().map(|f| json!({ "local": f })).collect())),
                None => source.call(cx, "pages", json!({ "chapter": chapter })).await,
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(v) => {
                        this.pages = v.as_array().cloned().unwrap_or_default();
                        if this.pages.is_empty() {
                            this.status = "This chapter has no pages".into();
                        } else {
                            this.page = start.min(this.pages.len() - 1);
                            this.strip.reset(this.pages.len() + 1);
                            if this.page > 0 {
                                this.strip.scroll_to(ListOffset { item_ix: this.page, offset_in_item: px(0.) });
                            }
                            this.status = "Loading page…".into();
                            this.sync(None, cx);
                        }
                    }
                    Err(e) => this.status = e.into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn prev_chapter(&mut self, cx: &mut Context<Self>) {
        if self.chapter + 1 < self.chapters.len() {
            self.open_chapter(self.chapter + 1, 0, cx);
        }
    }

    fn next_chapter(&mut self, cx: &mut Context<Self>) {
        if self.chapter > 0 {
            self.open_chapter(self.chapter - 1, 0, cx);
        } else {
            self.status = "You're on the latest chapter".into();
            cx.notify();
        }
    }

    fn go(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.pages.is_empty() {
            return;
        }
        let next = self.page as isize + delta;
        if next < 0 {
            if self.chapter + 1 < self.chapters.len() {
                self.open_chapter(self.chapter + 1, usize::MAX, cx);
            }
        } else if next as usize >= self.pages.len() {
            self.next_chapter(cx);
        } else {
            self.page = next as usize;
            self.status = "Loading page…".into();
            self.sync(Some(window), cx);
            cx.notify();
        }
    }

    /// Which way the right arrow and the right side of the page turn.
    fn forward(&self) -> isize {
        if self.rtl { -1 } else { 1 }
    }

    /// Paged, left to right or right to left.
    fn set_direction(&mut self, rtl: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Vertical {
            self.toggle_mode(window, cx);
        }
        self.rtl = rtl;
        self.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.right_to_left = rtl));
        cx.notify();
    }

    fn toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.release_all(Some(window), cx);
        self.mode = if self.mode == Mode::Paged { Mode::Vertical } else { Mode::Paged };
        let vertical = self.mode == Mode::Vertical;
        self.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.vertical_reader = vertical));
        self.inflight.clear();
        self.generation += 1;
        self.glide = 0.;
        if !self.pages.is_empty() {
            self.strip.reset(self.pages.len() + 1);
            self.strip.scroll_to(ListOffset { item_ix: self.page, offset_in_item: px(0.) });
        }
        self.sync(Some(window), cx);
        cx.notify();
    }

    fn sync(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let len = self.pages.len();
        if len == 0 {
            return;
        }
        self.save_progress(cx);
        let (behind, ahead) = self.mode.window();
        let lo = self.page.saturating_sub(behind);
        let hi = (self.page + ahead).min(len - 1);
        let outside: Vec<String> = (0..len).filter(|i| *i < lo || *i > hi).map(|i| self.key(i)).collect();
        let released: Vec<_> =
            self.cache.update(cx, |c, _| outside.iter().filter_map(|k| c.remove(k)).flat_map(|p| p.to_vec()).collect());
        drop_images(cx, window, released);

        self.failed.clear();
        self.load_next(cx);
    }

    /// One page at a time, current first. Comix drives a hidden browser per page and thrashes if asked for several.
    fn load_next(&mut self, cx: &mut Context<Self>) {
        let len = self.pages.len();
        if len == 0 || !self.inflight.is_empty() {
            return;
        }
        let (behind, ahead) = self.mode.window();
        let lo = self.page.saturating_sub(behind);
        let hi = (self.page + ahead).min(len - 1);
        let mut order = std::iter::once(self.page).chain(self.page + 1..=hi).chain(lo..self.page);
        if let Some(i) = order.find(|i| !self.failed.contains(i) && !self.cache.read(cx).contains(&self.key(*i))) {
            self.inflight.insert(i);
            self.fetch_page(i, cx);
        }
    }

    /// Saving the position also puts the series in History.
    fn save_progress(&mut self, cx: &mut Context<Self>) {
        let ch = &self.chapters[self.chapter];
        let finished = !self.pages.is_empty() && self.page + 1 >= self.pages.len();
        let progress = Progress {
            chapter_url: ch["url"].as_str().unwrap_or_default().to_string(),
            chapter_name: ch["name"].as_str().unwrap_or("Chapter").to_string(),
            page: self.page,
            pages: self.pages.len(),
            at: now_ms(),
        };
        let (source, manga, chapters) = (self.source.clone(), self.manga.clone(), self.chapters.clone());
        self.store.update(cx, |store, cx| {
            store.update(&source.id, &source.name, &manga, cx, |s| {
                if s.chapters.is_empty() {
                    s.chapters = chapters;
                }
                if finished && !progress.chapter_url.is_empty() {
                    s.marks.insert(progress.chapter_url.clone(), true);
                }
                s.progress = Some(progress);
            })
        });
    }

    fn fetch_page(&mut self, i: usize, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let page = self.pages[i].clone();
        let key = self.key(i);
        let mode = self.mode;
        let (w, h) = match mode {
            Mode::Paged => (f32::from(self.viewport.width) * self.scale, f32::from(self.viewport.height) * self.scale),
            Mode::Vertical => (self.column_width() * self.scale, 0.),
        };
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let result: Result<Picture, String> = async {
                let cache_key = page.to_string();
                let k = cache_key.clone();
                let local = page["local"].as_str().map(PathBuf::from);
                let cached = cx.background_executor().spawn(async move { match local {
                    Some(file) => std::fs::read(file).ok(),
                    None => page_cache::get(&k),
                } }).await;
                let bytes = match cached {
                    Some(bytes) => bytes,
                    None => {
                        let path = source.call(cx, "download", json!({ "page": page })).await?;
                        let path = PathBuf::from(path.as_str().ok_or("no file from host")?);
                        let k = cache_key.clone();
                        cx.background_executor()
                            .spawn(async move {
                                let kept = page_cache::put(&k, path.clone());
                                let bytes = std::fs::read(&kept).map_err(|e| format!("{}: {e}", kept.display()));
                                if kept == path {
                                    let _ = std::fs::remove_file(&path); // not cached, so don't leave it behind
                                }
                                bytes
                            })
                            .await?
                    }
                };
                cx.background_executor()
                    .spawn(async move {
                        let result = match mode {
                            Mode::Paged => images::decode_bytes(bytes, images::fit(w as u32, h as u32)).await,
                            Mode::Vertical => images::decode_bytes(bytes, images::fit_width(w as u32)).await,
                        };
                        if result.is_err() {
                            page_cache::remove(&cache_key); // an error page, not an image
                        }
                        result
                    })
                    .await
            }
            .await;
            if std::env::var_os("NANO_DEMO").is_some() {
                eprintln!("loaded page {i}: {}", if result.is_ok() { "ok" } else { "failed" });
            }
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return; // the chapter or mode changed, so this page was never shown
                }
                this.inflight.remove(&i);
                let (behind, ahead) = this.mode.window();
                match result {
                    Ok(picture) if i + behind >= this.page && i <= this.page + ahead => {
                        let evicted = this.cache.update(cx, |c, _| c.insert(key, picture));
                        drop_images(cx, None, evicted);
                        if this.mode == Mode::Vertical {
                            this.strip.remeasure_items(i..i + 1);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        this.failed.insert(i);
                        if i == this.page {
                            this.status = format!("Couldn't load this page: {e}").into();
                        } else {
                            eprintln!("page {i}: {e}");
                        }
                    }
                }
                this.load_next(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn release_all(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let keys: Vec<String> = (0..self.pages.len()).map(|i| self.key(i)).collect();
        let released: Vec<_> =
            self.cache.update(cx, |c, _| keys.iter().filter_map(|k| c.remove(k)).flat_map(|p| p.to_vec()).collect());
        drop_images(cx, window, released);
    }

    fn scrub_to(&mut self, pos: Point<Pixels>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.pages.len();
        if n < 2 {
            return;
        }
        let t = match self.mode {
            Mode::Paged if self.rtl => f32::from(bounds.origin.x + bounds.size.width - pos.x) / f32::from(bounds.size.width),
            Mode::Paged => f32::from(pos.x - bounds.origin.x) / f32::from(bounds.size.width),
            Mode::Vertical => f32::from(pos.y - bounds.origin.y) / f32::from(bounds.size.height),
        };
        let i = scrub_index(t, n);
        if i == self.page {
            return;
        }
        match self.mode {
            Mode::Paged => {
                self.page = i;
                self.status = "Loading page…".into();
                self.sync(Some(window), cx);
            }
            Mode::Vertical => self.strip.scroll_to(ListOffset { item_ix: i, offset_in_item: px(0.) }),
        }
        cx.notify();
    }

    fn scrubber(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.pages.len();
        let vertical = self.mode == Mode::Vertical;
        let (vw, vh) = (f32::from(self.viewport.width), f32::from(self.viewport.height));
        let length = if vertical { vh - f32::from(crate::u(72. + 112.)) } else { (vw * 0.6).min(f32::from(crate::u(640.))) };
        let step = dot_step(length, n);
        let flip = self.rtl && !vertical;
        let at = |i: usize| relative(if flip { 1. - i as f32 / (n - 1) as f32 } else { i as f32 / (n - 1) as f32 });
        let dot = |i: usize, size: f32, color: u32| {
            let d = div().absolute().size(crate::u(size)).rounded_full().bg(rgb(color));
            let half = crate::u(-size / 2.);
            if vertical { d.top(at(i)).mt(half).left(relative(0.5)).ml(half) } else { d.left(at(i)).ml(half).top(relative(0.5)).mt(half) }
        };
        let rail = div().absolute().bg(rgb(LINE));
        let rail = if vertical { rail.top_0().bottom_0().left(relative(0.5)).w(px(2.)).ml(px(-1.)) } else { rail.left_0().right_0().top(relative(0.5)).h(px(2.)).mt(px(-1.)) };
        let dots = (0..n).filter(|i| i % step == 0 || *i == n - 1).map(|i| dot(i, 5., if i <= self.page { MUTED } else { LINE }));
        let label = self.scrubbing.then(|| {
            let l = div().absolute().font_family(crate::fonts::mono()).text_size(crate::u(12.)).text_color(rgb(TEXT)).bg(rgb(SURFACE)).px_1().rounded(crate::u(4.)).whitespace_nowrap().child(format!("{}", self.page + 1));
            if vertical { l.right(crate::u(30.)).top(at(self.page)).mt(crate::u(-9.)) } else { l.bottom(crate::u(22.)).left(at(self.page)).ml(crate::u(-12.)) }
        });

        // Listening on the whole window keeps a drag going when the pointer leaves the bar.
        let entity = cx.entity().downgrade();
        let hit = canvas(|_, _, _| (), move |bounds, _, window, _| {
            let e = entity.clone();
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && ev.button == MouseButton::Left && bounds.contains(&ev.position) {
                    cx.stop_propagation(); // not a tap on the page underneath
                    let _ = e.update(cx, |r, cx| {
                        r.scrubbing = true;
                        r.scrub_to(ev.position, bounds, window, cx);
                    });
                }
            });
            let e = entity.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = e.update(cx, |r, cx| {
                        if r.scrubbing && ev.pressed_button == Some(MouseButton::Left) {
                            r.scrub_to(ev.position, bounds, window, cx);
                        }
                    });
                }
            });
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && ev.button == MouseButton::Left {
                    let _ = entity.update(cx, |r, cx| {
                        if r.scrubbing {
                            r.scrubbing = false;
                            cx.stop_propagation();
                            r.show_chrome(Duration::from_secs(3), cx);
                        }
                    });
                }
            });
        })
        .absolute()
        .size_full();

        let track = div().relative().child(rail).children(dots).child(dot(self.page, 13., TEXT)).children(label).child(hit);
        let track = if vertical { track.w(crate::u(20.)).h_full() } else { track.h(crate::u(20.)).w(px(length)) };
        let pad = crate::u(10.);
        let hover = cx.listener(|this, hovered: &bool, _, cx| {
            if *hovered {
                this.extend_chrome(cx)
            }
        });
        let bar = div().id("scrubber").on_hover(hover).flex().p(pad).rounded(crate::u(20.)).bg(rgba(0x1A1816D0)).child(track);
        if vertical {
            bar.absolute().right(crate::u(16.)).top(crate::u(72.) - pad).bottom(crate::u(112.) - pad).into_any_element()
        } else {
            div().absolute().bottom(crate::u(84.)).left_0().right_0().flex().justify_center().child(bar).into_any_element()
        }
    }

    fn toggle_chapter_menu(&mut self, cx: &mut Context<Self>) {
        self.chapter_menu = !self.chapter_menu;
        if self.chapter_menu {
            self.chapter_scroll.scroll_to_item(self.chapter, ScrollStrategy::Center);
        } else {
            self.show_chrome(Duration::from_secs(3), cx);
        }
        cx.notify();
    }

    fn chapter_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.chapter;
        let list = uniform_list(
            "reader-chapters",
            self.chapters.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|i| {
                        let name = this.chapters[i]["name"].as_str().unwrap_or("Chapter").to_string();
                        let on = i == current;
                        div()
                            .id(("reader-ch", i))
                            .w_full()
                            .h(crate::u(36.))
                            .px(crate::u(14.))
                            .flex()
                            .items_center()
                            .rounded(crate::u(6.))
                            .cursor_pointer()
                            .text_size(crate::u(13.))
                            .when(on, |d| d.bg(rgb(HOVER)).text_color(rgb(TEXT)).font_weight(gpui::FontWeight::SEMIBOLD))
                            .when(!on, |d| d.text_color(rgb(MUTED)).hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT))))
                            .child(div().truncate().child(name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.chapter_menu = false;
                                this.open_chapter(i, 0, cx);
                                this.show_chrome(Duration::from_secs(3), cx);
                            }))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.chapter_scroll)
        .size_full();
        div()
            .absolute()
            .bottom(crate::u(84.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("chapter-menu")
                    .occlude()
                    .w(crate::u(420.))
                    .h(crate::u(380.).min(self.viewport.height * 0.6))
                    .p(crate::u(6.))
                    .rounded(crate::u(12.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(list),
            )
            .into_any_element()
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let screen = f32::from(self.viewport.height) * 0.85;
        match (self.mode, ev.keystroke.key.as_str()) {
            (_, "escape") if self.chapter_menu => self.toggle_chapter_menu(cx),
            (_, "escape") => cx.emit(ReaderEvent::Close),
            (_, "v") => self.toggle_mode(window, cx),
            (_, "f") => window.toggle_fullscreen(),
            (Mode::Paged, "right") => self.go(self.forward(), window, cx),
            (Mode::Paged, "left") => self.go(-self.forward(), window, cx),
            (Mode::Paged, "space" | "pagedown" | "down") => self.go(1, window, cx),
            (Mode::Paged, "pageup" | "up") => self.go(-1, window, cx),
            (Mode::Vertical, "left") => self.prev_chapter(cx),
            (Mode::Vertical, "right") => self.next_chapter(cx),
            (Mode::Vertical, "down") => self.glide_by(KEY_STEP, cx),
            (Mode::Vertical, "up") => self.glide_by(-KEY_STEP, cx),
            (Mode::Vertical, "space" | "pagedown") => self.glide_by(screen, cx),
            (Mode::Vertical, "pageup") => self.glide_by(-screen, cx),
            _ => {}
        }
    }

    fn strip_item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let col = self.column_width();
        if ix >= self.pages.len() {
            let label = if self.chapter > 0 { "Next chapter →" } else { "You're on the latest chapter" };
            let has_prev = self.chapter + 1 < self.chapters.len();
            return div()
                .w_full()
                .pt_12()
                .pb(crate::u(120.))
                .flex()
                .justify_center()
                .gap(crate::u(12.))
                .when(has_prev, |d| {
                    d.child(pill("prev-chapter", "← Previous chapter", false).on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation(); // a control, not a tap on the page
                        this.prev_chapter(cx)
                    })))
                })
                .child(
                    pill("next-chapter", label, self.chapter > 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation(); // a control, not a tap on the page
                            this.next_chapter(cx)
                        })),
                )
                .into_any_element();
        }
        let body = match self.cache.update(cx, |c, _| c.get(&self.key(ix))) {
            Some(picture) => picture_element(&picture, col).into_any_element(),
            None => div()
                .w(px(col))
                .h(px(col * 1.42))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(MUTED))
                .child(format!("Page {}", ix + 1))
                .into_any_element(),
        };
        div().w_full().flex().justify_center().child(body).into_any_element()
    }

    /// NANO_DEMO reads forward every 700 ms and logs the image cache, to check memory stays flat.
    fn run_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            for step in 0..60 {
                cx.background_executor().timer(std::time::Duration::from_millis(700)).await;
                let Ok(()) = this.update_in(cx, |this, window, cx| {
                    let mb = this.cache.read(cx).used_bytes() as f64 / 1_048_576.;
                    eprintln!(
                        "demo step {step:2}: {:?} chapter {} page {}/{}  cache {mb:.1} MB",
                        this.mode,
                        this.chapter,
                        this.page + 1,
                        this.pages.len()
                    );
                    match this.mode {
                        Mode::Paged => this.go(1, window, cx),
                        Mode::Vertical => {
                            this.strip.scroll_by(px(f32::from(this.viewport.height) * 0.85));
                            cx.notify();
                        }
                    }
                }) else {
                    return;
                };
            }
            eprintln!("demo done");
        })
        .detach();
    }
}

fn glide_step(remaining: f32, dt: f32) -> f32 {
    remaining * (1. - (-dt.min(0.05) / 0.09).exp())
}

fn scrub_index(t: f32, n: usize) -> usize {
    (t.clamp(0., 1.) * (n - 1) as f32).round() as usize
}

fn dot_step(length: f32, n: usize) -> usize {
    let gap = length / (n.max(2) - 1) as f32;
    [1, 2, 5, 10, 20, 50, 100].into_iter().find(|k| *k as f32 * gap >= 7.).unwrap_or(100)
}

fn fit_width((w, h): (u32, u32), viewport: Size<Pixels>) -> f32 {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    w as f32 * (vw / w.max(1) as f32).min(vh / h.max(1) as f32)
}

/// Sizes every tile explicitly. GPUI's own aspect ratio let tall pages run past the window.
fn picture_element(picture: &Picture, width: f32) -> impl IntoElement {
    div().flex().flex_col().children(picture.iter().map(|tile| {
        let s = tile.size(0);
        img(tile.clone()).w(px(width)).h(px(width * s.height.0 as f32 / s.width.0.max(1) as f32))
    }))
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport = crate::titlebar::viewport(window);
        self.scale = window.scale_factor();
        let len = self.pages.len();

        if self.glide != 0. {
            let now = Instant::now();
            let step = glide_step(self.glide, now.duration_since(self.glide_at).as_secs_f32());
            self.glide_at = now;
            self.strip.scroll_by(px(step));
            self.glide -= step;
            if self.glide.abs() < 0.5 {
                self.glide = 0.;
            } else {
                window.request_animation_frame();
            }
        }

        // Pages decode to fit the window, so redo them once fullscreen has its size
        if self.refit && window.is_fullscreen() && self.viewport != self.decoded_for {
            self.refit = false;
            self.decoded_for = self.viewport;
            if len > 0 {
                self.release_all(Some(window), cx);
                self.inflight.clear();
                self.generation += 1;
                self.sync(Some(window), cx);
            }
        }

        // In vertical mode the reading position is the page at the top.
        if self.mode == Mode::Vertical && len > 0 {
            let at_end = self.strip.bounds_for_item(len).is_some_and(|b| b.bottom() <= self.strip.viewport_bounds().bottom() + px(1.));
            let top = if at_end { len - 1 } else { self.strip.logical_scroll_top().item_ix.min(len - 1) };
            if top != self.page {
                self.page = top;
                self.sync(Some(window), cx);
            }
        }

        let body: AnyElement = match self.mode {
            Mode::Paged => {
                let picture = if len == 0 { None } else { self.cache.update(cx, |c, _| c.get(&self.key(self.page))) };
                match picture {
                    Some(picture) => {
                        let width = fit_width(picture_size(&picture), self.viewport);
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(picture_element(&picture, width))
                            .into_any_element()
                    }
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(MUTED))
                        .child(self.status.clone())
                        .into_any_element(),
                }
            }
            Mode::Vertical if len == 0 => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(MUTED))
                .child(self.status.clone())
                .into_any_element(),
            Mode::Vertical => {
                let this = cx.entity();
                let entity = this.downgrade();
                // GPUI scrolls 20 px per wheel notch, so notches glide and trackpads stay native
                let wheel = canvas(|_, _, _| (), move |bounds, _, window, _| {
                    let e = entity.clone();
                    window.on_mouse_event(move |ev: &ScrollEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture && !ev.delta.precise() && bounds.contains(&ev.position) && e.read_with(cx, |r, _| !r.chapter_menu).unwrap_or(false) {
                            cx.stop_propagation();
                            let dy = -f32::from(ev.delta.pixel_delta(px(WHEEL_STEP)).y);
                            let _ = e.update(cx, |r, cx| r.glide_by(dy, cx));
                        }
                    });
                })
                .absolute()
                .size_full();
                div()
                    .id("strip")
                    .size_full()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_chrome(cx)))
                    .child(list(self.strip.clone(), move |ix, _window, cx| this.update(cx, |this, cx| this.strip_item(ix, cx))).size_full())
                    .child(wheel)
                    .into_any_element()
            }
        };

        let chapter: SharedString = self.chapters[self.chapter]["name"].as_str().unwrap_or("Chapter").to_string().into();
        let position = if len == 0 { String::new() } else { format!("p.{} / {}", self.page + 1, len) };
        let keys = match self.mode {
            Mode::Paged => "← → turn · V mode · F fullscreen · esc close",
            Mode::Vertical => "↑ ↓ scroll · ← → chapter · V mode · F fullscreen · esc close",
        };

        let mut root = div()
            .id("reader")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(body);
        if self.mode == Mode::Paged {
            root = root
                .child(
                    div()
                        .id("middle")
                        .absolute()
                        .top_0()
                        .left(relative(1. / 3.))
                        .w(relative(1. / 3.))
                        .h_full()
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_chrome(cx))),
                )
                .child(
                    div()
                        .id("prev")
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(relative(1. / 3.))
                        .h_full()
                        .on_click(cx.listener(|this, _, window, cx| this.go(-this.forward(), window, cx))),
                )
                .child(
                    div()
                        .id("next")
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(relative(1. / 3.))
                        .h_full()
                        .on_click(cx.listener(|this, _, window, cx| this.go(this.forward(), window, cx))),
                );
        }
        let mode_button = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .h(crate::u(36.))
                .px(crate::u(14.))
                .rounded(crate::u(18.))
                .flex()
                .items_center()
                .text_size(crate::u(13.))
                .cursor_pointer()
                .when(on, |d| d.bg(rgb(TEXT)).text_color(rgb(BG)).font_weight(gpui::FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(rgb(MUTED)).hover(|s| s.bg(rgb(HOVER))))
                .child(label)
        };
        if !self.chrome && !self.scrubbing && !self.chapter_menu {
            return root;
        }
        let root = if self.chapter_menu {
            root.child(self.chapter_list(cx))
        } else {
            root.when(len > 1, |r| r.child(self.scrubber(cx)))
        };
        let (has_prev, has_next) = (self.chapter + 1 < self.chapters.len(), self.chapter > 0);
        let paged = self.mode == Mode::Paged;
        let corner = |id: &'static str| div().id(id).absolute().bottom(crate::u(28.)).flex().flex_col().gap_1().font_family(crate::fonts::mono()).text_size(crate::u(12.)).text_color(rgb(FAINT)).bg(rgba(0x0D0C0BB0)).px_2().py_1().rounded(crate::u(4.));
        let hover = |this: &mut Self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>| {
            if *hovered {
                this.extend_chrome(cx)
            }
        };
        root.child(corner("info").on_hover(cx.listener(hover)).left(crate::u(28.)).max_w(crate::u(320.)).child(div().text_color(rgb(TEXT)).child(position)).child(div().truncate().child(chapter)))
            .child(corner("keys").on_hover(cx.listener(hover)).right(crate::u(28.)).child(keys))
            .child(
                div().absolute().bottom(crate::u(24.)).left_0().right_0().flex().justify_center().child(
                    div()
                        .id("bar")
                        .on_hover(cx.listener(hover))
                        .flex()
                        .items_center()
                        .gap_1()
                        .p_1()
                        .rounded(crate::u(24.))
                        .bg(rgb(SURFACE))
                        .border_1()
                        .border_color(rgb(crate::LINE))
                        .child(mode_button("prev-ch", "‹", false).when(!has_prev, |d| d.opacity(0.3)).on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.prev_chapter(cx)
                        })))
                        .child(mode_button("chapters", "Chapters", self.chapter_menu).on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_chapter_menu(cx)
                        })))
                        .child(mode_button("next-ch", "›", false).when(!has_next, |d| d.opacity(0.3)).on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            if this.chapter > 0 {
                                this.next_chapter(cx)
                            }
                        })))
                        .child(div().w(px(1.)).h(crate::u(20.)).mx(crate::u(4.)).bg(rgb(LINE)))
                        .child(mode_button("ltr", "Left to right", paged && !self.rtl).on_click(cx.listener(|this, _, window, cx| this.set_direction(false, window, cx))))
                        .child(mode_button("rtl", "Right to left", paged && self.rtl).on_click(cx.listener(|this, _, window, cx| this.set_direction(true, window, cx))))
                        .child(mode_button("vertical", "Vertical", !paged).on_click(cx.listener(
                            |this, _, window, cx| {
                                if this.mode != Mode::Vertical {
                                    this.toggle_mode(window, cx)
                                }
                            },
                        )))
                        .child(mode_button("close", "Close", false).on_click(cx.listener(|_, _, _, cx| cx.emit(ReaderEvent::Close)))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{px, size};

    #[test]
    fn glide_eats_a_share_and_settles() {
        assert_eq!(super::glide_step(100., 0.), 0.);
        let (mut left, mut frames) = (-100f32, 0);
        while left.abs() >= 0.5 {
            left -= super::glide_step(left, 1. / 60.);
            frames += 1;
        }
        assert!(left <= 0. && frames < 60);
    }

    #[test]
    fn scrubber_maps_and_spaces_pages() {
        assert_eq!(super::scrub_index(-1., 10), 0);
        assert_eq!(super::scrub_index(0.5, 11), 5);
        assert_eq!(super::scrub_index(2., 10), 9);
        assert_eq!(super::dot_step(600., 30), 1);
        assert_eq!(super::dot_step(600., 200), 5);
    }

    #[test]
    fn pages_fit_inside_the_window() {
        let window = size(px(1280.), px(800.));
        let w = super::fit_width((1000, 3000), window);
        assert!((w - 266.666).abs() < 0.01 && w * 3. <= 800.01);
        assert_eq!(super::fit_width((3000, 1000), window), 1280.);
        assert!((super::fit_width((400, 600), window) * 1.5 - 800.).abs() < 0.01);
    }
}
