mod browse;
mod catalog;
mod challenge;
mod cache_db;
mod covers;
mod downloads;
mod ext_settings;
mod extensions;
mod filters;
mod fonts;
mod global;
mod history;
mod host;
mod images;
mod input;
mod library;
mod manga;
mod migrate;
mod page_cache;
mod palette;
mod reader;
mod registry;
mod rich;
mod saved;
mod segmented;
mod settings;
mod sidebar;
mod solver;
mod store;
mod sync;
mod titlebar;
mod track;
mod tracking;
mod update;
mod webviews;

use browse::{Browse, OpenManga};
use catalog::Catalog;
use history::History;
use library::{GoBrowse, Library, Resume};
use registry::Registry;
use store::Store;
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, AsyncApp, Bounds, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled, Subscription,
    Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px, rgb, size,
};
use host::{Host, HostError};
use images::ImageCache;
use manga::{MangaEvent, MangaView};
use reader::{Reader, ReaderEvent};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// mimalloc hands freed decode buffers back to macOS. The system allocator sat on 341 MB.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub const BG: u32 = 0x0D0C0B;
pub const SURFACE: u32 = 0x1A1816;
pub const HOVER: u32 = 0x24211D;
pub const BORDER: u32 = 0x24211D;
pub const RULE: u32 = 0x1E1B18;
pub const LINE: u32 = 0x3A352F;
pub const TEXT: u32 = 0xEDE6D8;
pub const BODY: u32 = 0xC9C1B3;
pub const MUTED: u32 = 0xA39C90;
pub const FAINT: u32 = 0x857E73;
pub const ACCENT: u32 = 0xFF4A2B;
pub const ACCENT_TEXT: u32 = 0xFF6A4D;

/// UI zoom. Every UI size goes through `u`. Reader pages use real pixels and don't zoom.
static ZOOM: AtomicU32 = AtomicU32::new(1f32.to_bits());

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

fn set_zoom(z: f32) {
    ZOOM.store(((z * 10.).round() / 10.).clamp(0.7, 2.0).to_bits(), Ordering::Relaxed);
}

pub fn u(v: f32) -> gpui::Pixels {
    px(v * zoom())
}

static SIDEBAR: AtomicU32 = AtomicU32::new(sidebar::WIDE.to_bits());

fn set_sidebar_width(w: f32) {
    SIDEBAR.store(w.to_bits(), Ordering::Relaxed);
}

/// The series panel's width right now, which changes while it slides.
static PANEL: AtomicU32 = AtomicU32::new(0);

/// The window's width minus the sidebar and the series panel, as they are this frame, for sizing grids.
pub fn content_width(window: &Window) -> f32 {
    let taken = f32::from_bits(SIDEBAR.load(Ordering::Relaxed)) + f32::from_bits(PANEL.load(Ordering::Relaxed));
    f32::from(titlebar::viewport(window).width) - taken * zoom()
}

/// How long the sidebar and series panel take to slide.
pub const SLIDE: std::time::Duration = std::time::Duration::from_millis(240);

struct Assets;

const ICONS: [(&str, &[u8]); 11] = [
    ("icons/search.svg", include_bytes!("../assets/icons/search.svg")),
    ("icons/chevron.svg", include_bytes!("../assets/icons/chevron.svg")),
    ("icons/bookmark.svg", include_bytes!("../assets/icons/bookmark.svg")),
    ("icons/downloaded.svg", include_bytes!("../assets/icons/downloaded.svg")),
    ("icons/check.svg", include_bytes!("../assets/icons/check.svg")),
    ("icons/sidebar.svg", include_bytes!("../assets/icons/sidebar.svg")),
    ("icons/download.svg", include_bytes!("../assets/icons/download.svg")),
    ("icons/win-min.svg", include_bytes!("../assets/icons/win-min.svg")),
    ("icons/win-max.svg", include_bytes!("../assets/icons/win-max.svg")),
    ("icons/win-restore.svg", include_bytes!("../assets/icons/win-restore.svg")),
    ("icons/win-close.svg", include_bytes!("../assets/icons/win-close.svg")),
];

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, b)| (*b).into()))
    }
    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

pub fn search_icon() -> impl IntoElement {
    gpui::svg().path("icons/search.svg").flex_none().size(u(16.)).text_color(rgb(FAINT))
}

/// The ✓ glyph falls back to a font where it looks like √, so draw it.
pub fn check(size: f32, color: u32) -> impl IntoElement {
    gpui::svg().path("icons/check.svg").flex_none().size(u(size)).text_color(rgb(color))
}

/// System font for the few Japanese labels, so no CJK font is bundled.
pub const CJK: &str = "Hiragino Sans";

fn load_fonts(cx: &App) {
    let fonts = [
        &include_bytes!("../fonts/BigShouldersDisplay-ExtraBold.ttf")[..],
        include_bytes!("../fonts/BigShouldersDisplay-Black.ttf"),
        include_bytes!("../fonts/IBMPlexSans-Regular.ttf"),
        include_bytes!("../fonts/IBMPlexSans-Medium.ttf"),
        include_bytes!("../fonts/IBMPlexSans-SemiBold.ttf"),
        include_bytes!("../fonts/IBMPlexMono-Regular.ttf"),
        include_bytes!("../fonts/IBMPlexMono-Medium.ttf"),
    ];
    if let Err(e) = cx.text_system().add_fonts(fonts.into_iter().map(std::borrow::Cow::Borrowed).collect()) {
        eprintln!("fonts: {e}");
    }
}

pub fn heading(title: &str, note: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex()
        .items_end()
        .gap_4()
        .min_w_0()
        .child(div().flex_none().font_family(crate::fonts::display()).font_weight(FontWeight::EXTRA_BOLD).text_size(u(28.)).child(title.to_uppercase()))
        .child(div().min_w_0().truncate().pb(u(5.)).font_family(crate::fonts::mono()).text_size(u(13.)).text_color(rgb(FAINT)).child(note.into()))
}

pub fn pill(id: &'static str, label: impl Into<SharedString>, primary: bool) -> gpui::Stateful<gpui::Div> {
    let d = div()
        .id(id)
        .h(u(44.))
        .px(u(20.))
        .rounded(u(22.))
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .child(label.into());
    if primary {
        d.bg(rgb(ACCENT)).text_color(rgb(BG)).font_weight(FontWeight::SEMIBOLD).hover(|s| s.bg(rgb(ACCENT_TEXT)))
    } else {
        d.border_1().border_color(rgb(LINE)).hover(|s| s.bg(rgb(HOVER)))
    }
}

pub(crate) struct Tip(pub SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(u(8.))
            .py(u(4.))
            .rounded(u(6.))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE))
            .text_size(u(12.))
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

/// Hover-revealed button that clears a series' reading progress from History and Continue reading.
pub(crate) fn remove_history_button(
    id: impl Into<gpui::ElementId>,
    group: &'static str,
    overlay: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group("rm-btn")
        .flex_none()
        .size(u(24.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(overlay, |d| d.absolute().top(u(6.)).right(u(6.)))
        .invisible()
        .group_hover(group, |s| s.visible())
        .tooltip(|_, cx| cx.new(|_| Tip("Remove from history".into())).into())
        .child(
            gpui::svg()
                .path("icons/win-close.svg")
                .flex_none()
                .size(u(12.))
                .text_color(rgb(MUTED))
                .group_hover("rm-btn", |s| s.size(u(15.)).text_color(rgb(TEXT))),
        )
}

/// Shared banner offering Undo after removing items from History or Continue reading.
pub(crate) fn history_undo_banner(
    id_prefix: &'static str,
    store: Entity<Store>,
    count: usize,
) -> gpui::AnyElement {
    let message = if count == 1 {
        "Removed 1 series from history.".into()
    } else {
        format!("Cleared {count} series from history.")
    };
    let undo_store = store.clone();
    div()
        .px(u(14.))
        .py(u(10.))
        .rounded(u(10.))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(SURFACE))
        .flex()
        .items_center()
        .gap(u(12.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(u(13.))
                .text_color(rgb(BODY))
                .child(message),
        )
        .child(
            catalog::small_button(format!("{id_prefix}-undo"), "Undo", true).on_click(
                move |_, _, cx| {
                    undo_store.update(cx, |s, cx| s.undo_history(cx));
                },
            ),
        )
        .child(
            div()
                .id(format!("{id_prefix}-dismiss"))
                .flex_none()
                .size(u(24.))
                .rounded(u(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(MUTED))
                .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
                .child(gpui::svg().path("icons/win-close.svg").size(u(12.)))
                .on_click(move |_, _, cx| {
                    store.update(cx, |s, cx| s.dismiss_history_undo(cx));
                }),
        )
        .into_any_element()
}

#[derive(Clone)]
pub struct Source {
    pub host: Arc<Host>,
    pub id: String,
    pub name: String,
    /// Empty when the extension was removed.
    pub lang: String,
    pub base_url: String,
    /// False for a saved series whose extension was removed.
    pub installed: bool,
}

impl Source {
    pub async fn call(&self, cx: &mut AsyncApp, op: &str, mut params: Value) -> Result<Value, String> {
        params["source"] = self.id.clone().into();
        call(&self.host, cx, op, params).await
    }
}

/// One Cloudflare solve at a time. Requests that hit the same check wait and reuse its cookie.
// One lock for all sites. Per-host locks if two sites often challenge at once.
static SOLVING: async_lock::Mutex<()> = async_lock::Mutex::new(());

/// Calls the host, and on a Cloudflare check solves it (with the user's solver if one is set) and retries once.
pub async fn call(host: &Arc<Host>, cx: &mut AsyncApp, op: &str, params: Value) -> Result<Value, String> {
    call_raw(host, cx, op, params).await.map_err(|e| readable(&e))
}

/// Shown when a site wants a sign-in or its own check. Browse offers to open the site on this text.
pub const NEEDS_SITE: &str = "The site wants you signed in or checked first. Open it, finish there, then try again.";

/// Host errors arrive as Java exceptions with a stack trace. Keep the message.
fn readable(e: &str) -> String {
    let line = e.lines().next().unwrap_or("").trim();
    if line.contains("HTTP error 401") || line.contains("Open in WebView") {
        return NEEDS_SITE.into();
    }
    if line.contains("HTTP error 429") {
        return "The site is limiting requests right now. Wait a minute, then try again.".into();
    }
    match line.split_once(": ") {
        Some((class, msg)) if class.ends_with("Exception") && !class.contains(' ') => msg.into(),
        _ => line.into(),
    }
}

async fn call_raw(host: &Arc<Host>, cx: &mut AsyncApp, op: &str, params: Value) -> Result<Value, String> {
    let mut first = host.call(op, params.clone()).await;
    // Opening a series while the Library checks it would ask twice at once, which extensions refuse. Wait instead.
    for _ in 0..60 {
        match &first {
            Err(e) if e.to_string().contains("must not be called concurrently") => {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                first = host.call(op, params.clone()).await;
            }
            _ => break,
        }
    }
    match first {
        Err(HostError::Challenge { .. }) => {
            let _solving = SOLVING.lock().await;
            // Another request may have solved it while we waited.
            let (url, user_agent) = match host.call(op, params.clone()).await {
                Err(HostError::Challenge { url, user_agent }) => (url, user_agent),
                other => return other.map_err(|e| e.to_string()),
            };
            let attempt = match solver::url() {
                Some(base) => {
                    let url = url.clone();
                    cx.background_executor().spawn(async move { solver::running(&base).then(|| solver::solve(&base, &url)) }).await
                }
                None => None,
            };
            match attempt {
                Some(Ok(solved)) => {
                    let _ = host.set_cookies(&url, solved.cookies).await;
                    let _ = host.call("setUserAgent", serde_json::json!({ "url": url, "userAgent": solved.user_agent })).await;
                    match host.call(op, params.clone()).await {
                        Err(HostError::Challenge { .. }) => eprintln!("The solver's cookies didn't pass {url}, trying the webview"),
                        other => return other.map_err(|e| e.to_string()),
                    }
                }
                Some(Err(e)) => eprintln!("{e}, trying the webview"),
                None => {}
            }
            let solved = cx.update(|cx| challenge::solve(cx, host.clone(), url.clone(), challenge::browser_user_agent(user_agent)));
            if solved.await.unwrap_or(false) {
                match host.call(op, params.clone()).await {
                    Err(HostError::Challenge { .. }) => {
                        let _ = host.call("browserOnly", serde_json::json!({ "url": url })).await;
                        host.call(op, params).await.map_err(|e| e.to_string())
                    }
                    other => other.map_err(|e| e.to_string()),
                }
            } else {
                Err("Verification was cancelled".into())
            }
        }
        other => other.map_err(|e| e.to_string()),
    }
}

/// NANO_DEMO=<n> opens the nth popular series and pages through it (memory check).
#[cfg(test)]
mod tests {
    #[test]
    fn errors_read_like_sentences() {
        assert_eq!(super::readable("eu.kanade.tachiyomi.network.HttpException: HTTP error 503\n\tat x"), "HTTP error 503");
        assert!(super::readable("eu.kanade.tachiyomi.network.HttpException: HTTP error 429").starts_with("The site is limiting"));
        assert_eq!(super::readable("Verification was cancelled"), "Verification was cancelled");
    }
}

/// FNV-1a, for file names that must stay the same across Rust versions (DefaultHasher can change).
pub fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn demo() -> Option<usize> {
    std::env::var("NANO_DEMO").ok().map(|v| v.parse().unwrap_or(0))
}

/// NANO_DEMO_MODE picks a screen state for testing, empty when unset.
pub fn demo_mode() -> String {
    std::env::var("NANO_DEMO_MODE").unwrap_or_default()
}

/// Frees the GPU copies. Pass the window if you're inside one, since GPUI can't reach it while it's borrowed.
pub fn drop_images(cx: &mut App, mut window: Option<&mut Window>, images: Vec<Arc<RenderImage>>) {
    for image in images {
        cx.drop_image(image, window.as_deref_mut());
    }
}

gpui::actions!(yomi, [Quit, ShowLibrary, ShowBrowse, ShowExtensions, ShowHistory, ShowSettings, Search, ToggleSidebar, ZoomIn, ZoomOut, ZoomReset]);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Library,
    Updates,
    Search,
    Downloads,
    Saved,
    Browse,
    Extensions,
    History,
    Settings,
}

struct Root {
    cache: Entity<ImageCache>,
    store: Entity<Store>,
    registry: Entity<Registry>,
    tab: Tab,
    library: Entity<Library>,
    browse: Entity<Browse>,
    global: Entity<global::GlobalSearch>,
    catalog: Option<Entity<Catalog>>,
    history: Option<Entity<History>>,
    saved: Option<Entity<saved::Saved>>,
    settings: Option<Entity<settings::Settings>>,
    manga: Option<(Entity<MangaView>, Subscription)>,
    migrate: Option<(Entity<migrate::Migrate>, Subscription)>,
    reader: Option<(Entity<Reader>, Subscription)>,
    palette: Option<palette::Palette>,
    /// A site check or sign-in shown over the window.
    check: Option<Entity<challenge::Check>>,
    check_sub: Option<Subscription>,
    banner: Entity<update::Banner>,
    /// The series panel that's sliding shut, kept until it's out of sight.
    closing: Option<Entity<MangaView>>,
    /// Bumped each time the panel opens or closes, or the sidebar folds, so their slides start over.
    panel_moves: usize,
    sidebar_moves: usize,
    /// Extension icons shown in the sidebar.
    icons: covers::Covers,
    focus: gpui::FocusHandle,
    _subs: Vec<Subscription>,
}

impl Root {
    fn new(host: Arc<Host>, data_dir: PathBuf, store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cache = cx.new(|_| ImageCache::new(images::BUDGET_BYTES));
        let store = cx.new(|_| store);
        sync::run(store.clone(), cx);
        set_zoom(store.read(cx).zoom());
        downloads::apply(&store.read(cx).prefs().download_dir, store.read(cx).prefs().download_format);
        page_cache::set_cap_mb(store.read(cx).page_cache_mb());
        solver::set_url(store.read(cx).solver_url());
        set_sidebar_width(if store.read(cx).prefs().sidebar_collapsed { sidebar::NARROW } else { sidebar::WIDE });
        let registry = cx.new(|cx| Registry::new(host, data_dir.clone(), cx));
        let repos = store.read(cx).repos().to_vec();
        registry.update(cx, |r, cx| r.load_cached_index(repos, cx));
        let library = cx.new(|cx| Library::new(store.clone(), registry.clone(), cache.clone(), cx));
        let browse = cx.new(|cx| Browse::new(registry.clone(), store.clone(), cache.clone(), cx));
        let global = cx.new(|cx| global::GlobalSearch::new(registry.clone(), store.clone(), cache.clone(), cx));
        let subs = vec![
            cx.observe(&registry, |this, _, cx| this.sync_nsfw(cx)),
            cx.subscribe_in(&browse, window, |this, _, ev: &OpenManga, window, cx| this.open_manga(ev, window, cx)),
            cx.subscribe_in(&global, window, |this, _, ev: &OpenManga, window, cx| this.open_manga(ev, window, cx)),
            cx.subscribe_in(&library, window, |this, _, ev: &OpenManga, window, cx| this.open_manga(ev, window, cx)),
            cx.subscribe_in(&library, window, |this, _, ev: &Resume, window, cx| this.resume(ev, window, cx)),
            cx.subscribe_in(&library, window, |this, _, _: &GoBrowse, window, cx| this.show(Tab::Browse, window, cx)),
        ];
        let tab = if demo().is_some() { Tab::Browse } else { Tab::Library };
        // NANO_TAB=library|browse|extensions|history|settings|search starts on that tab (for testing).
        let tab = match std::env::var("NANO_TAB").as_deref() {
            Ok("library") => Tab::Library,
            Ok("browse") => Tab::Browse,
            Ok("extensions") => Tab::Extensions,
            Ok("history") => Tab::History,
            Ok("settings") => Tab::Settings,
            Ok("search") => Tab::Search,
            _ => tab,
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let banner = cx.new(|cx| update::Banner::new(store.clone(), cx));
        cx.spawn({
            let store = store.clone();
            async move |_, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
                cx.update(|cx| update::check(store, false, cx));
            }
        })
        .detach();
        let settings = (tab == Tab::Settings).then(|| {
            let store = store.clone();
            cx.new(|cx| settings::Settings::new(store, cx))
        });
        let catalog = (tab == Tab::Extensions).then(|| {
            let (registry, store, cache) = (registry.clone(), store.clone(), cache.clone());
            cx.new(|cx| Catalog::new(registry, store, cache, cx))
        });
        let history = (tab == Tab::History).then(|| {
            let (store, registry, cache) = (store.clone(), registry.clone(), cache.clone());
            cx.new(|cx| History::new(store, registry, cache, cx))
        });
        Root { cache, store, registry, tab, library, browse, global, catalog, history, saved: None, settings, manga: None, migrate: None, reader: None, palette: None, check: None, check_sub: None, banner, closing: None, panel_moves: 0, sidebar_moves: 0, icons: Default::default(), focus, _subs: subs }
    }

    /// Remembers which sources the repos flag as 18+, or as mixing 18+ in, so they can be hidden.
    fn sync_nsfw(&mut self, cx: &mut Context<Self>) {
        let (nsfw, mixed): (Vec<String>, Vec<String>) = {
            let Some(index) = self.registry.read(cx).index.clone() else { return };
            // Only sources in use: installed ones, and ones a saved series came from.
            let mut used: std::collections::HashSet<String> = self.store.read(cx).all_series().iter().map(|s| s.source.clone()).collect();
            used.extend(self.registry.read(cx).sources.iter().map(|s| s.id.clone()));
            let ids = |flag: fn(&extensions::Ext) -> bool| -> Vec<String> {
                index.iter().filter(|e| flag(e)).flat_map(|e| e.source_ids.iter()).filter(|id| used.contains(*id)).cloned().collect()
            };
            (ids(|e| e.nsfw), ids(|e| e.mixed))
        };
        let p = self.store.read(cx).prefs();
        if nsfw.iter().all(|id| p.nsfw_sources.contains(id)) && mixed.iter().all(|id| p.mixed_sources.contains(id)) {
            return;
        }
        self.store.update(cx, |s, cx| {
            s.edit_prefs(cx, |p| {
                for (known, new) in [(&mut p.nsfw_sources, nsfw), (&mut p.mixed_sources, mixed)] {
                    for id in new {
                        if !known.contains(&id) {
                            known.push(id);
                        }
                    }
                }
            })
        });
    }

    fn zoom_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        set_zoom(zoom() + delta);
        let z = zoom();
        self.store.update(cx, |s, cx| s.set_zoom(z, cx));
        cx.refresh_windows();
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.library.update(cx, |l, cx| {
            l.updates_page = tab == Tab::Updates;
            cx.notify();
        });
        self.close_manga(cx);
        self.migrate = None;
        match tab {
            Tab::Extensions if self.catalog.is_none() => {
                let registry = self.registry.clone();
                let (store, cache) = (self.store.clone(), self.cache.clone());
                self.catalog = Some(cx.new(|cx| Catalog::new(registry, store, cache, cx)));
            }
            Tab::History if self.history.is_none() => {
                let (store, registry, cache) = (self.store.clone(), self.registry.clone(), self.cache.clone());
                let history = cx.new(|cx| History::new(store, registry, cache, cx));
                self._subs.push(cx.subscribe_in(&history, window, |this, _, ev: &Resume, window, cx| this.resume(ev, window, cx)));
                self._subs.push(cx.subscribe_in(&history, window, |this, _, ev: &OpenManga, window, cx| this.open_manga(ev, window, cx)));
                self.history = Some(history);
            }
            Tab::Saved => {
                if let Some(saved) = &self.saved {
                    saved.update(cx, |s, cx| s.rescan(cx));
                } else {
                    let (store, registry, cache) = (self.store.clone(), self.registry.clone(), self.cache.clone());
                    let saved = cx.new(|cx| saved::Saved::new(store, registry, cache, cx));
                    self._subs.push(cx.subscribe_in(&saved, window, |this, _, ev: &saved::OpenSaved, window, cx| this.open_saved(ev, window, cx)));
                    self.saved = Some(saved);
                }
            }
            Tab::Settings if self.settings.is_none() => {
                let store = self.store.clone();
                self.settings = Some(cx.new(|cx| settings::Settings::new(store, cx)));
            }
            _ => {}
        }
        cx.notify();
    }

    fn open_manga(&mut self, ev: &OpenManga, window: &mut Window, cx: &mut Context<Self>) {
        let (source, manga, cache, store) = (ev.source.clone(), ev.manga.clone(), self.cache.clone(), self.store.clone());
        let view = cx.new(|cx| MangaView::new(source, cache, store, manga, cx));
        self.attach_manga(view, window, cx);
    }

    fn open_saved(&mut self, ev: &saved::OpenSaved, window: &mut Window, cx: &mut Context<Self>) {
        let (source, manga, chapters, cache, store) = (ev.source.clone(), ev.manga.clone(), ev.chapters.clone(), self.cache.clone(), self.store.clone());
        let view = cx.new(|cx| MangaView::local(source, cache, store, manga, chapters, cx));
        self.attach_manga(view, window, cx);
    }

    fn attach_manga(&mut self, view: Entity<MangaView>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(&view, window, |this, view, ev: &MangaEvent, window, cx| match ev {
            MangaEvent::Back => this.close_manga(cx),
            MangaEvent::Read { chapters, index, page } => {
                let (source, manga) = { let v = view.read(cx); (v.source.clone(), v.manga.clone()) };
                this.open_reader(source, manga, chapters.clone(), *index, *page, window, cx);
            }
            MangaEvent::Migrate => {
                let saved = { let v = view.read(cx); this.store.read(cx).get(&v.source.id, v.manga["url"].as_str().unwrap_or_default()).cloned() };
                if let Some(series) = saved {
                    this.open_migrate(series, window, cx);
                }
            }
        });
        if self.manga.is_none() {
            self.panel_moves += 1;
        }
        self.closing = None;
        self.manga = Some((view, sub));
        cx.notify();
    }

    fn close_manga(&mut self, cx: &mut Context<Self>) {
        let Some((view, _)) = self.manga.take() else { return };
        if let Some(saved) = &self.saved {
            saved.update(cx, |s, cx| s.rescan(cx));
        }
        self.closing = Some(view);
        self.panel_moves += 1;
        let moves = self.panel_moves;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SLIDE).await;
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

    fn open_migrate(&mut self, series: store::Series, window: &mut Window, cx: &mut Context<Self>) {
        let (registry, store, cache) = (self.registry.clone(), self.store.clone(), self.cache.clone());
        let view = cx.new(|cx| migrate::Migrate::new(series, &registry, store, cache, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &migrate::MigrateEvent, window, cx| {
            this.migrate = None;
            if let migrate::MigrateEvent::Done { source, manga } = ev {
                this.open_manga(&OpenManga { source: source.clone(), manga: manga.clone() }, window, cx);
            }
            cx.notify();
        });
        self.migrate = Some((view, sub));
        cx.notify();
    }

    fn resume(&mut self, ev: &Resume, window: &mut Window, cx: &mut Context<Self>) {
        self.open_reader(ev.source.clone(), ev.manga.clone(), ev.chapters.clone(), ev.index, ev.page, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn open_reader(
        &mut self,
        source: Source,
        manga: Value,
        chapters: Arc<Vec<Value>>,
        index: usize,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (cache, store) = (self.cache.clone(), self.store.clone());
        let reader = cx.new(|cx| Reader::new(source, cache, store, manga, chapters, index, page, window, cx));
        let sub = cx.subscribe_in(&reader, window, |this, reader, _: &ReaderEvent, window, cx| {
            reader.update(cx, |r, cx| {
                r.release_all(Some(window), cx);
                r.leave_fullscreen(window);
            });
            this.reader = None;
            sync::run(this.store.clone(), cx);
            this.focus.focus(window, cx);
            cx.notify();
        });
        self.reader = Some((reader, sub));
        cx.notify();
    }

}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page(window, cx);
        titlebar::wrap(page, window)
    }
}

impl Root {
    fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        window.set_rem_size(u(16.));
        let root = div().relative().size_full().flex().bg(rgb(BG)).text_color(rgb(TEXT)).font_family(fonts::sans()).text_size(u(14.));
        if let Some((reader, _)) = &self.reader {
            return root.child(reader.clone()).children(self.check.clone());
        }
        let screen: AnyElement = match self.tab {
            _ if self.migrate.is_some() => self.migrate.as_ref().unwrap().0.clone().into_any_element(),
            Tab::Library | Tab::Updates => self.library.clone().into_any_element(),
            Tab::Browse => self.browse.clone().into_any_element(),
            Tab::Search => self.global.clone().into_any_element(),
            Tab::Downloads => downloads::get(cx).into_any_element(),
            Tab::Saved => self.saved.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
            Tab::Extensions => self.catalog.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
            Tab::History => self.history.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
            Tab::Settings => self.settings.clone().map(IntoElement::into_any_element).unwrap_or_else(|| div().into_any_element()),
        };
        // The series slides open beside the grid, so you keep your place in it.
        let open = self.manga.is_some();
        let view = self.manga.as_ref().map(|(m, _)| m.clone()).or_else(|| self.closing.clone()).filter(|_| self.migrate.is_none());
        let panel = view.map(|view| {
            let (from, to) = if open { (0., manga::PANEL) } else { (manga::PANEL, 0.) };
            div()
                .flex_none()
                .relative()
                .h_full()
                .overflow_hidden()
                .border_l_1()
                .border_color(rgb(RULE))
                .bg(rgb(0x11100E))
                // A fixed width pinned to the right, so it slides in whole instead of rewrapping.
                .child(div().absolute().top_0().bottom_0().right_0().w(u(manga::PANEL)).child(view))
                .with_animation(("panel", self.panel_moves), Animation::new(SLIDE).with_easing(gpui::ease_out_quint()), move |d, t| {
                    let w = from + (to - from) * t;
                    PANEL.store(w.to_bits(), Ordering::Relaxed);
                    d.w(u(w))
                })
        });
        if panel.is_none() {
            PANEL.store(0f32.to_bits(), Ordering::Relaxed);
        }
        root.track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ShowLibrary, window, cx| this.show(Tab::Library, window, cx)))
            .on_action(cx.listener(|this, _: &ShowBrowse, window, cx| this.show(Tab::Browse, window, cx)))
            .on_action(cx.listener(|this, _: &ShowExtensions, window, cx| this.show(Tab::Extensions, window, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, window, cx| this.show(Tab::History, window, cx)))
            .on_action(cx.listener(|this, _: &ShowSettings, window, cx| this.show(Tab::Settings, window, cx)))
            .on_action(cx.listener(|this, _: &Search, window, cx| this.open_palette(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by(0.1, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by(-0.1, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom_by(1. - zoom(), cx)))
            .child(self.sidebar(cx))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_manga(cx);
                }
            }))
            .child(div().flex_1().min_w_0().h_full().child(screen))
            .children(panel)
            .children(self.palette(cx))
            .children(self.check.clone())
            .children(self.check.as_ref().is_none_or(|c| !c.read(cx).is_open()).then(|| self.banner.clone()))
    }
}

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NANO_DATA") {
        return dir.into();
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let (dir, old) = if cfg!(target_os = "macos") {
        (home.join("Library/Application Support/Nano"), home.join("Library/Application Support/Yomi"))
    } else if cfg!(windows) {
        let appdata = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default());
        (appdata.join("Nano"), appdata.join("Yomi"))
    } else {
        (home.join(".local/share/nano"), home.join(".local/share/yomi"))
    };
    // The app used to be called Yomi. Move its library over once.
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

fn main() {
    let data_dir = data_dir();
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    covers::set_cache_dir(data_dir.join("covers"));
    cache_db::open(&data_dir.join("cache.db"));
    page_cache::set_dir(data_dir.join("pages"));
    downloads::set_dir(data_dir.join("downloads"));
    // Kagane serves JPEG XL, which `image` only decodes through this hook.
    jxl_oxide::integration::register_image_decoding_hook();
    fonts::set_dir(data_dir.join("fonts"));
    let first = Store::load(data_dir.join("library.json"));
    fonts::choose(&first.prefs().fonts);
    let heap_mb = first.prefs().host_heap_mb.unwrap_or_else(host::auto_heap_mb);
    let _ = host::LAUNCH_HEAP_MB.set(heap_mb);
    let host = Host::spawn(&host::host_bin(), &data_dir.join("host"), heap_mb)
        .unwrap_or_else(|e| panic!("start extension host {}: {e}", host::host_bin().display()));

    gpui_platform::application().with_assets(Assets).run(move |cx: &mut App| {
        load_fonts(cx);
        fonts::load(cx);
        downloads::init(cx);
        webviews::start(cx, host.clone());
        cx.bind_keys([
            gpui::KeyBinding::new("cmd-1", ShowLibrary, None),
            gpui::KeyBinding::new("cmd-2", ShowBrowse, None),
            gpui::KeyBinding::new("cmd-3", ShowExtensions, None),
            gpui::KeyBinding::new("cmd-4", ShowHistory, None),
            gpui::KeyBinding::new("cmd-5", ShowSettings, None),
            gpui::KeyBinding::new("cmd-,", ShowSettings, None),
            gpui::KeyBinding::new(if cfg!(target_os = "macos") { "cmd-k" } else { "ctrl-k" }, Search, None),
            gpui::KeyBinding::new(if cfg!(target_os = "macos") { "cmd-\\" } else { "ctrl-\\" }, ToggleSidebar, None),
            gpui::KeyBinding::new("cmd-=", ZoomIn, None),
            gpui::KeyBinding::new("cmd-+", ZoomIn, None),
            gpui::KeyBinding::new("cmd--", ZoomOut, None),
            gpui::KeyBinding::new("cmd-0", ZoomReset, None),
            gpui::KeyBinding::new(if cfg!(target_os = "macos") { "cmd-q" } else { "ctrl-q" }, Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(vec![gpui::Menu { name: "Nano".into(), items: vec![gpui::MenuItem::action("Quit Nano", Quit)], disabled: false }]);
        cx.on_window_closed(|cx, _| {
            // The hidden WebView window shouldn't keep the app open.
            if cx.windows().iter().all(|w| webviews::is_webview_window(cx, w.window_id())) {
                cx.quit();
            }
        })
        .detach();

        // NANO_CHALLENGE_TEST=<url> opens the challenge window for a manual check.
        if let Ok(url) = std::env::var("NANO_CHALLENGE_TEST") {
            let host = host.clone();
            cx.spawn(async move |cx| {
                let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
                let solved = cx.update(|cx| challenge::solve(cx, host, url, ua.into())).await;
                eprintln!("challenge solved: {solved:?}");
            })
            .detach();
        }

        let bounds = Bounds::centered(None, size(px(1280.), px(860.)), cx);
        let (titlebar, decorations) = titlebar::options();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(titlebar),
                window_decorations: decorations,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Root::new(host, data_dir, first, window, cx)),
        )
        .expect("open window");
        cx.activate(true);
    });
}
