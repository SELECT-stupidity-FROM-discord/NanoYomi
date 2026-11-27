//! Site checks and sign-ins in a web view inside the main window, with its cookies handed to the extension host.

use crate::catalog::small_button;
use crate::host::Host;
use crate::store::Store;
use crate::{LINE, RULE, TEXT, u};
use futures::channel::oneshot;
use gpui::{
    App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels, Render, Size,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, WindowHandle, div, px, rgb,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const PAGE_PASSED_JS: &str = r#"(() => {
  const challenge = /just a moment|attention required|un instant/i.test(document.title || '')
    || !!document.querySelector('#challenge-form, #challenge-running, #challenge-stage, #cf-challenge-running, [name="cf-turnstile-response"]');
  return document.readyState === 'complete' && !challenge && !!document.body && document.body.innerText.trim().length > 0;
})()"#;

const TIMEOUT: Duration = Duration::from_secs(120);
/// Most checks pass on their own by now. After this the window shows so you can click.
const SHOW_AFTER: Duration = Duration::from_secs(12);
/// Quick checks pass before this, so the pill only shows for slower ones.
const PILL_AFTER: Duration = Duration::from_secs(2);

/// A check closes itself when passed, a sign-in waits for you to close it.
enum Mode {
    Check(oneshot::Sender<bool>),
    SignIn,
}

pub struct Check {
    webview: wry::WebView,
    title: &'static str,
    shown: bool,
    waiting: bool,
    site: String,
    root: WeakEntity<crate::Root>,
    store: Entity<Store>,
    /// Set when the user closes the panel's own window.
    #[cfg(target_os = "linux")]
    closed: Arc<std::sync::atomic::AtomicBool>,
    _sub: Subscription,
}

#[cfg(target_os = "linux")]
impl Drop for Check {
    fn drop(&mut self) {
        crate::webviews::native::close(&self.webview);
    }
}

pub fn browser_user_agent(_extension_agent: String) -> String {
    if cfg!(target_os = "macos") {
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.2 Safari/605.1.15".into()
    } else {
        String::new()
    }
}

fn main_window(cx: &App) -> Option<WindowHandle<crate::Root>> {
    cx.windows().into_iter().find_map(|w| w.downcast::<crate::Root>())
}

/// Resolves true once the clearance cookie reaches the host. Errors if the user closes the panel.
pub fn solve(cx: &mut App, host: Arc<Host>, url: String, user_agent: String) -> oneshot::Receiver<bool> {
    let (tx, rx) = oneshot::channel();
    open(cx, host, url, user_agent, "Verify to continue", Mode::Check(tx));
    rx
}

/// Opens the site to sign in. Its cookies go to the host when the panel closes.
pub fn browse(cx: &mut App, host: Arc<Host>, url: String, user_agent: String) {
    open(cx, host, url, user_agent, "Sign in, then close this", Mode::SignIn);
}

fn open(cx: &mut App, host: Arc<Host>, url: String, user_agent: String, title: &'static str, mode: Mode) {
    let Some(main) = main_window(cx) else { return };
    let _ = main.update(cx, |root, window, cx| {
        let mut builder = crate::webviews::builder().with_url(&url).with_bounds(rect(crate::titlebar::viewport(window)));
        if !user_agent.is_empty() {
            builder = builder.with_user_agent(&user_agent);
        }
        // Linux can't embed WebKitGTK in gpui's window, so the panel's view lives in its own window.
        #[cfg(target_os = "linux")]
        let built = crate::webviews::native::build(builder, 600., 724.);
        #[cfg(not(target_os = "linux"))]
        let built = builder.build_as_child(window);
        let webview = match built {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("system webview unavailable: {e}");
                return;
            }
        };
        let root_weak = cx.weak_entity();
        let store = root.store.clone();
        let check = cx.new(|cx| Check::new(webview, host, url, user_agent, title, mode, root_weak, store, window, cx));
        root.check_sub = Some(cx.observe(&check, |_, _, cx| cx.notify()));
        root.check = Some(check);
        cx.notify();
    });
}

impl Check {
    pub fn is_open(&self) -> bool {
        self.shown
    }

    #[allow(clippy::too_many_arguments)]
    fn new(webview: wry::WebView, host: Arc<Host>, url: String, user_agent: String, title: &'static str, mode: Mode, root: WeakEntity<crate::Root>, store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Kept drawn off to the side so the page runs at full speed.
        crate::webviews::native::keep_running_hidden(&webview);
        let sign_in = matches!(mode, Mode::SignIn);
        let site = url.split("//").nth(1).and_then(|r| r.split('/').next()).unwrap_or(&url).to_string();
        let close_root = root.clone();
        #[cfg(target_os = "linux")]
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(target_os = "linux")]
        let watch_closed = closed.clone();

        cx.spawn_in(window, async move |this, cx| {
            let mut tx = match mode {
                Mode::Check(tx) => Some(tx),
                Mode::SignIn => None,
            };
            // A stale clearance would look like an instant solve, so clear it and reload.
            let mut stale: Option<Vec<String>> = None;
            let started = std::time::Instant::now();
            // Two passing checks in a row count as solved.
            let passed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let agent = Arc::new(std::sync::Mutex::new(user_agent.clone()));
            let mut clear_ticks = 0;
            let mut shown = sign_in;
            let mut last = Vec::new();
            if sign_in {
                let _ = this.update(cx, |this: &mut Check, cx| {
                    this.shown = true;
                    cx.notify();
                });
            }
            let finish = |cx: &mut gpui::AsyncWindowContext| {
                let _ = close_root.update(cx, |r: &mut crate::Root, cx| {
                    r.check = None;
                    cx.notify();
                });
            };
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                #[cfg(target_os = "linux")]
                if watch_closed.load(Ordering::Relaxed) {
                    finish(cx);
                    if sign_in {
                        break;
                    }
                    return;
                }
                if !sign_in && !shown && started.elapsed() > PILL_AFTER {
                    let _ = this.update(cx, |this: &mut Check, cx| {
                        this.waiting = true;
                        cx.notify();
                    });
                }
                if !shown && started.elapsed() > SHOW_AFTER {
                    shown = true;
                    let _ = this.update(cx, |this: &mut Check, cx| {
                        this.shown = true;
                        cx.notify();
                    });
                }
                // Give up eventually, or every queued page waits behind it.
                if !sign_in && started.elapsed() > TIMEOUT {
                    finish(cx);
                    return;
                }
                // This errors once the panel is gone, which means the user closed it.
                let Ok(cookies) = this.update(cx, |this: &mut Check, _| this.webview.cookies_for_url(&url)) else {
                    break;
                };
                let cookies = cookies.unwrap_or_default();
                if sign_in {
                    last = cookies;
                    continue;
                }
                let old = cookies.iter().filter(|c| c.name() == "cf_clearance").map(|c| c.value().to_string()).collect::<Vec<_>>();
                let Some(stale) = &stale else {
                    if !old.is_empty() {
                        let _ = this.update(cx, |this: &mut Check, _| {
                            for c in cookies.iter().filter(|c| c.name() == "cf_clearance") {
                                let _ = this.webview.delete_cookie(c);
                            }
                            let _ = this.webview.load_url(&url);
                        });
                    }
                    stale = Some(old);
                    continue;
                };
                // Solved when a new clearance shows up, or the page stops being a challenge.
                clear_ticks = if passed.swap(false, Ordering::Relaxed) { clear_ticks + 1 } else { 0 };
                let p = passed.clone();
                let own_agent = user_agent.is_empty().then(|| agent.clone());
                let _ = this.update(cx, |this: &mut Check, _| {
                    if let Some(agent) = own_agent {
                        let _ = this.webview.evaluate_script_with_callback("navigator.userAgent", move |r| {
                            if let Ok(ua) = serde_json::from_str::<String>(&r) {
                                *agent.lock().unwrap() = ua;
                            }
                        });
                    }
                    this.webview.evaluate_script_with_callback(PAGE_PASSED_JS, move |r| {
                        if r == "true" {
                            p.store(true, Ordering::Relaxed);
                        }
                    })
                });
                if !old.iter().any(|v| !stale.contains(v)) && clear_ticks < 2 {
                    continue;
                }
                let passed_agent = agent.lock().unwrap().clone();
                if passed_agent.is_empty() {
                    continue;
                }
                // The full Set-Cookie form, which the host parses.
                let header_values = cookies.iter().map(|c| c.to_string()).collect();
                let ok = host.set_cookies(&url, header_values).await.is_ok()
                    // The webview passed with this agent, so the host keeps using it for the site.
                    && host.call("setUserAgent", serde_json::json!({ "url": url, "userAgent": passed_agent })).await.is_ok();
                if let Some(tx) = tx.take() {
                    let _ = tx.send(ok);
                }
                finish(cx);
                return;
            }
            // Closing the panel ends a sign-in, so hand over its cookies.
            let header_values = last.iter().map(|c| c.to_string()).collect();
            let _ = host.set_cookies(&url, header_values).await;
            let signed_in_agent = agent.lock().unwrap().clone();
            if !signed_in_agent.is_empty() {
                let _ = host.call("setUserAgent", serde_json::json!({ "url": url, "userAgent": signed_in_agent })).await;
            }
        })
        .detach();

        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Check { webview, title, shown: sign_in, waiting: false, site, root, store, #[cfg(target_os = "linux")] closed, _sub: sub }
    }
}

/// The panel's rectangle, centred.
fn panel(viewport: Size<Pixels>) -> (f32, f32, f32, f32) {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let w = f32::from(u(600.)).min(vw - f32::from(u(48.)));
    let h = f32::from(u(780.)).min(vh - f32::from(u(48.)));
    ((vw - w) / 2., (vh - h) / 2., w, h)
}

impl Render for Check {
    #[allow(refining_impl_trait)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (x, y, w, h) = panel(crate::titlebar::viewport(window));
        let (left, top) = crate::titlebar::origin(window);
        let head = f32::from(u(56.));
        #[cfg(target_os = "linux")]
        if self.shown {
            let closed = self.closed.clone();
            crate::webviews::native::present(&self.webview, self.title, move || closed.store(true, Ordering::Relaxed));
        }
        if self.shown {
            let _ = self.webview.set_bounds(bounds(left + x, top + y + head, w, h - head));
        } else {
            // Off to the side at full size so the page lays out normally.
            let _ = self.webview.set_bounds(bounds(-10000., 0., w, h - head));
            if !self.waiting {
                return div().into_any_element();
            }
            return div()
                .id("check-pill")
                .occlude()
                .absolute()
                .bottom(u(if crate::update::pending(self.store.read(cx)).is_some() { 24. + 52. } else { 24. }))
                .right(u(24.))
                .h(u(40.))
                .pl(u(16.))
                .pr(u(5.))
                .gap(u(12.))
                .flex()
                .items_center()
                .rounded(u(20.))
                .bg(rgb(crate::SURFACE))
                .border_1()
                .border_color(rgb(LINE))
                .text_size(u(13.))
                .text_color(rgb(crate::MUTED))
                .child(format!("Checking {}…", self.site))
                .child(small_button("check-show", "Show now", false).on_click(cx.listener(|this, _, _, cx| {
                    this.shown = true;
                    cx.notify();
                })))
                .into_any_element();
        }
        div()
            .id("check-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .bg(gpui::rgba(0x0000008C))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(w))
                    .h(px(h))
                    .flex()
                    .flex_col()
                    .bg(rgb(crate::SURFACE))
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded(u(12.))
                    .overflow_hidden()
                    .child(
                        div()
                            .h(px(head))
                            .flex_none()
                            .px(u(20.))
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(rgb(RULE))
                            .child(div().flex_1().min_w_0().truncate().text_size(u(16.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(TEXT)).child(self.title))
                            .child(small_button("check-close", "Close", false).on_click(cx.listener(|this, _, _, cx| {
                                let _ = this.root.update(cx, |r, cx| {
                                    r.check = None;
                                    cx.notify();
                                });
                            }))),
                    ),
            )
            .into_any_element()
    }
}

fn bounds(x: f32, y: f32, w: f32, h: f32) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(f64::from(x), f64::from(y)).into(),
        size: wry::dpi::LogicalSize::new(f64::from(w), f64::from(h)).into(),
    }
}

fn rect(s: Size<Pixels>) -> wry::Rect {
    bounds(0., 0., f32::from(s.width), f32::from(s.height))
}
