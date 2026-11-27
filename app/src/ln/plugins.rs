//! LNReader plugins: the repo lists, what's installed, and one running copy of each plugin.

use super::js::Js;
use super::store::Chapter;
use gpui::{Context, SharedString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

pub const DEFAULT_REPO: &str = "https://raw.githubusercontent.com/LNReader/lnreader-plugins/plugins/v3.0.0/.dist/plugins.min.json";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub site: String,
    pub lang: String,
    pub version: String,
    pub url: String,
    #[serde(rename = "iconUrl")]
    pub icon_url: String,
}

pub struct Plugins {
    dir: PathBuf,
    pub index: Vec<Entry>,
    pub installed: Vec<Entry>,
    pub busy: HashSet<String>,
    pub loading: bool,
    pub status: SharedString,
    runtimes: HashMap<String, Arc<Js>>,
}

fn download(url: &str, limit: u64) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("Addresses start with https://".into());
    }
    let mut res = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    res.body_mut().with_config().limit(limit).read_to_string().map_err(|e| e.to_string())
}

/// A list can be one array of entries, or hold it under `plugins`.
fn parse_index(text: &str) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "That isn't a plugin list".to_string())?;
    let list = if v.is_array() { v } else { v["plugins"].clone() };
    let entries: Vec<Entry> = serde_json::from_value(list).map_err(|_| "That isn't a plugin list".to_string())?;
    Ok(entries.into_iter().filter(|e| !e.id.is_empty() && e.url.starts_with("https://")).collect())
}

fn newer(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(a), Ok(b)) => a > b,
        _ => a != b,
    }
}

impl Plugins {
    pub fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let read = |name: &str| std::fs::read(dir.join(name)).ok().and_then(|b| serde_json::from_slice::<Vec<Entry>>(&b).ok()).unwrap_or_default();
        let (installed, index) = (read("installed.json"), read("index.json"));
        let _ = cx;
        Plugins { dir, index, installed, busy: HashSet::new(), loading: false, status: "".into(), runtimes: HashMap::new() }
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.installed.iter().any(|e| e.id == id)
    }

    /// The listed entry when it's newer than the installed one.
    pub fn update_for(&self, id: &str) -> Option<&Entry> {
        let have = self.installed.iter().find(|e| e.id == id)?;
        self.index.iter().find(|e| e.id == id && newer(&e.version, &have.version))
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.installed.iter().find(|e| e.id == id)
    }

    /// Fetches every repo's list and keeps the union, the first repo winning a clash.
    pub fn refresh_index(&mut self, repos: Vec<String>, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.status = "Loading the plugin list…".into();
        cx.notify();
        let dir = self.dir.clone();
        cx.spawn(async move |this, cx| {
            let (merged, errors) = cx
                .background_executor()
                .spawn(async move {
                    let (mut merged, mut errors): (Vec<Entry>, Vec<String>) = (vec![], vec![]);
                    for repo in repos {
                        match download(&repo, 8 << 20).and_then(|t| parse_index(&t)) {
                            Ok(list) => {
                                for e in list {
                                    if !merged.iter().any(|m| m.id == e.id) {
                                        merged.push(e);
                                    }
                                }
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    if errors.is_empty() {
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::fs::write(dir.join("index.json"), serde_json::to_vec(&merged).unwrap_or_default());
                    }
                    (merged, errors)
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.loading = false;
                if errors.is_empty() {
                    p.index = merged;
                    p.status = "".into();
                } else {
                    p.status = errors.join(" ").into();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join("plugins").join(format!("{}.js", crate::downloads::valid_name(id, 100)))
    }

    fn save_installed(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("installed.json"), serde_json::to_vec(&self.installed).unwrap_or_default());
    }

    pub fn install(&mut self, entry: Entry, cx: &mut Context<Self>) {
        if !self.busy.insert(entry.id.clone()) {
            return;
        }
        cx.notify();
        let path = self.file(&entry.id);
        cx.spawn(async move |this, cx| {
            let e = entry.clone();
            let saved = cx
                .background_executor()
                .spawn(async move {
                    let code = download(&e.url, 8 << 20)?;
                    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
                    std::fs::write(&path, code).map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&entry.id);
                match saved {
                    Ok(()) => {
                        p.runtimes.remove(&entry.id);
                        p.installed.retain(|i| i.id != entry.id);
                        p.installed.push(entry);
                        p.installed.sort_by_key(|i| i.name.to_lowercase());
                        p.save_installed();
                        p.status = "".into();
                    }
                    Err(e) => p.status = e.into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn uninstall(&mut self, id: &str, cx: &mut Context<Self>) {
        let _ = std::fs::remove_file(self.file(id));
        let _ = std::fs::remove_file(self.dir.join("storage").join(format!("{}.json", crate::downloads::valid_name(id, 100))));
        self.runtimes.remove(id);
        self.installed.retain(|i| i.id != id);
        self.save_installed();
        cx.notify();
    }

    /// The running plugin, started the first time it's asked for.
    pub fn source(&mut self, id: &str) -> Option<Source> {
        let entry = self.entry(id)?.clone();
        if !self.runtimes.contains_key(id) {
            let code = std::fs::read_to_string(self.file(id)).ok()?;
            let storage = self.dir.join("storage").join(format!("{}.json", crate::downloads::valid_name(id, 100)));
            self.runtimes.insert(id.to_string(), Arc::new(Js::spawn(code, storage)));
        }
        Some(Source { id: entry.id, name: entry.name, site: entry.site, js: self.runtimes[id].clone() })
    }
}

#[derive(Clone)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub site: String,
    js: Arc<Js>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub path: String,
    pub cover: String,
}

#[derive(Clone, Debug, Default)]
pub struct Details {
    pub name: String,
    pub cover: String,
    pub summary: String,
    pub author: String,
    pub status: String,
    pub genres: String,
    pub chapters: Vec<Chapter>,
}

/// Plugins return paths and sometimes covers relative to the site.
pub fn absolute(site: &str, url: &str) -> String {
    let url = url.trim();
    if url.is_empty() || url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        url.to_string()
    } else if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        let origin = site.split('/').take(3).collect::<Vec<_>>().join("/");
        format!("{}/{}", origin.trim_end_matches('/'), url.trim_start_matches('/'))
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

fn chapter(v: &Value) -> Option<Chapter> {
    Some(Chapter {
        name: text(&v["name"]),
        path: v["path"].as_str()?.to_string(),
        release: text(&v["releaseTime"]),
        number: v["chapterNumber"].as_f64(),
        page: v["page"].as_str().map(str::to_string),
    })
}

impl Source {
    fn items(&self, v: Value) -> Vec<Item> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| Some(Item { name: text(&n["name"]), path: n["path"].as_str()?.to_string(), cover: absolute(&self.site, n["cover"].as_str().unwrap_or_default()) }))
            .filter(|i| !i.name.is_empty())
            .collect()
    }

    pub async fn popular(&self, page: u32, latest: bool) -> Result<Vec<Item>, String> {
        let filters = self.js.meta().await.map(|m| m["filters"].clone()).unwrap_or(Value::Null);
        let options = json!({ "showLatestNovels": latest, "filters": if filters.is_null() { json!({}) } else { filters } });
        Ok(self.items(self.js.call("popularNovels", json!([page, options])).await?))
    }

    pub async fn search(&self, query: &str, page: u32) -> Result<Vec<Item>, String> {
        Ok(self.items(self.js.call("searchNovels", json!([query, page])).await?))
    }

    /// The novel with all its chapters, fetching the rest when the plugin pages them.
    pub async fn novel(&self, path: &str) -> Result<Details, String> {
        let v = self.js.call("parseNovel", json!([path])).await?;
        let mut chapters: Vec<Chapter> = v["chapters"].as_array().into_iter().flatten().filter_map(chapter).collect();
        let pages = v["totalPages"].as_u64().unwrap_or(1).min(300);
        for page in 2..=pages {
            match self.js.call("parsePage", json!([path, page.to_string()])).await {
                Ok(p) => chapters.extend(p["chapters"].as_array().into_iter().flatten().filter_map(chapter)),
                Err(e) => {
                    eprintln!("{} page {page}: {e}", self.name);
                    break;
                }
            }
        }
        let genres = match &v["genres"] {
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
            other => text(other),
        };
        Ok(Details {
            name: text(&v["name"]),
            cover: absolute(&self.site, v["cover"].as_str().unwrap_or_default()),
            summary: text(&v["summary"]),
            author: text(&v["author"]),
            status: text(&v["status"]),
            genres,
            chapters,
        })
    }

    pub async fn chapter(&self, path: &str) -> Result<String, String> {
        let html = self.js.call("parseChapter", json!([path])).await?;
        html.as_str().map(str::to_string).ok_or_else(|| "The plugin sent no chapter text".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_resolve_against_the_site() {
        assert_eq!(absolute("https://a.com/x/", "/c.png"), "https://a.com/c.png");
        assert_eq!(absolute("https://a.com/", "c.png"), "https://a.com/c.png");
        assert_eq!(absolute("https://a.com/", "//cdn.b.com/c.png"), "https://cdn.b.com/c.png");
        assert_eq!(absolute("https://a.com/", "https://b.com/c.png"), "https://b.com/c.png");
        assert_eq!(absolute("https://a.com/", ""), "");
    }

    #[test]
    fn index_reads_both_shapes() {
        let one = r#"[{"id":"a","name":"A","site":"https://a","lang":"English","version":"1.0.0","url":"https://a/a.js","iconUrl":"https://a/i.png"},{"id":"","url":"https://x"}]"#;
        assert_eq!(parse_index(one).unwrap().len(), 1);
        assert_eq!(parse_index(&format!(r#"{{"plugins":{one}}}"#)).unwrap().len(), 1);
        assert!(parse_index("nope").is_err());
        assert!(newer("1.2.0", "1.1.9") && !newer("1.1.9", "1.1.9"));
    }

    /// LN_LIVE=<plugin .js files, comma separated> goes through Source the way the screens do.
    #[test]
    #[ignore]
    fn live_source_end_to_end() {
        let Some(files) = std::env::var_os("LN_LIVE") else { return };
        for file in files.to_string_lossy().split(',') {
            let js = Arc::new(Js::spawn(std::fs::read_to_string(file).unwrap(), std::env::temp_dir().join("nano-ln-e2e.json")));
            let meta = futures::executor::block_on(js.meta()).unwrap();
            let src = Source { id: meta["id"].as_str().unwrap().into(), name: meta["name"].as_str().unwrap().into(), site: meta["site"].as_str().unwrap().into(), js };
            let items = match futures::executor::block_on(src.popular(1, false)) { Ok(i) => i, Err(e) => { eprintln!("{}: popular FAILED {e}", src.name); continue } };
            eprintln!("{}: {} novels, first {:?} cover {:?}", src.name, items.len(), items[0].name, items[0].cover);
            let d = match futures::executor::block_on(src.novel(&items[0].path)) { Ok(d) => d, Err(e) => { eprintln!("{}: novel FAILED {e}", src.name); continue } };
            eprintln!("{}: {:?} by {:?} [{}] {} chapters, genres {:?}, summary {} chars", src.name, d.name, d.author, d.status, d.chapters.len(), d.genres, d.summary.len());
            let Some(ch) = d.chapters.first() else { continue };
            match futures::executor::block_on(src.chapter(&ch.path)) {
                Ok(html) => {
                    let blocks = crate::ln::text::parse(&html);
                    eprintln!("{}: chapter {:?} -> {} paragraphs, first {:?}", src.name, ch.name, blocks.len(), blocks.first().map(|b| b.text.chars().take(70).collect::<String>()));
                }
                Err(e) => eprintln!("{}: chapter FAILED {e}", src.name),
            }
        }
    }
}
