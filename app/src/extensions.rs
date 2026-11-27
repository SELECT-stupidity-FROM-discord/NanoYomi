//! Extensions from the repos you add. A repo's signing key is pinned when you add it, and every jar is checked against it.
//! Everything here blocks, so run it off the main thread.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Extensions installed before repos could be added came from keiyoushi, signed with this.
const OLD_KEY: &str = "9add655a78e96c4ec7a53ef89dccb557cb5d767489fac5e785d671a5a75d4da2";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Repo {
    /// The repo's index.json.
    pub url: String,
    pub name: String,
    /// SHA-256 of the signing certificate, as listed when the repo was added.
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct Ext {
    pub pkg: String,
    pub name: String,
    pub version: String,
    /// "all" for multi-language extensions.
    pub langs: Vec<String>,
    pub nsfw: bool,
    /// Some of its sources are 18+, some aren't.
    pub mixed: bool,
    /// Ids of its sources, which is how a series saved from one finds it after the extension is removed.
    pub source_ids: Vec<String>,
    pub sites: Vec<String>,
    pub site: Option<String>,
    pub icon_url: String,
    jar_url: String,
    /// The signing key of the repo it came from.
    pub key: String,
}

impl Ext {
    /// An installed extension that none of your repos lists any more.
    pub fn orphan(pkg: &str, name: &str, version: &str, langs: Vec<String>) -> Ext {
        Ext { pkg: pkg.into(), name: name.into(), version: version.into(), langs, nsfw: false, mixed: false, source_ids: vec![], sites: vec![], site: None, icon_url: String::new(), jar_url: String::new(), key: String::new() }
    }
}

impl Ext {
    pub fn rows(&self) -> Vec<Ext> {
        if self.sites.is_empty() {
            return vec![self.clone()];
        }
        self.sites.iter().map(|s| Ext { name: s.clone(), site: Some(s.clone()), ..self.clone() }).collect()
    }
}

pub fn site_of(name: &str) -> String {
    match name.rsplit_once(" (") {
        Some((base, n)) if n.strip_suffix(')').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) => base.to_string(),
        _ => name.to_string(),
    }
}

fn split_sites(sources: &[(String, String)]) -> Vec<String> {
    let mut sites: Vec<String> = vec![];
    let mut split = false;
    for (lang, name) in sources {
        let site = site_of(name);
        split |= sources.iter().any(|(l, n)| l == lang && site_of(n) != site);
        if !sites.contains(&site) {
            sites.push(site);
        }
    }
    if split { sites } else { vec![] }
}

/// Extension icons come from a public CDN, not through a source.
pub fn fetch_icon(url: &str) -> Result<Vec<u8>, String> {
    let mut res = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    res.body_mut().with_config().limit(1024 * 1024).read_to_vec().map_err(|e| e.to_string())
}

/// Accepts index.min.json, repo.json or just the repo folder, since people paste any of them.
fn index_url(input: &str) -> String {
    let url = input.trim().trim_end_matches('/');
    let base = match url.rsplit_once('/') {
        Some((base, file)) if file.ends_with(".json") || file.ends_with(".pb") => base,
        _ => url,
    };
    format!("{base}/index.json")
}

/// Reads a repo for the first time and remembers the key it's signed with.
pub fn add_repo(input: &str) -> Result<Repo, String> {
    let url = index_url(input);
    if !url.starts_with("https://") {
        return Err("Repo addresses start with https://".into());
    }
    let text = get(&url)?.into_string()?;
    let index: Value = serde_json::from_str(&text).map_err(|_| format!("{url} isn't a repo index"))?;
    let key = index["signingKey"].as_str().filter(|k| k.len() == 64).ok_or("This repo doesn't list a signing key")?.to_lowercase();
    let name = index["name"].as_str().unwrap_or("Repo").to_string();
    let list = parse_index(&text, &key)?;
    if list.is_empty() {
        return Err("This repo has no desktop (.jar) builds, so Nano can't use its extensions".into());
    }
    Ok(Repo { url, name, key })
}

/// Every repo's extensions, plus a line for each repo that failed.
pub fn fetch_index(repos: &[Repo], data_dir: &Path) -> (Vec<Ext>, Vec<String>) {
    let mut list = vec![];
    let mut errors = vec![];
    for repo in repos {
        let fetched = get(&repo.url).and_then(Body::into_string).and_then(|text| {
            let exts = parse_index(&text, &repo.key)?;
            save_index(&repo.url, &exts);
            Ok(exts)
        });
        match fetched {
            Ok(exts) => list.extend(exts),
            Err(e) => {
                errors.push(format!("{}: {e}", repo.name));
                list.extend(cached(repo, data_dir));
            }
        }
    }
    list.sort_by_key(|e| e.name.to_lowercase());
    (list, errors)
}

/// Lists in the cache database are one string with a line each.
fn lines(v: &[String]) -> String {
    v.join("\n")
}

fn unlines(s: String) -> Vec<String> {
    if s.is_empty() { vec![] } else { s.split('\n').map(str::to_string).collect() }
}

/// Remembers what a repo listed, in place of the last list.
fn save_index(url: &str, exts: &[Ext]) {
    crate::cache_db::with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("DELETE FROM repo_exts WHERE repo = ?1", [url])?;
        let mut put = tx.prepare(
            "INSERT INTO repo_exts(repo, pos, pkg, name, version, langs, nsfw, mixed, source_ids, sites, site, icon_url, jar_url, key) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for (i, e) in exts.iter().enumerate() {
            put.execute(rusqlite::params![url, i as i64, e.pkg, e.name, e.version, lines(&e.langs), e.nsfw, e.mixed, lines(&e.source_ids), lines(&e.sites), e.site, e.icon_url, e.jar_url, e.key])?;
        }
        drop(put);
        tx.commit()
    });
}

/// What a repo listed last time, only if it was listed under the key the repo has now.
fn load_index(url: &str, key: &str) -> Vec<Ext> {
    crate::cache_db::with(|c| {
        let mut q = c.prepare("SELECT pkg, name, version, langs, nsfw, mixed, source_ids, sites, site, icon_url, jar_url, key FROM repo_exts WHERE repo = ?1 AND key = ?2 ORDER BY pos")?;
        q.query_map([url, key], |r| {
            Ok(Ext {
                pkg: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                langs: unlines(r.get(3)?),
                nsfw: r.get(4)?,
                mixed: r.get(5)?,
                source_ids: unlines(r.get(6)?),
                sites: unlines(r.get(7)?),
                site: r.get(8)?,
                icon_url: r.get(9)?,
                jar_url: r.get(10)?,
                key: r.get(11)?,
            })
        })?
        .collect()
    })
    .unwrap_or_default()
}

/// The saved list, or the file older versions kept in its place, which is moved into the database the first time it's read.
fn cached(repo: &Repo, data_dir: &Path) -> Vec<Ext> {
    let saved = load_index(&repo.url, &repo.key);
    if !saved.is_empty() {
        return saved;
    }
    let file = legacy_file(data_dir, repo);
    let Some(exts) = std::fs::read_to_string(&file).ok().and_then(|t| parse_index(&t, &repo.key).ok()) else { return vec![] };
    save_index(&repo.url, &exts);
    let _ = std::fs::remove_file(file);
    exts
}

fn legacy_file(data_dir: &Path, repo: &Repo) -> PathBuf {
    let hash = crate::fnv(&repo.url);
    data_dir.join("repos").join(format!("{hash:016x}.json"))
}

pub fn cached_index(repos: &[Repo], data_dir: &Path) -> Vec<Ext> {
    let mut list: Vec<Ext> = repos.iter().flat_map(|r| cached(r, data_dir)).collect();
    list.sort_by_key(|e| e.name.to_lowercase());
    list
}

/// Extensions without a desktop build are left out, since the host only loads jars.
fn parse_index(text: &str, key: &str) -> Result<Vec<Ext>, String> {
    let index: Value = serde_json::from_str(text).map_err(|e| format!("repo index: {e}"))?;
    check_key(index["signingKey"].as_str(), key)?;
    let list: Vec<Ext> = index["extensionList"]["extensions"]
        .as_array()
        .ok_or("repo index has no extensions")?
        .iter()
        .filter_map(|e| {
            let mut langs: Vec<String> =
                e["sources"].as_array()?.iter().filter_map(|s| s["language"].as_str().map(str::to_string)).collect();
            let pairs: Vec<(String, String)> = e["sources"]
                .as_array()?
                .iter()
                .filter_map(|s| Some((s["language"].as_str()?.to_string(), s["name"].as_str()?.to_string())))
                .collect();
            langs.sort();
            langs.dedup();
            if langs.len() > 3 {
                langs = vec!["all".into()];
            }
            Some(Ext {
                pkg: e["packageName"].as_str()?.to_string(),
                name: e["name"].as_str()?.trim_start_matches("Tachiyomi: ").to_string(),
                version: e["versionName"].as_str().unwrap_or("0").to_string(),
                langs,
                nsfw: e["contentWarning"] == "CONTENT_WARNING_NSFW",
                mixed: e["contentWarning"] == "CONTENT_WARNING_MIXED",
                source_ids: e["sources"].as_array()?.iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect(),
                sites: split_sites(&pairs),
                site: None,
                icon_url: e["resources"]["iconUrl"].as_str().unwrap_or_default().to_string(),
                jar_url: e["resources"]["jarUrl"].as_str()?.to_string(),
                key: key.to_string(),
            })
        })
        .collect();
    Ok(list)
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("extensions")
}

/// Downloads the jar unless this version is already here, and removes older versions.
pub fn install(ext: &Ext, data_dir: &Path) -> Result<PathBuf, String> {
    let dir = dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let jar = dir.join(format!("{}-{}.jar", ext.pkg, ext.version));
    if !jar.exists() {
        let bytes = get(&ext.jar_url)?.into_bytes()?;
        let tmp = jar.with_extension("part");
        std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &jar)).map_err(|e| e.to_string())?;
    }
    std::fs::write(jar.with_extension("key"), &ext.key).map_err(|e| e.to_string())?;
    for (pkg, other) in installed(data_dir) {
        if pkg == ext.pkg && other != jar {
            remove(&other);
        }
    }
    Ok(jar)
}

fn remove(jar: &Path) {
    let _ = std::fs::remove_file(jar);
    let _ = std::fs::remove_file(jar.with_extension("key"));
}

/// The key a jar must be signed with: the one its repo had when it was installed.
pub fn key_for(jar: &Path) -> String {
    std::fs::read_to_string(jar.with_extension("key")).map(|k| k.trim().to_lowercase()).unwrap_or_else(|_| OLD_KEY.into())
}

/// The version in `<package>-<version>.jar`.
pub fn version_of(jar: &Path) -> String {
    jar.file_stem().and_then(|f| f.to_str()).and_then(|f| f.rsplit_once('-')).map_or(String::new(), |(_, v)| v.to_string())
}

pub fn uninstall(pkg: &str, data_dir: &Path) {
    for (p, jar) in installed(data_dir) {
        if p == pkg {
            remove(&jar);
        }
    }
}

/// Installed jars, named `<package>-<version>.jar`.
pub fn installed(data_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir(data_dir)) else { return vec![] };
    let mut list: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| Some((package_of(p.file_name()?.to_str()?)?, p)))
        .collect();
    list.sort();
    list
}

fn package_of(file: &str) -> Option<String> {
    Some(file.strip_suffix(".jar")?.rsplit_once('-')?.0.to_string())
}

/// A key other than the pinned one means the repo changed hands or was tampered with.
fn check_key(listed: Option<&str>, pinned: &str) -> Result<(), String> {
    match listed {
        Some(k) if k.eq_ignore_ascii_case(pinned) => Ok(()),
        Some(k) => Err(format!(
            "this repo now lists a different signing key ({k}), so nothing from it will be installed. \
             It may have changed hands or been tampered with. Remove and re-add it only if you trust the new key."
        )),
        None => Err("repo index has no signingKey".into()),
    }
}

fn get(url: &str) -> Result<Body, String> {
    ureq::get(url).call().map(Body).map_err(|e| format!("{url}: {e}"))
}

struct Body(ureq::http::Response<ureq::Body>);

impl Body {
    fn into_bytes(mut self) -> Result<Vec<u8>, String> {
        self.0.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
    }
    fn into_string(mut self) -> Result<String, String> {
        self.0.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> Repo {
        Repo { url: "https://r.example/index.json".into(), name: "R".into(), key: "ab".repeat(32) }
    }

    /// Spelled out with no `..` so a new field stops this compiling until it's saved in `save_index` and read in `load_index`.
    fn ext(n: usize) -> Ext {
        Ext {
            pkg: format!("pkg.{n}"),
            name: format!("Ext {n}"),
            version: "1.4.2".into(),
            langs: vec!["en".into(), "ja".into()],
            nsfw: true,
            mixed: true,
            source_ids: vec!["11".into(), "22".into()],
            sites: vec!["a.example".into(), "b.example".into()],
            site: Some("a.example".into()),
            icon_url: "https://i.example/icon.png".into(),
            jar_url: "https://j.example/ext.jar".into(),
            key: repo().key,
        }
    }

    #[test]
    fn a_saved_list_reads_back_whole_and_in_order() {
        crate::cache_db::open(&std::env::temp_dir().join(format!("nano-cache-{}-exts.db", std::process::id())));
        let list = vec![ext(2), ext(1), Ext { langs: vec![], source_ids: vec![], sites: vec![], site: None, nsfw: false, mixed: false, ..ext(3) }];
        save_index(&repo().url, &list);
        let back = load_index(&repo().url, &repo().key);
        assert_eq!(format!("{back:?}"), format!("{list:?}"), "a field was not saved, add it to save_index and load_index");
        save_index(&repo().url, &list[..1]);
        assert_eq!(load_index(&repo().url, &repo().key).len(), 1, "a new list replaces the old one");
        assert!(load_index(&repo().url, &"cd".repeat(32)).is_empty(), "a list saved under another key isn't trusted");
    }

    #[test]
    fn the_file_older_versions_kept_is_moved_into_the_database() {
        let dir = std::env::temp_dir().join(format!("nano-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = Repo { url: "https://legacy.example/index.json".into(), ..repo() };
        std::fs::create_dir_all(dir.join("repos")).unwrap();
        let index = serde_json::json!({ "signingKey": repo.key, "extensionList": { "extensions": [{
            "packageName": "pkg.old", "name": "Tachiyomi: Old", "versionName": "1.0",
            "sources": [{ "id": "5", "language": "en", "name": "Old" }], "resources": { "jarUrl": "https://j.example/old.jar" } }] } });
        let file = legacy_file(&dir, &repo);
        std::fs::write(&file, index.to_string()).unwrap();
        crate::cache_db::open(&std::env::temp_dir().join(format!("nano-cache-{}-exts.db", std::process::id())));
        assert_eq!(cached(&repo, &dir).iter().map(|e| e.pkg.as_str()).collect::<Vec<_>>(), ["pkg.old"]);
        assert!(!file.exists(), "the old file is gone once it's imported");
        assert_eq!(cached(&repo, &dir).len(), 1, "and the list now comes from the database");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_pinned_key_is_accepted() {
        assert!(check_key(Some(OLD_KEY), OLD_KEY).is_ok());
        assert!(check_key(Some(&OLD_KEY.to_uppercase()), OLD_KEY).is_ok());
        assert!(check_key(Some("00".repeat(32).as_str()), OLD_KEY).unwrap_err().contains("different signing key"));
        assert!(check_key(None, OLD_KEY).is_err());
    }

    #[test]
    fn any_file_in_a_repo_folder_points_at_its_index() {
        let want = "https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.json";
        for input in [
            "https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json",
            "https://raw.githubusercontent.com/keiyoushi/extensions/repo/repo.json",
            "https://raw.githubusercontent.com/keiyoushi/extensions/repo/",
            " https://raw.githubusercontent.com/keiyoushi/extensions/repo ",
        ] {
            assert_eq!(index_url(input), want);
        }
    }

    #[test]
    fn parses_the_index_and_jar_names() {
        let index = serde_json::json!({
            "signingKey": OLD_KEY,
            "extensionList": { "extensions": [
                { "name": "Tachiyomi: Zeta", "packageName": "a.zeta", "versionName": "1.4.2",
                  "contentWarning": "CONTENT_WARNING_NSFW", "resources": { "jarUrl": "https://x/z.jar" },
                  "sources": [{ "language": "en", "id": "11" }, { "language": "es" }, { "language": "en" }] },
                { "name": "Alpha", "packageName": "a.alpha", "versionName": "1.0",
                  "contentWarning": "CONTENT_WARNING_MIXED", "resources": { "jarUrl": "https://x/a.jar" },
                  "sources": [{ "language": "fr" }, { "language": "de" }, { "language": "it" }, { "language": "pt" }] }
            ]}
        });
        let mut list = parse_index(&index.to_string(), OLD_KEY).unwrap();
        list.sort_by_key(|e| e.name.to_lowercase());
        assert_eq!(list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Alpha", "Zeta"]);
        assert_eq!(list[0].langs, ["all"]);
        assert_eq!(list[1].langs, ["en", "es"]);
        assert_eq!(list[1].source_ids, ["11"]);
        assert!(list[1].nsfw && !list[0].nsfw && list[0].mixed && !list[1].mixed);
        assert_eq!(package_of("eu.kanade.tachiyomi.extension.all.mangadex-1.6.4.jar").unwrap(), "eu.kanade.tachiyomi.extension.all.mangadex");
        assert_eq!(package_of("x.part"), None);
    }

    #[test]
    fn splits_sites_sharing_a_jar() {
        let src = |v: &[(&str, &str)]| v.iter().map(|(l, n)| (l.to_string(), n.to_string())).collect::<Vec<_>>();
        assert_eq!(split_sites(&src(&[("en", "LewdManhwa"), ("en", "Toonx"), ("en", "Toonz")])), ["LewdManhwa", "Toonx", "Toonz"]);
        assert!(split_sites(&src(&[("all", "Komga"), ("all", "Komga (2)"), ("all", "Komga (3)")])).is_empty());
        assert!(split_sites(&src(&[("en", "MangaToon"), ("es", "MangaToon"), ("fr", "MangaToon")])).is_empty());
        assert!(split_sites(&src(&[("en", "Site"), ("es", "Sitio")])).is_empty());
        assert_eq!(site_of("Shadow Manga (+18)"), "Shadow Manga (+18)");
    }

    #[test]
    #[ignore = "needs the network"]
    fn adds_keiyoushi_from_its_mihon_address() {
        let repo = add_repo("https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json").unwrap();
        assert_eq!(repo.key, OLD_KEY);
        let dir = std::env::temp_dir().join("nano-repo-test");
        let (list, errors) = fetch_index(&[repo], &dir);
        assert!(errors.is_empty() && list.len() > 100);
    }
}
