//! Requests made from a hidden web view, for sites that turn away plain HTTP even with the cookies.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::Window;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, mpsc};
use std::time::Duration;

type Reply = Result<Value, String>;

static ROUTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);
static QUEUE: OnceLock<UnboundedSender<Job>> = OnceLock::new();
static PENDING: LazyLock<Mutex<HashMap<u64, mpsc::Sender<Reply>>>> = LazyLock::new(Mutex::default);
static NEXT: AtomicU64 = AtomicU64::new(1);

pub struct Job {
    pub id: u64,
    pub origin: String,
    pub script: String,
    /// Load this page in the view instead of running the script.
    pub navigate: Option<String>,
}

/// The scheme and host of an address, like `https://example.com`.
fn origin_of(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    format!("{scheme}://{}", rest.split(['/', '?', '#']).next().unwrap_or(""))
}

/// From now on this site's requests go through the web view.
pub fn route(url: &str) {
    ROUTED.lock().unwrap().insert(super::js::site_key(url));
}

pub fn routed(url: &str) -> bool {
    ROUTED.lock().unwrap().contains(&super::js::site_key(url))
}

/// The queue the window drains. Call once.
pub fn queue() -> UnboundedReceiver<Job> {
    let (tx, rx) = unbounded();
    let _ = QUEUE.set(tx);
    rx
}

/// Called with what the page posted back.
pub fn answer(message: &str) {
    let Ok(m) = serde_json::from_str::<Value>(message) else { return };
    let Some(id) = m["id"].as_u64() else { return };
    let Some(tx) = PENDING.lock().unwrap().remove(&id) else { return };
    let _ = tx.send(match m["error"].as_str() {
        Some(e) => Err(e.to_string()),
        None => Ok(m),
    });
}

pub fn fail(id: u64, error: &str) {
    answer(&json!({ "id": id, "error": error }).to_string());
}

/// Makes the request in the page and waits for it. Runs on a plugin's thread.
pub fn fetch(req: &Value) -> Reply {
    let url = req["url"].as_str().ok_or("fetch needs a url")?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let headers: Vec<(&String, &Value)> = req["headers"]
        .as_object()
        .into_iter()
        .flatten()
        // The page sets these itself, and refuses to be told.
        .filter(|(k, _)| !matches!(k.as_str(), "user-agent" | "cookie" | "host" | "accept-encoding" | "content-length" | "referer" | "origin" | "connection") && !k.starts_with("sec-"))
        .collect();
    let init = json!({ "method": req["method"], "headers": headers.into_iter().collect::<HashMap<_, _>>(), "body": req["body"].as_str().filter(|b| !b.is_empty()), "credentials": "include" });
    let script = format!(
        "(async () => {{ const post = (m) => (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify(Object.assign({{ id: {id} }}, m))); \
         try {{ const init = {init}; if (init.body === null) delete init.body; const r = await fetch({url}, init); const body = await r.text(); const headers = []; r.headers.forEach((v, k) => headers.push([k, v])); \
         post({{ status: r.status, statusText: r.statusText, url: r.url, headers, body }}); }} catch (e) {{ post({{ error: String(e) }}); }} }})()",
        url = Value::from(url),
    );
    let (tx, rx) = mpsc::channel();
    PENDING.lock().unwrap().insert(id, tx);
    send(Job { id, origin: origin_of(url), script, navigate: None })?;
    let reply = rx.recv_timeout(Duration::from_secs(75)).unwrap_or_else(|_| {
        PENDING.lock().unwrap().remove(&id);
        Err("The page took too long to answer".into())
    });
    // Some pages turn away a script's request but let a page load through.
    let get = req["method"].as_str().is_none_or(|m| m.eq_ignore_ascii_case("GET"));
    match reply {
        Ok(r) if get && challenged(&r) => {
            if std::env::var_os("NANO_LN_DEBUG").is_some() {
                eprintln!("ln fetch {url} was challenged in the page, loading it instead");
            }
            navigate(url)
        }
        other => other,
    }
}

fn send(job: Job) -> Result<(), String> {
    let id = job.id;
    QUEUE.get().ok_or("The browser isn't ready")?.unbounded_send(job).map_err(|_| {
        PENDING.lock().unwrap().remove(&id);
        "The browser isn't ready".to_string()
    })
}

fn challenged(r: &Value) -> bool {
    let headers: Vec<(String, String)> = r["headers"].as_array().into_iter().flatten().filter_map(|h| Some((h[0].as_str()?.to_string(), h[1].as_str()?.to_string()))).collect();
    super::js::is_challenge(r["status"].as_u64().unwrap_or(0) as u16, &headers, r["body"].as_str().unwrap_or("").as_bytes())
}

/// Asks the page where it is and what it shows. None if it didn't answer in time.
fn look(origin: &str) -> Option<Value> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let script = format!(
        "(() => (window.webkit ? window.webkit.messageHandlers.ipc : window.ipc).postMessage(JSON.stringify({{ id: {id}, title: document.title, ready: document.readyState, href: location.href, \
         html: document.readyState === 'complete' ? document.documentElement.outerHTML : '' }})))()"
    );
    let (tx, rx) = mpsc::channel();
    PENDING.lock().unwrap().insert(id, tx);
    if send(Job { id, origin: origin.to_string(), script, navigate: None }).is_err() {
        return None;
    }
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(m)) => Some(m),
        _ => {
            PENDING.lock().unwrap().remove(&id);
            None
        }
    }
}

fn same_page(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.split('#').next().unwrap_or("").trim_end_matches('/').to_string();
    trim(a) == trim(b)
}

/// Loads the page in the view and waits for any check on it to finish, then answers with what the page shows.
fn navigate(url: &str) -> Reply {
    let origin = origin_of(url);
    let before = look(&origin).and_then(|m| m["href"].as_str().map(str::to_string)).unwrap_or_default();
    send(Job { id: 0, origin: origin.clone(), script: String::new(), navigate: Some(url.to_string()) })?;
    let started = std::time::Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < Duration::from_secs(45) {
        std::thread::sleep(Duration::from_millis(800));
        let Some(m) = look(&origin) else { continue };
        let title = m["title"].as_str().unwrap_or("").to_lowercase();
        let href = m["href"].as_str().unwrap_or("").to_string();
        let html = m["html"].as_str().unwrap_or("");
        let checking = title.contains("just a moment") || title.contains("attention required");
        // The old page can still be showing for a moment after the load starts.
        let moved = !same_page(&href, &before) || same_page(&href, url) && started.elapsed() > Duration::from_millis(2500);
        if m["ready"] == "complete" && !checking && !html.is_empty() && moved {
            return Ok(json!({ "status": 200, "statusText": "OK", "url": href, "headers": [], "body": html }));
        }
        last = m;
    }
    Ok(json!({ "status": 403, "statusText": "Forbidden", "url": url, "headers": [["cf-mitigated", "challenge"]], "body": last["html"].as_str().unwrap_or("") }))
}

/// One hidden web view per site, kept on the site's own page.
#[derive(Default)]
pub struct Views(HashMap<String, (wry::WebView, Arc<AtomicBool>)>);

impl Views {
    /// The flag turns true once the page has loaded.
    pub fn ensure(&mut self, origin: &str, window: &mut Window) -> Option<Arc<AtomicBool>> {
        if let Some((_, loaded)) = self.0.get(origin) {
            return Some(loaded.clone());
        }
        let loaded = Arc::new(AtomicBool::new(false));
        let flag = loaded.clone();
        let mut builder = crate::webviews::builder()
            .with_url(format!("{origin}/"))
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(-10000., 0.).into(),
                size: wry::dpi::LogicalSize::new(1000., 700.).into(),
            })
            .with_ipc_handler(|req| answer(req.body()))
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    flag.store(true, Ordering::Relaxed);
                }
            });
        // The check was passed with this agent, so the cookies only work with it.
        let agent = crate::challenge::browser_user_agent(String::new());
        if !agent.is_empty() {
            builder = builder.with_user_agent(&agent);
        }
        match builder.build_as_child(window) {
            Ok(view) => {
                crate::webviews::native::keep_running_hidden(&view);
                self.0.insert(origin.to_string(), (view, loaded.clone()));
                Some(loaded)
            }
            Err(e) => {
                eprintln!("system webview unavailable: {e}");
                None
            }
        }
    }

    pub fn go(&self, origin: &str, url: &str) {
        if let Some((view, _)) = self.0.get(origin) {
            let _ = view.load_url(url);
        }
    }

    pub fn run(&self, origin: &str, script: &str) {
        if let Some((view, _)) = self.0.get(origin) {
            let _ = view.evaluate_script(script);
        }
    }
}
