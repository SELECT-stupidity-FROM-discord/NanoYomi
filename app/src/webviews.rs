//! Extensions' Android WebViews, run as hidden WebKit views in one hidden window.
//! Each live view can cost a WebKit process (100+ MB), so views close when the extension is done.

use crate::host::Host;
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, Render, Styled, Window, WindowBounds, WindowHandle, WindowOptions, div,
    px, size,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// More than this means an extension is leaking views.
// Fixed cap. Queue instead if an extension ever needs more.
const MAX_VIEWS: usize = 6;

/// Runs in every view: console capture and the reply channel scripts use.
const BOOTSTRAP: &str = r#"(() => {
  // Stay out of other sites' frames. Turnstile checks its own window for tampering.
  try { if (window !== window.top && window.top.location.origin !== location.origin) return; } catch (e) { return; }
  if (window !== window.top && !window.webkit) return;
  const post = (m) => { try { (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify(m)); } catch (e) {} };
  Object.defineProperty(window, '__yomiPost', { value: post });
  // Android's shouldInterceptRequest. fetch() and async XMLHttpRequest wait for the extension's answer, sync XHR is only reported.
  const absolute = (url) => { try { return new URL(url, location.href).href; } catch (e) { return String(url); } };
  const pending = new Map();
  // Ids are unique across frames, so an answer can go to every frame.
  const newId = () => Math.random().toString(36).slice(2) + Date.now().toString(36);
  Object.defineProperty(window, '__yomiIntercepted', { value: (id, answer) => { const done = pending.get(id); if (done) { pending.delete(id); done(answer); } } });
  const decode = (a) => Uint8Array.from(atob(a.body || ''), (c) => c.charCodeAt(0));
  // The extension may answer a frame's own document too.
  if (window !== window.top && /^https?:/.test(location.href)) {
    const id = newId();
    pending.set(id, (a) => {
      if (!a || a.error) return;
      document.open();
      document.write(new TextDecoder().decode(decode(a)));
      document.close();
    });
    post({ t: 'request', id, url: location.href, method: 'GET', main: false });
  }
  const fetch0 = window.fetch;
  if (fetch0) window.fetch = function (input, init) {
    const self = this, args = arguments;
    const url = absolute(typeof input === 'string' ? input : (input && input.url) || String(input));
    const method = ((init && init.method) || (input && input.method) || 'GET').toUpperCase();
    const id = newId();
    const answer = new Promise((resolve) => { pending.set(id, resolve); setTimeout(() => window.__yomiIntercepted(id, null), 60000); });
    post({ t: 'request', id, url, method, main: false });
    return answer.then((a) => {
      if (!a) return fetch0.apply(self, args);
      if (a.error) throw new TypeError('Failed to fetch');
      return new Response(decode(a), { status: a.status || 200, statusText: a.reason || 'OK', headers: a.headers || {} });
    });
  };
  const ask = (url, method) => {
    const id = newId();
    const answer = new Promise((resolve) => { pending.set(id, resolve); setTimeout(() => window.__yomiIntercepted(id, null), 60000); });
    post({ t: 'request', id, url, method, main: false });
    return answer;
  };
  const XHR = XMLHttpRequest.prototype, open0 = XHR.open, send0 = XHR.send, abort0 = XHR.abort;
  const req = new WeakMap();
  XHR.open = function (method, url, async) {
    req.set(this, { url: absolute(url), method: String(method || 'GET').toUpperCase(), async: async !== false });
    return open0.apply(this, arguments);
  };
  XHR.abort = function () { const r = req.get(this); if (r) r.aborted = true; return abort0.apply(this, arguments); };
  // An answered request looks finished to the page, as when Android serves the extension's response.
  const finish = (xhr, r, a) => {
    const body = a.error ? new Uint8Array() : decode(a);
    const text = new TextDecoder().decode(body);
    const headers = a.headers || {};
    const type = xhr.responseType;
    let response = text;
    if (type === 'json') { try { response = JSON.parse(text); } catch (e) { response = null; } }
    else if (type === 'arraybuffer') response = body.buffer;
    else if (type === 'blob') response = new Blob([body], { type: headers['Content-Type'] || headers['content-type'] || '' });
    const fixed = { readyState: 4, status: a.error ? 0 : a.status || 200, statusText: a.error ? '' : a.reason || 'OK', responseURL: r.url, response, responseText: type === '' || type === 'text' ? text : '' };
    for (const k in fixed) Object.defineProperty(xhr, k, { configurable: true, get: () => fixed[k] });
    const lines = Object.keys(headers).map((k) => k.toLowerCase() + ': ' + headers[k]).join('\r\n');
    xhr.getAllResponseHeaders = () => (a.error ? '' : lines);
    xhr.getResponseHeader = (n) => { const k = Object.keys(headers).find((h) => h.toLowerCase() === String(n).toLowerCase()); return a.error || k === undefined ? null : String(headers[k]); };
    for (const e of ['readystatechange', a.error ? 'error' : 'load', 'loadend']) xhr.dispatchEvent(new ProgressEvent(e));
  };
  XHR.send = function () {
    const r = req.get(this), self = this, args = arguments;
    if (!r || !r.async) {
      if (r) post({ t: 'request', url: r.url, method: r.method, main: false });
      return send0.apply(this, args);
    }
    ask(r.url, r.method).then((a) => {
      if (r.aborted) return;
      if (!a) return send0.apply(self, args);
      finish(self, r, a);
    });
  };
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    const original = console[level];
    console[level] = function (...args) {
      try { post({ t: 'console', level: level === 'warn' ? 'warning' : level === 'info' ? 'log' : level, message: args.map(String).join(' '), source: location.href }); } catch (e) {}
      return original.apply(this, args);
    };
  }
})();"#;

pub struct WebViewHost {
    host: Arc<Host>,
    views: HashMap<i64, wry::WebView>,
    /// The view shown to the user (a captcha to tap), if any. The window is only visible then.
    presented: Option<i64>,
}

impl Render for WebViewHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

pub fn start(cx: &mut App, host: Arc<Host>) {
    let Some(mut requests) = host.take_webview_requests() else { return };
    #[cfg(target_os = "linux")]
    native::start(cx);
    cx.spawn(async move |cx| {
        let mut window: Option<WindowHandle<WebViewHost>> = None;
        while let Some(msg) = requests.next().await {
            let handle = match window {
                Some(w) => w,
                None => {
                    let host = host.clone();
                    let opened = cx.update(|cx| {
                        cx.open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(411.), px(891.))))),
                                show: false,
                                focus: false,
                                ..Default::default()
                            },
                            |window, cx| {
                                cx.new(|cx| {
                                    // Closing the captcha window means cancel. The window itself stays around.
                                    let this = cx.entity().downgrade();
                                    window.on_window_should_close(cx, move |window, cx| {
                                        if let Some(this) = this.upgrade() {
                                            this.update(cx, |this: &mut WebViewHost, _| this.cancel_presented(window));
                                        }
                                        false
                                    });
                                    WebViewHost { host, views: HashMap::new(), presented: None }
                                })
                            },
                        )
                    });
                    // Opening a window, even a hidden one, can steal focus. Give it back.
                    let _ = cx.update(|cx| {
                        let main = cx.windows().into_iter().find(|w| w.downcast::<WebViewHost>().is_none());
                        if let Some(main) = main {
                            let _ = main.update(cx, |_, window, _| window.activate_window());
                        }
                    });
                    match opened {
                        Ok(w) => *window.insert(w),
                        Err(e) => {
                            eprintln!("webview window: {e}");
                            continue;
                        }
                    }
                }
            };
            let shown_done = handle.update(cx, |this, window, _| this.handle(msg, window)).unwrap_or(false);
            if shown_done {
                // A captcha window just closed, so put the user back in the app's window.
                let _ = cx.update(|cx| {
                    let main = cx.windows().into_iter().find(|w| w.downcast::<WebViewHost>().is_none());
                    if let Some(main) = main {
                        let _ = main.update(cx, |_, window, _| window.activate_window());
                    }
                });
            }
        }
    })
    .detach();
}

pub fn is_webview_window(cx: &App, id: gpui::WindowId) -> bool {
    cx.windows().iter().any(|w| w.window_id() == id && w.downcast::<WebViewHost>().is_some())
}

impl WebViewHost {
    fn event(&self, view: i64, kind: &str, mut fields: Value) {
        fields["wvEvent"] = kind.into();
        fields["view"] = view.into();
        self.host.notify(&fields);
    }

    fn cancel_presented(&mut self, window: &mut Window) {
        if let Some(view) = self.presented.take() {
            self.event(view, "presentClosed", json!({}));
        }
        self.hide_window(window);
    }

    fn hide_window(&self, _window: &mut Window) {
        #[cfg(target_os = "linux")]
        for wv in self.views.values() {
            native::hide_window(wv);
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(wv) = self.views.values().next() {
            native::hide_window(wv);
        }
        for wv in self.views.values() {
            let _ = wv.set_visible(true);
        }
    }

    fn handle(&mut self, msg: Value, window: &mut Window) -> bool {
        debug("wv>", &msg.to_string());
        let view = msg["view"].as_i64().unwrap_or(-1);
        let s = |k: &str| msg[k].as_str().unwrap_or_default().to_string();
        let op = s("wv");
        if op == "create" {
            if self.views.len() >= MAX_VIEWS {
                eprintln!("webview: refusing view {view}; {MAX_VIEWS} already open");
                self.event(view, "gone", json!({ "crashed": false }));
                return false;
            }
            match self.build(view, msg["width"].as_f64().unwrap_or(411.), msg["height"].as_f64().unwrap_or(891.), window) {
                Ok(wv) => {
                    if self.views.is_empty() && self.presented.is_none() {
                        native::hide_window(&wv);
                    }
                    self.views.insert(view, wv);
                }
                Err(e) => {
                    eprintln!("webview {view}: {e}");
                    self.event(view, "gone", json!({ "crashed": true }));
                }
            }
            return false;
        }
        if op == "cookies" {
            // WebKit's cookie store is shared by every view, so any view will do.
            if let Some(wv) = self.views.values().next() {
                let url = s("url");
                let host = url.split("://").nth(1).and_then(|r| r.split(['/', ':']).next()).unwrap_or_default().to_string();
                for raw in msg["cookies"].as_array().into_iter().flatten().filter_map(|c| c.as_str()) {
                    if let Ok(mut c) = wry::cookie::Cookie::parse(raw.to_string()) {
                        if c.domain().is_none() {
                            c.set_domain(host.clone());
                        }
                        if c.path().is_none() {
                            c.set_path("/");
                        }
                        let _ = wv.set_cookie(&c);
                    }
                }
            }
            return false;
        }
        if op == "destroy" {
            native::cleanup(view);
            #[cfg(target_os = "linux")]
            if let Some(wv) = self.views.get(&view) {
                native::close(wv);
            }
            self.views.remove(&view); // dropping it detaches the view, and WebKit's process exits when idle
            if self.presented == Some(view) {
                self.presented = None;
                self.hide_window(window);
                return true;
            }
            return false;
        }
        if op == "unpresent" {
            if self.presented != Some(view) {
                return false;
            }
            self.presented = None;
            self.hide_window(window);
            return true;
        }
        if op == "present" {
            let Some(wv) = self.views.get(&view) else { return false };
            for (id, other) in &self.views {
                let _ = other.set_visible(*id == view);
            }
            if let Ok(b) = wv.bounds() {
                let s = b.size.to_logical::<f64>(window.scale_factor() as f64);
                window.resize(size(px(s.width as f32), px(s.height as f32)));
            }
            window.set_window_title("Verify to continue");
            native::show_window(wv);
            #[cfg(target_os = "linux")]
            {
                let host = self.host.clone();
                native::present(wv, "Verify to continue", move || host.notify(&json!({ "wvEvent": "presentClosed", "view": view })));
            }
            window.activate_window();
            self.presented = Some(view);
            return false;
        }
        let Some(wv) = self.views.get(&view) else { return false };
        match op.as_str() {
            "settings" => {
                native::set_user_agent(wv, &s("userAgent"));
                if let Some(allow_images) = msg["images"].as_bool() {
                    native::set_images(view, wv, allow_images);
                }
            }
            "load" => {
                let mut headers = wry::http::HeaderMap::new();
                if let Some(map) = msg["headers"].as_object() {
                    for (k, v) in map {
                        if let (Ok(name), Ok(value)) =
                            (wry::http::HeaderName::try_from(k.as_str()), wry::http::HeaderValue::try_from(v.as_str().unwrap_or_default()))
                        {
                            headers.insert(name, value);
                        }
                    }
                }
                let url = s("url");
                let _ = if headers.is_empty() { wv.load_url(&url) } else { wv.load_url_with_headers(&url, headers) };
            }
            "loadHtml" => native::load_html(wv, &s("html"), &s("baseUrl")),
            "eval" => {
                let script = s("script");
                let js = match msg["cb"].as_i64() {
                    // Android hands back the result as JSON text. Promises aren't awaited.
                    Some(cb) => format!(
                        "(() => {{ let v; try {{ v = JSON.stringify((0, eval)({src})); }} catch (e) {{ v = 'null'; }} \
                         (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify({{ t: 'eval', cb: {cb}, value: v === undefined ? 'null' : v }})); }})()",
                        src = Value::from(script)
                    ),
                    None => script,
                };
                let _ = wv.evaluate_script(&js);
            }
            "addInterface" => {
                // window.<name>.<method>(...args) calls the Java object's @JavascriptInterface method.
                let name = Value::from(s("name"));
                let js = format!(
                    "window[{name}] = new Proxy({{}}, {{ get: (_, m) => typeof m !== 'string' || m === 'then' || m === 'toJSON' ? undefined : \
                     (...args) => {{ (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify({{ t: 'call', name: {name}, method: m, args }})); }} }});"
                );
                self.add_script(wv, &js);
                let _ = wv.evaluate_script(&js);
            }
            "addListener" => {
                // An AndroidX web message listener: window.<name>.postMessage(text).
                let name = Value::from(s("name"));
                let js = format!(
                    "window[{name}] = {{ onmessage: null, postMessage: (data) => (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify(\
                     {{ t: 'message', name: {name}, data: typeof data === 'string' ? data : String(data), origin: location.origin, main: window === window.top }})) }};"
                );
                self.add_script(wv, &js);
            }
            "addScript" => {
                let rules: Vec<String> = msg["rules"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();
                let script = s("script");
                let js = if rules.iter().any(|r| r == "*") {
                    script
                } else {
                    // Only in matching origins, like Chromium. The host checks origins too.
                    format!("if ({}) {{\n{script}\n}}", origin_check(&rules))
                };
                self.add_script(wv, &js);
            }
            "touch" => {
                // Real mouse events, because sites ignore scripted clicks.
                let (x, y) = (msg["x"].as_f64().unwrap_or(0.), msg["y"].as_f64().unwrap_or(0.));
                native::touch(wv, msg["action"].as_i64().unwrap_or(-1), x, y);
            }
            "resize" => {
                #[cfg(target_os = "linux")]
                native::resize(wv, msg["width"].as_f64().unwrap_or(411.), msg["height"].as_f64().unwrap_or(891.));
                let _ = wv.set_bounds(rect(msg["width"].as_f64().unwrap_or(411.), msg["height"].as_f64().unwrap_or(891.)));
            }
            "stop" => native::stop(wv),
            _ => {}
        }
        false
    }

    fn add_script(&self, wv: &wry::WebView, js: &str) {
        native::add_script(wv, js);
    }

    fn build(&self, view: i64, width: f64, height: f64, _window: &mut Window) -> wry::Result<wry::WebView> {
        let ipc_host = self.host.clone();
        let load_host = self.host.clone();
        let b = builder()
            .with_bounds(rect(width, height))
            .with_initialization_script_for_main_only(BOOTSTRAP, false)
            .with_ipc_handler(move |req| {
                let Ok(mut m) = serde_json::from_str::<Value>(req.body()) else { return };
                let kind = m["t"].as_str().unwrap_or_default().to_string();
                if let Some(o) = m.as_object_mut() {
                    o.remove("t");
                }
                m["wvEvent"] = kind.into();
                m["view"] = view.into();
                debug("wv<", &m.to_string());
                ipc_host.notify(&m);
            })
            .with_on_page_load_handler(move |event, url| {
                let kind = match event {
                    wry::PageLoadEvent::Started => "pageStarted",
                    wry::PageLoadEvent::Finished => "pageFinished",
                };
                debug("wv<", &format!("{kind} {view} {url}"));
                load_host.notify(&json!({ "wvEvent": kind, "view": view, "url": url }));
            });
        // WebKitGTK can't be a child of gpui's window, so Linux gives each view its own GTK window.
        #[cfg(target_os = "linux")]
        let wv = native::build(b, width, height)?;
        #[cfg(not(target_os = "linux"))]
        let wv = b.build_as_child(_window)?;
        native::keep_running_hidden(&wv);
        native::prepare_image_blocker();
        Ok(wv)
    }
}

/// WebKit calls wry doesn't wrap. Linux gets stand-ins for now, so pages that need them don't work there yet.
#[cfg(target_os = "macos")]
pub(crate) mod native {
    use objc2::MainThreadOnly;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSError, NSPoint, NSString, NSURL};
    use objc2_web_kit::{
        WKContentRuleList, WKContentRuleListStore, WKInactiveSchedulingPolicy, WKUserContentController,
        WKUserScript, WKUserScriptInjectionTime,
    };
    use std::cell::{Cell, RefCell};
    use wry::{WebView, WebViewExtMacOS};

    const BLOCK_IMAGES_JSON: &str =
        r#"[{"trigger":{"url-filter":".*","resource-type":["image"]},"action":{"type":"block"}}]"#;

    thread_local! {
        static BLOCK_IMAGES_RULE: RefCell<Option<Retained<WKContentRuleList>>> = const { RefCell::new(None) };
        static PENDING_CONTROLLERS: RefCell<Vec<Retained<WKUserContentController>>> = const { RefCell::new(Vec::new()) };
        static COMPILING_RULE: Cell<bool> = const { Cell::new(false) };
    }

    pub fn prepare_image_blocker() {
        let already = BLOCK_IMAGES_RULE.with(|c| c.borrow().is_some()) || COMPILING_RULE.with(|c| c.get());
        if already {
            return;
        }
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else { return };
        COMPILING_RULE.with(|c| c.set(true));
        let block = block2::RcBlock::new(|list: *mut WKContentRuleList, _err: *mut NSError| {
            COMPILING_RULE.with(|c| c.set(false));
            if !list.is_null()
                && let Some(rule) = unsafe { Retained::retain(list) }
            {
                BLOCK_IMAGES_RULE.with(|cell| *cell.borrow_mut() = Some(rule.clone()));
                let waiting: Vec<_> = PENDING_CONTROLLERS.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
                for ctrl in waiting {
                    unsafe { ctrl.addContentRuleList(&rule) };
                }
            }
        });
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&NSString::from_str("nano-block-images")),
                Some(&NSString::from_str(BLOCK_IMAGES_JSON)),
                Some(&*block),
            );
        }
    }

    pub fn set_images(_view: i64, wv: &WebView, allow_images: bool) {
        let ctrl = wv.manager();
        if allow_images {
            PENDING_CONTROLLERS.with(|cell| {
                cell.borrow_mut().retain(|c| !std::ptr::eq(Retained::as_ptr(c), Retained::as_ptr(&ctrl)));
            });
            unsafe { ctrl.removeAllContentRuleLists() };
            return;
        }
        if let Some(rule) = BLOCK_IMAGES_RULE.with(|cell| cell.borrow().clone()) {
            unsafe { ctrl.addContentRuleList(&rule) };
            return;
        }
        PENDING_CONTROLLERS.with(|cell| {
            let mut list = cell.borrow_mut();
            if !list.iter().any(|c| std::ptr::eq(Retained::as_ptr(c), Retained::as_ptr(&ctrl))) {
                list.push(ctrl);
            }
        });
        prepare_image_blocker();
    }

    pub fn cleanup(_view: i64) {}

    pub fn set_user_agent(wv: &WebView, ua: &str) {
        unsafe { wv.webview().setCustomUserAgent(Some(&NSString::from_str(ua))) };
    }

    pub fn load_html(wv: &WebView, html: &str, base: &str) {
        unsafe {
            let base = NSURL::URLWithString(&NSString::from_str(base));
            let _ = wv.webview().loadHTMLString_baseURL(&NSString::from_str(html), base.as_deref());
        }
    }

    pub fn stop(wv: &WebView) {
        unsafe { wv.webview().stopLoading() };
    }

    /// Keeps the window drawn but invisible, behind everything. Pages only lazy-load images while on screen.
    pub fn hide_window(wv: &WebView) {
        if let Some(w) = wv.webview().window() {
            w.setAlphaValue(0.);
            w.setIgnoresMouseEvents(true);
            w.orderBack(None);
        }
    }

    pub fn show_window(wv: &WebView) {
        if let Some(w) = wv.webview().window() {
            w.setAlphaValue(1.);
            w.setIgnoresMouseEvents(false);
        }
    }

    /// Stops WebKit throttling hidden views or pausing covered pages. It's private API, so only if present.
    pub fn keep_running_hidden(wv: &WebView) {
        let wk = wv.webview();
        unsafe {
            wk.configuration().preferences().setInactiveSchedulingPolicy(WKInactiveSchedulingPolicy::None);
            let sel = objc2::sel!(_setWindowOcclusionDetectionEnabled:);
            let responds: bool = objc2::msg_send![&*wk, respondsToSelector: sel];
            if responds {
                let _: () = objc2::msg_send![&*wk, _setWindowOcclusionDetectionEnabled: false];
            } else {
                eprintln!("webview: no occlusion switch in this WebKit");
            }
        }
    }

    /// Runs `js` at document start in every frame of every later page.
    pub fn add_script(wv: &WebView, js: &str) {
        // GPUI calls this on the main thread, where WebKit objects have to be made.
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        unsafe {
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(js),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
            );
            wv.manager().addUserScript(&script);
        }
    }

    /// action is 0 down, 1 up, 2 move. (x, y) are CSS pixels from the top-left.
    pub fn touch(wv: &WebView, action: i64, x: f64, y: f64) {
        let kind = match action {
            0 => NSEventType::LeftMouseDown,
            1 => NSEventType::LeftMouseUp,
            2 => NSEventType::LeftMouseDragged,
            _ => return,
        };
        let wk = wv.webview();
        let Some(ns_window) = wk.window() else { return };
        // Window coordinates start at the bottom.
        let frame = wk.frame();
        let point = NSPoint::new(frame.origin.x + x, frame.origin.y + frame.size.height - y);
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            point,
            NSEventModifierFlags::empty(),
            0.,
            ns_window.windowNumber(),
            None,
            0,
            1,
            if kind == NSEventType::LeftMouseUp { 0. } else { 1. },
        );
        if let Some(event) = event {
            match kind {
                NSEventType::LeftMouseDown => wk.mouseDown(&event),
                NSEventType::LeftMouseUp => wk.mouseUp(&event),
                _ => wk.mouseDragged(&event),
            }
        }
    }
}

/// The WebView2 calls wry doesn't wrap.
#[cfg(windows)]
pub(crate) mod native {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, LazyLock, Mutex};
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{CallDevToolsProtocolMethodCompletedHandler, WebResourceRequestedEventHandler};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::SHCreateMemStream;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{HSTRING, Interface, w};
    use wry::{WebView, WebViewExtWindows};

    static BLOCK_IMAGES: LazyLock<Mutex<HashMap<i64, Arc<AtomicBool>>>> = LazyLock::new(Mutex::default);

    /// Every handler on a view gets every request that matches any of its filters, so each one checks the kind it wants.
    fn is_kind(args: &ICoreWebView2WebResourceRequestedEventArgs, kind: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> bool {
        let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
        unsafe { args.ResourceContext(&mut context) }.is_ok() && context == kind
    }

    pub fn prepare_image_blocker() {}

    pub fn set_images(view: i64, wv: &WebView, allow_images: bool) {
        let blocked = !allow_images;
        if let Some(flag) = BLOCK_IMAGES.lock().unwrap().get(&view) {
            flag.store(blocked, Ordering::Relaxed);
            return;
        }
        if allow_images {
            return;
        }
        let flag = Arc::new(AtomicBool::new(true));
        BLOCK_IMAGES.lock().unwrap().insert(view, flag.clone());
        let (webview, env) = (wv.webview(), wv.environment());
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            if !flag.load(Ordering::Relaxed) || !is_kind(&args, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE) {
                return Ok(());
            }
            unsafe {
                let response = env.CreateWebResourceResponse(None, 204, w!("No Content"), w!(""))?;
                args.SetResponse(&response)?;
            }
            Ok(())
        }));
        unsafe {
            let mut token = Default::default();
            let _ = webview.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE);
            let _ = webview.add_WebResourceRequested(&handler, &mut token);
        }
    }

    pub fn cleanup(view: i64) {
        BLOCK_IMAGES.lock().unwrap().remove(&view);
    }

    /// Chromium otherwise slows or pauses pages in a hidden or covered window.
    pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion \
        --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-renderer-backgrounding";

    pub fn set_user_agent(wv: &WebView, ua: &str) {
        unsafe {
            if let Ok(settings) = wv.webview().Settings().and_then(|s| s.cast::<ICoreWebView2Settings2>()) {
                let _ = settings.SetUserAgent(&HSTRING::from(ua));
            }
        }
    }

    /// Navigates to `base` and answers that one request with `html`, so the page gets the site's origin, as on Android.
    pub fn load_html(wv: &WebView, html: &str, base: &str) {
        if !base.starts_with("http") {
            let _ = wv.load_html(html);
            return;
        }
        let (webview, env) = (wv.webview(), wv.environment());
        let body = html.as_bytes().to_vec();
        let served = Arc::new(AtomicBool::new(false));
        // ponytail: each call leaves a spent handler behind; remove them by token if views ever load many documents.
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            if !is_kind(&args, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT) || served.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            unsafe {
                let stream = SHCreateMemStream(Some(&body));
                let response = env.CreateWebResourceResponse(stream.as_ref(), 200, w!("OK"), w!("Content-Type: text/html; charset=utf-8"))?;
                args.SetResponse(&response)?;
            }
            Ok(())
        }));
        unsafe {
            let mut token = Default::default();
            let filter = HSTRING::from(format!("{}*", base.trim_end_matches('/')));
            let ok = webview.AddWebResourceRequestedFilter(&filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT).is_ok()
                && webview.add_WebResourceRequested(&handler, &mut token).is_ok();
            if !ok || webview.Navigate(&HSTRING::from(base)).is_err() {
                let _ = wv.load_html(html);
            }
        }
    }

    pub fn stop(wv: &WebView) {
        let _ = unsafe { wv.webview().Stop() };
    }

    fn root(wv: &WebView) -> HWND {
        unsafe { GetAncestor(wv.hwnd(), GA_ROOT) }
    }

    /// Shown but far off screen and out of the taskbar, so pages still draw and lazy-load.
    pub fn hide_window(wv: &WebView) {
        let hwnd = root(wv);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize) & !(WS_EX_APPWINDOW.0 as isize));
            let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    /// Back in the taskbar, centred on the main screen.
    pub fn show_window(wv: &WebView) {
        let hwnd = root(wv);
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style & !(WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize)) | WS_EX_APPWINDOW.0 as isize);
            let mut r = Default::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), (sw - w) / 2, (sh - h) / 2, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    pub fn keep_running_hidden(wv: &WebView) {
        let _ = unsafe { wv.controller().SetIsVisible(true) };
    }

    /// Runs `js` at document start in every frame of every later page.
    pub fn add_script(wv: &WebView, js: &str) {
        let handler = webview2_com::AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(())));
        let _ = unsafe { wv.webview().AddScriptToExecuteOnDocumentCreated(&HSTRING::from(js), &handler) };
    }

    /// action is 0 down, 1 up, 2 move. (x, y) are CSS pixels from the top-left.
    /// Sent through DevTools, so the page sees trusted mouse events.
    pub fn touch(wv: &WebView, action: i64, x: f64, y: f64) {
        let (kind, buttons) = match action {
            0 => ("mousePressed", 1),
            1 => ("mouseReleased", 0),
            2 => ("mouseMoved", 1),
            _ => return,
        };
        let params = serde_json::json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 }).to_string();
        let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
        let _ = unsafe { wv.webview().CallDevToolsProtocolMethod(w!("Input.dispatchMouseEvent"), &HSTRING::from(params), &done) };
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod native {
    use gpui::App;
    use gtk::prelude::*;
    use std::cell::RefCell;
    use webkit2gtk::{SettingsExt, URIResponseExt, UserContentInjectedFrames, UserContentManagerExt, UserScript, UserScriptInjectionTime, WebResourceExt, WebViewExt};
    use wry::{WebView, WebViewBuilderExtUnix, WebViewExtUnix};

    /// GTK has its own loop, so gpui's loop turns it.
    // ponytail: polls every 16ms even when idle, drive it from GLib's fd if the wakeups matter.
    pub fn start(cx: &mut App) {
        if let Err(e) = gtk::init() {
            eprintln!("gtk: {e}");
            return;
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
            }
        })
        .detach();
    }

    /// An offscreen window counts as shown, so WebKit doesn't slow the page down.
    pub fn build(b: wry::WebViewBuilder<'_>, width: f64, height: f64) -> wry::Result<WebView> {
        let window = gtk::OffscreenWindow::new();
        window.set_default_size(width as i32, height as i32);
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&vbox);
        window.show_all();
        let wv = b.build_gtk(&vbox)?;
        if std::env::var_os("NANO_WV_DEBUG").is_some() {
            wv.webview().connect_resource_load_started(|_, resource, _| {
                let uri = resource.uri().map(|u| u.to_string()).unwrap_or_default();
                let u = uri.clone();
                resource.connect_finished(move |r| {
                    let res = r.response();
                    let code = res.as_ref().map_or(0, |x| x.status_code());
                    let head = |k: &str| res.as_ref().and_then(|x| x.http_headers()).and_then(|h| h.one(k)).map(|v| v.to_string()).unwrap_or_default();
                    if code < 400 && !head("content-type").contains("text/html") && !u.contains("/api/") {
                        return;
                    }
                    super::debug("wk", &format!("{code} {u} mitigated={} type={} server={}", head("cf-mitigated"), head("content-type"), head("server")));
                });
                resource.connect_failed(move |_, e| super::debug("wk", &format!("failed {uri}: {e}")));
            });
        }
        Ok(wv)
    }

    fn window(wv: &WebView) -> Option<gtk::Window> {
        wv.webview().toplevel()?.downcast().ok()
    }

    pub fn resize(wv: &WebView, width: f64, height: f64) {
        if let Some(w) = window(wv) {
            w.resize(width as i32, height as i32);
        }
    }

    pub fn close(wv: &WebView) {
        hide_window(wv);
        if let Some(w) = window(wv) {
            unsafe { w.destroy() };
        }
    }

    pub fn prepare_image_blocker() {}

    pub fn set_images(_: i64, wv: &WebView, allow: bool) {
        if let Some(s) = WebViewExt::settings(&wv.webview()) {
            s.set_auto_load_images(allow);
        }
    }

    pub fn cleanup(_: i64) {}

    /// The host claims Mac Safari by default, which Turnstile rejects next to Linux's real platform.
    pub fn set_user_agent(wv: &WebView, agent: &str) {
        if agent.contains("Macintosh") {
            return;
        }
        if let Some(s) = WebViewExt::settings(&wv.webview()) {
            s.set_user_agent((!agent.is_empty()).then_some(agent));
        }
    }

    pub fn load_html(wv: &WebView, html: &str, base: &str) {
        wv.webview().load_html(html, (!base.is_empty()).then_some(base));
    }

    pub fn stop(wv: &WebView) {
        wv.webview().stop_loading();
    }

    thread_local! {
        /// Views on show: the view, its window, and the offscreen box it came from.
        static SHOWN: RefCell<Vec<(gtk::Widget, gtk::Window, gtk::Box)>> = const { RefCell::new(Vec::new()) };
    }

    /// Moves the view into a real window for the user. Closing the window calls on_close, and the caller hides the view.
    pub fn present(wv: &WebView, title: &str, on_close: impl Fn() + 'static) {
        let widget: gtk::Widget = wv.webview().upcast();
        if SHOWN.with(|s| s.borrow().iter().any(|e| e.0 == widget)) {
            return;
        }
        let Some(home) = widget.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) else { return };
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(title);
        window.set_default_size(widget.allocated_width().max(1), widget.allocated_height().clamp(1, 800));
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&vbox);
        let _ = wv.reparent(&vbox);
        window.connect_delete_event(move |_, _| {
            on_close();
            glib::Propagation::Stop
        });
        window.show_all();
        window.present();
        SHOWN.with(|s| s.borrow_mut().push((widget, window, home)));
    }

    /// Puts the view back offscreen if it's on show.
    pub fn hide_window(wv: &WebView) {
        let widget: gtk::Widget = wv.webview().upcast();
        let entry = SHOWN.with(|s| {
            let mut s = s.borrow_mut();
            s.iter().position(|e| e.0 == widget).map(|i| s.remove(i))
        });
        if let Some((_, window, home)) = entry {
            let _ = wv.reparent(&home);
            unsafe { window.destroy() };
        }
    }

    pub fn show_window(_: &WebView) {}

    pub fn keep_running_hidden(_: &WebView) {}

    /// Runs at the start of every page, in every frame.
    pub fn add_script(wv: &WebView, js: &str) {
        if let Some(m) = wv.webview().user_content_manager() {
            m.add_script(&UserScript::new(js, UserContentInjectedFrames::AllFrames, UserScriptInjectionTime::Start, &[], &[]));
        }
    }

    /// action is 0 down, 1 up, 2 move. (x, y) are CSS pixels from the top-left. GDK events reach the page as trusted mouse events.
    pub fn touch(wv: &WebView, action: i64, x: f64, y: f64) {
        use glib::translate::{ToGlibPtr, ToGlibPtrMut};
        let widget = wv.webview();
        let Some(window) = widget.window() else { return };
        let Some(pointer) = gtk::gdk::Display::default().and_then(|d| d.default_seat()).and_then(|s| s.pointer()) else { return };
        // A down is preceded by a move, so the page has the pointer in place.
        let steps: &[gtk::gdk::EventType] = match action {
            0 => &[gtk::gdk::EventType::MotionNotify, gtk::gdk::EventType::ButtonPress],
            1 => &[gtk::gdk::EventType::ButtonRelease],
            2 => &[gtk::gdk::EventType::MotionNotify],
            _ => return,
        };
        for kind in steps {
            let mut event = gtk::gdk::Event::new(*kind);
            let held = if action == 0 && *kind == gtk::gdk::EventType::MotionNotify { 0 } else { gtk::gdk::ModifierType::BUTTON1_MASK.bits() };
            unsafe {
                let raw: *mut gtk::gdk::ffi::GdkEvent = event.to_glib_none_mut().0;
                // Button and motion events share their leading fields.
                let e = raw as *mut gtk::gdk::ffi::GdkEventButton;
                (*e).window = window.to_glib_full();
                (*e).send_event = 1;
                (*e).x = x;
                (*e).y = y;
                (*e).x_root = x;
                (*e).y_root = y;
                (*e).state = held;
                if *kind != gtk::gdk::EventType::MotionNotify {
                    (*e).button = 1;
                }
                gtk::gdk::ffi::gdk_event_set_device(raw, pointer.to_glib_none().0);
                gtk::gdk::ffi::gdk_event_set_source_device(raw, pointer.to_glib_none().0);
            }
            widget.event(&event);
        }
    }
}

/// Every WebView Nano makes starts here. WebView2 refuses a second view whose browser options differ.
pub fn builder() -> wry::WebViewBuilder<'static> {
    let builder = wry::WebViewBuilder::new();
    #[cfg(windows)]
    let builder = wry::WebViewBuilderExtWindows::with_additional_browser_args(builder, native::BROWSER_ARGS);
    builder
}

/// JavaScript that's true when the page's origin matches one of Chromium's origin rules.
fn origin_check(rules: &[String]) -> String {
    let checks: Vec<String> = rules
        .iter()
        .map(|r| match r.split_once("://*.") {
            Some((scheme, host)) => format!(
                "(location.protocol === {} && (location.hostname === {h} || location.hostname.endsWith('.' + {h})))",
                Value::from(format!("{scheme}:")),
                h = Value::from(host.trim_end_matches('/'))
            ),
            None => format!("location.origin === {}", Value::from(r.trim_end_matches('/'))),
        })
        .collect();
    if checks.is_empty() { "false".into() } else { checks.join(" || ") }
}

/// With NANO_WV_DEBUG set, logs a message with seconds since the first one, to see gaps between requests.
fn debug(dir: &str, text: &str) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    if std::env::var_os("NANO_WV_DEBUG").is_some() {
        let t = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
        eprintln!("[{t:9.3}] {dir} {}", truncate(text, 300));
    }
}

fn truncate(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

fn rect(width: f64, height: f64) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
        size: wry::dpi::LogicalSize::new(width, height).into(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn origin_rules_become_checks() {
        let c = super::origin_check(&["https://comix.to".into(), "https://*.example.com".into()]);
        assert!(c.contains(r#"location.origin === "https://comix.to""#));
        assert!(c.contains(r#"location.hostname.endsWith('.' + "example.com")"#));
        assert_eq!(super::origin_check(&[]), "false");
    }
}
