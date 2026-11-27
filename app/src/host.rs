//! Talks to the JVM extension host over JSON lines on stdin and stdout.

use futures::channel::oneshot;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum HostError {
    /// Cloudflare wants a check. Solve it in a webview with exactly this user agent.
    Challenge { url: String, user_agent: String },
    Failed(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HostError::Challenge { url, .. } => write!(f, "Cloudflare challenge at {url}"),
            HostError::Failed(e) => f.write_str(e),
        }
    }
}

type Reply = Result<Value, HostError>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

pub struct Host {
    stdin: Mutex<ChildStdin>,
    /// Requests from extensions that drive a WebView.
    webview_requests: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Value>>>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Child>,
}

impl Host {
    pub fn spawn(bin: &Path, data_dir: &Path, heap_mb: u32) -> std::io::Result<Arc<Host>> {
        let mut cmd = Command::new(bin);
        // A packaged build carries its own trimmed Java runtime beside bin/, so users needn't install Java.
        if let Some(jre) = bin.parent().and_then(Path::parent).map(|d| d.join("jre")).filter(|j| j.exists()) {
            cmd.env("JAVA_HOME", jre);
        }
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        let mut child = cmd
            .arg(data_dir)
            // The launcher adds these after its own defaults, so this -Xmx beats the built-in 64m.
            .env("YOMI_HOST_OPTS", format!("-Xmx{heap_mb}m"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped, not inherited: with no console of its own on Windows, the host's errors would go nowhere.
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            // Bytes, since Java on Windows may not write UTF-8, and stopping early would leave the host blocked on a full pipe.
            let (mut r, mut line) = (BufReader::new(stderr), Vec::new());
            while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                eprint!("{}", String::from_utf8_lossy(&line));
                line.clear();
            }
        });
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let pending: Pending = Arc::default();

        let reader_pending = pending.clone();
        let (wv_tx, wv_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if msg.get("wv").is_some() {
                    let _ = wv_tx.unbounded_send(msg);
                    continue;
                }
                let Some(id) = msg["id"].as_u64() else { continue };
                if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(parse_reply(msg));
                }
            }
            eprintln!("Extension host stopped. Anything it printed is above.");
            // The host died, so fail whatever's still waiting.
            for (_, tx) in reader_pending.lock().unwrap().drain() {
                let _ = tx.send(Err(HostError::Failed("extension host exited".into())));
            }
        });

        Ok(Arc::new(Host {
            stdin: Mutex::new(stdin),
            webview_requests: Mutex::new(Some(wv_rx)),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
        }))
    }

    /// Sends a request and waits for its reply. Safe to call from several places at once.
    pub async fn call(&self, op: &str, mut params: Value) -> Reply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        params["id"] = id.into();
        params["op"] = op.into();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let written = {
            let mut w = self.stdin.lock().unwrap();
            writeln!(w, "{params}").and_then(|_| w.flush())
        };
        if let Err(e) = written {
            self.pending.lock().unwrap().remove(&id);
            return Err(HostError::Failed(format!("extension host is not running: {e}")));
        }
        rx.await.unwrap_or_else(|_| Err(HostError::Failed("extension host dropped the request".into())))
    }

    /// Only the first caller gets the stream.
    pub fn take_webview_requests(&self) -> Option<futures::channel::mpsc::UnboundedReceiver<Value>> {
        self.webview_requests.lock().unwrap().take()
    }

    pub fn notify(&self, event: &Value) {
        let mut w = self.stdin.lock().unwrap();
        let _ = writeln!(w, "{event}").and_then(|_| w.flush());
    }

    pub async fn set_cookies(&self, url: &str, cookies: Vec<String>) -> Reply {
        self.call("setCookies", json!({ "url": url, "cookies": cookies })).await
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

fn parse_reply(msg: Value) -> Reply {
    if let Some(c) = msg.get("challenge") {
        return Err(HostError::Challenge {
            url: c["url"].as_str().unwrap_or_default().into(),
            user_agent: c["userAgent"].as_str().unwrap_or_default().into(),
        });
    }
    match msg.get("error") {
        Some(e) => Err(HostError::Failed(e.as_str().unwrap_or("unknown host error").into())),
        None => Ok(msg["result"].clone()),
    }
}

/// Settings uses this to say when a change needs a restart.
pub static LAUNCH_HEAP_MB: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

pub const HEAP_CHOICES_MB: [u32; 4] = [64, 128, 256, 512];

/// About 1/128 of the machine's memory, kept between 64 and 256 MB.
pub fn auto_heap_mb() -> u32 {
    static CACHED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        heap_for(sys.total_memory())
    })
}

fn heap_for(total_bytes: u64) -> u32 {
    ((total_bytes / 128 / 1024 / 1024) as u32).clamp(64, 256)
}

#[cfg(test)]
#[test]
fn heap_scales_with_memory() {
    let gb = 1024 * 1024 * 1024;
    assert_eq!(heap_for(4 * gb), 64);
    assert_eq!(heap_for(16 * gb), 128);
    assert_eq!(heap_for(64 * gb), 256);
}

/// $NANO_HOST, then a `host` folder beside the app (a packaged build), then the dev build next to this crate.
pub fn host_bin() -> PathBuf {
    let name = if cfg!(windows) { "yomi-host.bat" } else { "yomi-host" };
    if let Some(p) = std::env::var_os("NANO_HOST") {
        return p.into();
    }
    let packaged = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("host/bin").join(name)));
    packaged.filter(|p| p.exists()).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../host/build/install/yomi-host/bin").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_parsed() {
        assert_eq!(parse_reply(json!({"id":1,"result":[1,2]})).unwrap(), json!([1, 2]));
        assert!(matches!(parse_reply(json!({"id":1,"error":"boom"})), Err(HostError::Failed(e)) if e == "boom"));
        assert!(matches!(
            parse_reply(json!({"id":1,"error":"x","challenge":{"url":"https://a/","userAgent":"UA"}})),
            Err(HostError::Challenge { url, user_agent }) if url == "https://a/" && user_agent == "UA"
        ));
    }

    #[test]
    fn round_trip_with_real_host() {
        let bin = host_bin();
        if !bin.exists() {
            eprintln!("skipping: {} not built", bin.display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("yomi-host-test-{}", std::process::id()));
        let host = Host::spawn(&bin, &dir, 64).unwrap();
        let ok = futures::executor::block_on(host.set_cookies("https://example.com/", vec!["a=1; Path=/".into()]));
        assert_eq!(ok.unwrap(), json!(true));
        let bad = futures::executor::block_on(host.call("nope", json!({})));
        assert!(matches!(bad, Err(HostError::Failed(e)) if e.contains("unknown op")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
