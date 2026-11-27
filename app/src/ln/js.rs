//! Runs one LNReader plugin in QuickJS on a thread of its own, and answers its calls.

use futures::channel::oneshot;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Error, Function, Promise, Runtime, function::Opt};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// cheerio, htmlparser2, dayjs and the browser pieces plugins expect, bundled by `ln/js`.
const RUNTIME: &str = include_str!("../../ln/runtime.js");

/// One call may take this long before the plugin is cut off.
const LIMIT: Duration = Duration::from_secs(90);

struct Job {
    method: String,
    args: String,
    reply: oneshot::Sender<Result<String, String>>,
}

pub struct Js {
    tx: mpsc::Sender<Job>,
}

impl Js {
    /// Starts the plugin's thread. The plugin loads there, so this never waits, and a plugin that won't load fails its first call.
    pub fn spawn(code: String, storage: PathBuf) -> Js {
        let (tx, rx) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new().name("ln-plugin".into()).stack_size(16 << 20).spawn(move || run(code, storage, rx));
        if let Err(e) = spawned {
            eprintln!("ln plugin thread: {e}");
        }
        Js { tx }
    }

    /// What the plugin says about itself: its filters, settings and so on.
    pub async fn meta(&self) -> Result<Value, String> {
        self.call("__meta", Value::Null).await
    }

    pub async fn call(&self, method: &str, args: Value) -> Result<Value, String> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Job { method: method.into(), args: args.to_string(), reply }).map_err(|_| "The plugin stopped".to_string())?;
        let text = answer.await.map_err(|_| "The plugin stopped".to_string())??;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn debug() -> bool {
    std::env::var_os("NANO_LN_DEBUG").is_some()
}

fn run(code: String, storage: PathBuf, rx: mpsc::Receiver<Job>) {
    let deadline = Arc::new(AtomicU64::new(0));
    let setup = || -> Result<(Runtime, Context, String), String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(768 << 20);
        rt.set_max_stack_size(4 << 20);
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || {
            let at = d.load(Ordering::Relaxed);
            at != 0 && now_ms() > at
        })));
        let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
        let meta = ctx
            .with(|ctx| -> Result<String, String> {
                natives(&ctx, storage).map_err(|e| e.to_string())?;
                ctx.eval::<(), _>(RUNTIME).catch(&ctx).map_err(|e| e.to_string())?;
                let load: Function = ctx.globals().get("__load").map_err(|e| e.to_string())?;
                load.call::<_, String>((code,)).catch(&ctx).map_err(|e| e.to_string())
            })?;
        Ok((rt, ctx, meta))
    };
    let (_rt, ctx, meta) = match setup() {
        Ok(ok) => ok,
        Err(e) => {
            while let Ok(job) = rx.recv() {
                let _ = job.reply.send(Err(e.clone()));
            }
            return;
        }
    };
    while let Ok(job) = rx.recv() {
        if job.method == "__meta" {
            let _ = job.reply.send(Ok(meta.clone()));
            continue;
        }
        deadline.store(now_ms() + LIMIT.as_millis() as u64, Ordering::Relaxed);
        let result = ctx.with(|ctx| call(&ctx, &job.method, &job.args, &deadline));
        deadline.store(0, Ordering::Relaxed);
        let _ = job.reply.send(result);
    }
}

fn caught(ctx: &Ctx, e: Error) -> String {
    match CaughtError::from_error(ctx, e) {
        CaughtError::Exception(ex) => ex.message().unwrap_or_else(|| ex.to_string()),
        CaughtError::Value(v) => format!("{v:?}"),
        CaughtError::Error(e) => e.to_string(),
    }
}

/// Starts the plugin's method, then runs its promises and timers until it answers.
fn call(ctx: &Ctx, method: &str, args: &str, deadline: &AtomicU64) -> Result<String, String> {
    let globals = ctx.globals();
    let start: Function = globals.get("__call").map_err(|e| e.to_string())?;
    let promise: Promise = start.call((method, args)).map_err(|e| caught(ctx, e))?;
    loop {
        match promise.finish::<String>() {
            Ok(text) => return Ok(text),
            Err(Error::WouldBlock) => {
                let next: Function = globals.get("__nextTimer").map_err(|e| e.to_string())?;
                let wait: f64 = next.call(()).map_err(|e| caught(ctx, e))?;
                if wait < 0. {
                    return Err("The plugin stopped without an answer".into());
                }
                if now_ms() + wait as u64 > deadline.load(Ordering::Relaxed) {
                    return Err("The plugin took too long".into());
                }
                std::thread::sleep(Duration::from_millis(wait as u64));
                let run: Function = globals.get("__runTimer").map_err(|e| e.to_string())?;
                run.call::<_, ()>(()).map_err(|e| caught(ctx, e))?;
            }
            Err(e) => return Err(caught(ctx, e)),
        }
    }
}

fn natives(ctx: &Ctx, storage: PathBuf) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    globals.set(
        "__log",
        Function::new(ctx.clone(), |level: String, msg: String| {
            if debug() {
                eprintln!("ln {level}: {msg}");
            }
        })?,
    )?;
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(40))).build().into();
    globals.set("__fetch", Function::new(ctx.clone(), move |req: String| fetch(&agent, &req))?)?;
    let kept: Arc<Mutex<BTreeMap<String, String>>> =
        Arc::new(Mutex::new(std::fs::read(&storage).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()));
    globals.set(
        "__storage",
        Function::new(ctx.clone(), move |op: String, key: Opt<Option<String>>, value: Opt<Option<String>>| -> Option<String> {
            let mut kept = kept.lock().unwrap();
            let key = key.0.flatten().unwrap_or_default();
            let out = match op.as_str() {
                "get" => return kept.get(&key).cloned(),
                "keys" => return Some(serde_json::to_string(&kept.keys().collect::<Vec<_>>()).unwrap_or_default()),
                "set" => kept.insert(key, value.0.flatten().unwrap_or_default()),
                "delete" => kept.remove(&key),
                _ => {
                    kept.clear();
                    None
                }
            };
            if let Some(dir) = storage.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&storage, serde_json::to_vec(&*kept).unwrap_or_default());
            out
        })?,
    )?;
    Ok(())
}

/// One HTTP request for the plugin's `fetch`. Errors come back as `{"error": ...}` so JS can throw them.
fn fetch(agent: &ureq::Agent, req: &str) -> String {
    let result = || -> Result<Value, String> {
        let req: Value = serde_json::from_str(req).map_err(|e| e.to_string())?;
        let url = req["url"].as_str().ok_or("fetch needs a url")?;
        let method = req["method"].as_str().unwrap_or("GET");
        let mut builder = ureq::http::Request::builder().method(method).uri(url);
        for (k, v) in req["headers"].as_object().into_iter().flatten() {
            // ureq handles compression and length itself.
            if !matches!(k.as_str(), "accept-encoding" | "content-length" | "host") {
                builder = builder.header(k.as_str(), v.as_str().unwrap_or_default());
            }
        }
        let body = req["body"].as_str().unwrap_or_default().as_bytes().to_vec();
        let request = builder.body(body).map_err(|e| e.to_string())?;
        if debug() {
            eprintln!("ln fetch {method} {url}");
        }
        let mut res = agent.run(request).map_err(|e| format!("{url}: {e}"))?;
        let status = res.status();
        let headers: Vec<(String, String)> = res.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        let final_url = ureq::ResponseExt::get_uri(&res).to_string();
        let bytes = res.body_mut().with_config().limit(32 << 20).read_to_vec().map_err(|e| format!("{url}: {e}"))?;
        if debug() {
            eprintln!("ln fetch {status} {} bytes", bytes.len());
        }
        Ok(serde_json::json!({
            "status": status.as_u16(),
            "statusText": status.canonical_reason().unwrap_or(""),
            "url": final_url,
            "headers": headers,
            "body": String::from_utf8_lossy(&bytes),
        }))
    };
    match result() {
        Ok(v) => v.to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = r#"
        const { load } = require('cheerio');
        const { NovelStatus } = require('@libs/novelStatus');
        const dayjs = require('dayjs');
        class P {
            id = 'fake'; name = 'Fake'; version = '1.0.0'; site = 'https://example.com/'; icon = 'x.png';
            async popularNovels(page, { showLatestNovels }) {
                const $ = load('<ul><li><a href="/n/1">One &amp; Two</a></li><li><a href="/n/2">Three</a></li></ul>');
                await new Promise((r) => setTimeout(r, 20));
                return $('li a').toArray().map((a) => ({ name: $(a).text(), path: new URL($(a).attr('href'), this.site).pathname, cover: '' + page + showLatestNovels }));
            }
            async parseNovel(path) { return { path, name: 'n', status: NovelStatus.Ongoing, when: dayjs('2024-03-05').format('YYYY/MM/DD') }; }
            async boom() { throw new Error('nope'); }
        }
        exports.default = new P();
    "#;

    fn start() -> Js {
        let js = Js::spawn(FAKE.into(), std::env::temp_dir().join("nano-ln-test-storage.json"));
        assert_eq!(futures::executor::block_on(js.meta()).unwrap()["id"], "fake");
        js
    }

    #[test]
    fn a_broken_plugin_fails_its_calls() {
        let js = Js::spawn("this is not javascript".into(), std::env::temp_dir().join("nano-ln-test-storage3.json"));
        assert!(futures::executor::block_on(js.meta()).is_err());
    }

    #[test]
    fn plugin_runs_with_cheerio_timers_and_url() {
        let js = start();
        let list = futures::executor::block_on(js.call("popularNovels", serde_json::json!([2, { "showLatestNovels": true }]))).unwrap();
        assert_eq!(list[0]["name"], "One & Two");
        assert_eq!(list[1]["path"], "/n/2");
        assert_eq!(list[0]["cover"], "2true");
    }

    #[test]
    fn libs_and_errors() {
        let js = start();
        let novel = futures::executor::block_on(js.call("parseNovel", serde_json::json!(["/n/1"]))).unwrap();
        assert_eq!(novel["status"], "Ongoing");
        assert_eq!(novel["when"], "2024/03/05");
        let err = futures::executor::block_on(js.call("boom", serde_json::json!([]))).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let err = futures::executor::block_on(js.call("missing", serde_json::json!([]))).unwrap_err();
        assert!(err.contains("no missing"), "{err}");
    }

    /// LN_PLUGINS=<dir of plugin .js files> loads every one and lists those that don't start.
    #[test]
    #[ignore]
    fn real_plugins_start() {
        let Some(dir) = std::env::var_os("LN_PLUGINS") else { return };
        let (mut ok, mut bad) = (0, vec![]);
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let code = std::fs::read_to_string(e.path()).unwrap();
            let js = Js::spawn(code, std::env::temp_dir().join("nano-ln-test-storage2.json"));
            match futures::executor::block_on(js.meta()) {
                Ok(_) => ok += 1,
                Err(err) => bad.push(format!("{}: {}", e.file_name().to_string_lossy(), err.lines().next().unwrap_or(""))),
            }
        }
        eprintln!("{ok} started, {} failed", bad.len());
        for b in &bad {
            eprintln!("  {b}");
        }
        assert!(bad.is_empty());
    }

    /// LN_LIVE=<plugin .js files, comma separated> browses, opens a novel and reads a chapter on the real sites.
    #[test]
    #[ignore]
    fn live_plugins_work() {
        let Some(files) = std::env::var_os("LN_LIVE") else { return };
        let run = |js: &Js, m: &str, a: Value| futures::executor::block_on(js.call(m, a));
        for file in files.to_string_lossy().split(',') {
            let js = Js::spawn(std::fs::read_to_string(file).unwrap_or_default(), std::env::temp_dir().join("nano-ln-live.json"));
            let Ok(meta) = futures::executor::block_on(js.meta()) else { continue };
            let name = meta["name"].as_str().unwrap_or("?").to_string();
            let t = std::time::Instant::now();
            let list = match run(&js, "popularNovels", serde_json::json!([1, { "showLatestNovels": false, "filters": meta["filters"] }])) {
                Ok(l) => l,
                Err(e) => { eprintln!("{name}: popular FAILED {}", e.lines().next().unwrap_or("")); continue }
            };
            let first = &list[0];
            eprintln!("{name}: popular {} novels in {:?}, first {:?} {:?}", list.as_array().map_or(0, |a| a.len()), t.elapsed(), first["name"], first["path"]);
            let novel = match run(&js, "parseNovel", serde_json::json!([first["path"]])) {
                Ok(n) => n,
                Err(e) => { eprintln!("{name}: parseNovel FAILED {}", e.lines().next().unwrap_or("")); continue }
            };
            let chapters = novel["chapters"].as_array().cloned().unwrap_or_default();
            eprintln!("{name}: novel {:?} by {:?}, status {:?}, {} chapters, summary {} chars", novel["name"], novel["author"], novel["status"], chapters.len(), novel["summary"].as_str().map_or(0, str::len));
            let Some(ch) = chapters.first() else { continue };
            match run(&js, "parseChapter", serde_json::json!([ch["path"]])) {
                Ok(html) => eprintln!("{name}: chapter {:?} is {} chars: {:?}", ch["name"], html.as_str().map_or(0, str::len), html.as_str().unwrap_or("").chars().take(80).collect::<String>()),
                Err(e) => eprintln!("{name}: parseChapter FAILED {}", e.lines().next().unwrap_or("")),
            }
            match run(&js, "searchNovels", serde_json::json!(["dragon", 1])) {
                Ok(r) => eprintln!("{name}: search {} results", r.as_array().map_or(0, |a| a.len())),
                Err(e) => eprintln!("{name}: search FAILED {}", e.lines().next().unwrap_or("")),
            }
        }
    }
}
