//! Looks for a newer Nano release on GitHub and shows a banner linking to it.

use crate::store::{Store, now_ms};
use crate::{ACCENT, ACCENT_TEXT, FAINT, LINE, MUTED, SURFACE, TEXT, u};
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px, rgb,
};
use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/SELECT-stupidity-FROM-discord/NanoYomi/releases/latest";
const RELEASES: &str = "https://github.com/SELECT-stupidity-FROM-discord/NanoYomi/";
const EVERY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone)]
pub struct Update {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

/// Only release builds know their version, so local builds never check.
pub fn current() -> Option<Version> {
    option_env!("NANO_VERSION").and_then(parse)
}

fn parse(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

/// Matches the file names the release workflow uploads.
fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", _) => "macos-x64",
        ("windows", "aarch64") => "windows-arm64",
        ("windows", _) => "windows-x64",
        (_, "aarch64") => "linux-arm64",
        _ => "linux-x64",
    }
}

/// The release, when it's newer than this build and already has a download for this machine.
fn newer(release: &Release, current: &Version, platform: &str) -> Option<Update> {
    let latest = parse(&release.tag_name)?;
    let file = format!("nano-{platform}.");
    let ready = release.assets.iter().any(|a| a.name.starts_with(&file));
    (latest > *current && ready && release.html_url.starts_with(RELEASES)).then(|| Update { version: latest.to_string(), url: release.html_url.clone() })
}

fn fetch() -> Result<Release, String> {
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build());
    let mut res = agent.get(LATEST).header("User-Agent", "nano-reader").header("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?;
    let text = res.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Checks now when asked to, otherwise once a day and only when it's switched on.
pub fn check(store: Entity<Store>, force: bool, cx: &mut App) {
    let Some(current) = current() else { return };
    let (off, last) = {
        let p = store.read(cx).prefs();
        (p.updates_off, p.last_update_check)
    };
    if !force && (off || now_ms() - last < EVERY_MS) {
        return;
    }
    store.update(cx, |s, cx| {
        s.update_note = "Checking…";
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let result = cx.background_executor().spawn(async move { fetch().map(|r| newer(&r, &current, platform())) }).await;
        store.update(cx, |s, cx| {
            match result {
                Ok(found) => {
                    s.update_note = if found.is_some() { "" } else { "You’re up to date." };
                    s.update = found;
                    s.edit_prefs(cx, |p| p.last_update_check = now_ms());
                }
                Err(e) => {
                    eprintln!("update check: {e}");
                    s.update_note = "Couldn’t check. Try again later.";
                    cx.notify();
                }
            };
        });
    })
    .detach();
}

/// The pill over the window's corner while a newer release waits.
pub struct Banner {
    store: Entity<Store>,
    _sub: Subscription,
}

impl Banner {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Banner { store, _sub: sub }
    }
}

impl Render for Banner {
    #[allow(refining_impl_trait)]
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let s = self.store.read(cx);
        let Some(update) = s.update.clone().filter(|u| u.version != s.prefs().dismissed_update) else { return div().into_any_element() };
        let (version, url) = (update.version.clone(), update.url);
        let store = self.store.clone();
        div()
            .id("update-banner")
            .occlude()
            .absolute()
            .bottom(u(24.))
            .right(u(24.))
            .h(u(40.))
            .pl(u(16.))
            .pr(u(5.))
            .gap(u(14.))
            .flex()
            .items_center()
            .rounded(u(20.))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE))
            .text_size(u(13.))
            .text_color(rgb(MUTED))
            .child(div().size(u(8.)).rounded_full().bg(rgb(ACCENT)))
            .child(div().flex().gap(u(4.)).child("Nano").child(div().font_weight(FontWeight::MEDIUM).text_color(rgb(TEXT)).child(version.clone())).child("is available"))
            .child(
                div()
                    .id("update-download")
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(ACCENT_TEXT))
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .child("Download")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
            .child(
                div()
                    .id("update-hide")
                    .ml(px(-6.))
                    .size(u(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .text_size(u(18.))
                    .text_color(rgb(FAINT))
                    .cursor_pointer()
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child("×")
                    .on_click(move |_, _, cx| store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.dismissed_update = version.clone()))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, files: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("{RELEASES}releases/tag/{tag}"),
            assets: files.iter().map(|f| Asset { name: (*f).into() }).collect(),
        }
    }

    fn found(tag: &str, current: &str, files: &[&str]) -> bool {
        newer(&release(tag, files), &Version::parse(current).unwrap(), "macos-arm64").is_some()
    }

    #[test]
    fn compares_versions_as_numbers() {
        let dmg = ["nano-macos-arm64.dmg"];
        assert!(found("v0.10.0", "0.9.0", &dmg));
        assert!(found("v1.0.0", "0.99.9", &dmg));
        assert!(!found("v0.2.1", "0.2.1", &dmg));
        assert!(!found("v0.2.0", "0.2.1", &dmg));
    }

    #[test]
    fn ignores_tags_that_are_not_versions() {
        let dmg = ["nano-macos-arm64.dmg"];
        assert!(!found("nightly", "0.2.1", &dmg));
        assert!(!found("v0.3", "0.2.1", &dmg));
    }

    #[test]
    fn prereleases_rank_below_their_release() {
        let dmg = ["nano-macos-arm64.dmg"];
        assert!(!found("v0.3.0-beta.1", "0.3.0", &dmg));
        assert!(found("v0.3.0", "0.3.0-beta.1", &dmg));
    }

    #[test]
    fn waits_for_this_machines_download() {
        assert!(!found("v0.3.0", "0.2.1", &[]));
        assert!(!found("v0.3.0", "0.2.1", &["nano-windows-x64.zip"]));
        assert!(found("v0.3.0", "0.2.1", &["nano-windows-x64.zip", "nano-macos-arm64.dmg"]));
    }

    #[test]
    fn only_trusts_links_to_this_repo() {
        let mut r = release("v0.3.0", &["nano-macos-arm64.dmg"]);
        r.html_url = "https://example.com/nano".into();
        assert!(newer(&r, &Version::parse("0.2.1").unwrap(), "macos-arm64").is_none());
    }

    /// Hits GitHub. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_the_live_latest_release() {
        let release = fetch().unwrap();
        let update = newer(&release, &Version::parse("0.1.0").unwrap(), platform()).expect("a newer release with a file for this machine");
        println!("{} {}", update.version, update.url);
        assert!(newer(&release, &parse(&release.tag_name).unwrap(), platform()).is_none());
    }
}
