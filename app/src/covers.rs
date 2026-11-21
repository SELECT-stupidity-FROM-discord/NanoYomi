//! Covers load when their card is drawn, through the extension, and are decoded at card size.

use crate::u;
use crate::images::{self, ImageCache, Picture};
use crate::{BG, SURFACE, Source, TEXT, drop_images};
use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ObjectFit, ParentElement, RenderImage, SharedString, Styled,
    StyledImage, div, img, prelude::FluentBuilder, relative, rgb,
};
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const CARD_W: f32 = 168.;
pub const CARD_H: f32 = 252.;

/// How many cards fit across `width` (in real pixels), and how wide each is in design points.
pub fn grid(width: f32, pad: f32, gap: f32, min: f32) -> (usize, f32) {
    let inner = (width / crate::zoom() - 2. * pad).max(min);
    let cols = (((inner + gap) / (min + gap)).floor() as usize).max(1);
    (cols, (inner - gap * (cols - 1) as f32) / cols as f32)
}

#[cfg(test)]
#[test]
fn grid_fills_the_width() {
    let (cols, w) = grid(1280., 40., 24., CARD_W);
    assert_eq!(cols, 6);
    assert!((cols as f32 * w + 5. * 24. - 1200.).abs() < 0.01);
    assert_eq!(grid(100., 40., 24., CARD_W).0, 1);
}

/// Covers on disk are reused for this long before asking the site again.
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_cache_dir(dir: PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

pub fn clear_cache() {
    if let Some(dir) = CACHE_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn cache_path(url: &str) -> Option<PathBuf> {
    CACHE_DIR.get().map(|d| d.join(format!("{:016x}", crate::fnv(url))))
}

fn fresh(path: &Path) -> bool {
    std::fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age < CACHE_TTL))
}

/// Uses the disk copy while it's under a week old, or any old copy if the download fails.
pub async fn load_cover(source: &Source, cx: &mut gpui::AsyncApp, url: &str) -> Result<Picture, String> {
    let size = || images::fill((CARD_W * 2.) as u32, (CARD_H * 2.) as u32);
    let read = move |p: PathBuf| async move { images::decode_bytes(std::fs::read(&p).map_err(|e| e.to_string())?, size()).await };
    let cached = cache_path(url);
    if let Some(p) = cached.clone().filter(|p| fresh(p)) {
        if let Ok(picture) = cx.background_executor().spawn(read(p)).await {
            return Ok(picture);
        }
    }
    let downloaded = source.call(cx, "download", json!({ "url": url })).await.and_then(|v| {
        v.as_str().map(PathBuf::from).ok_or_else(|| "no file from host".to_string())
    });
    let path = match (downloaded, &cached) {
        (Ok(path), _) => path,
        (Err(e), Some(p)) if p.exists() => return cx.background_executor().spawn(read(p.clone())).await.map_err(|_| e),
        (Err(e), _) => return Err(e),
    };
    let Some(target) = cached else {
        return cx.background_executor().spawn(images::decode_file(path, size())).await;
    };
    cx.background_executor()
        .spawn(async move {
            let kept = std::fs::create_dir_all(target.parent().unwrap())
                .and_then(|_| std::fs::rename(&path, &target).or_else(|_| std::fs::copy(&path, &target).map(|_| ())));
            if kept.is_err() {
                return images::decode_file(path, size()).await;
            }
            let _ = std::fs::remove_file(&path);
            let result = read(target.clone()).await;
            if result.is_err() {
                let _ = std::fs::remove_file(&target); // an error page, not a cover
            }
            result
        })
        .await
}

pub enum Fetch {
    Cover(Source),
    Icon,
}

async fn fetch(how: Fetch, cx: &mut gpui::AsyncApp, url: &str) -> Result<Picture, String> {
    match how {
        Fetch::Cover(source) => load_cover(&source, cx, url).await,
        Fetch::Icon => {
            let url = url.to_string();
            cx.background_executor()
                .spawn(async move { images::decode_bytes(crate::extensions::fetch_icon(&url)?, images::fill(88, 88)).await })
                .await
        }
    }
}

#[derive(Default)]
pub struct Covers {
    requested: HashSet<String>,
    failed: HashSet<String>,
    /// Not on disk while its extension isn't loaded. Tried again once there's a source to fetch from.
    missing: HashSet<String>,
}

impl Covers {
    /// The cover if it's decoded, otherwise starts loading it and redraws when it's in.
    pub fn get<V: 'static>(
        &mut self,
        cache: &Entity<ImageCache>,
        how: Option<Fetch>,
        url: Option<&str>,
        field: fn(&mut V) -> &mut Covers,
        cx: &mut Context<V>,
    ) -> Option<Arc<RenderImage>> {
        let url = url?;
        if let Some(picture) = cache.update(cx, |c, _| c.get(url)) {
            return picture.first().cloned();
        }
        let offline = how.is_none();
        if self.failed.contains(url) || (offline && self.missing.contains(url)) || !self.requested.insert(url.to_string()) {
            return None;
        }
        let (url, cache) = (url.to_string(), cache.clone());
        cx.spawn(async move |this, cx| {
            let result = match how {
                Some(how) => fetch(how, cx, &url).await,
                // The extension is gone, so only a cover saved on disk can show.
                None => match cache_path(&url) {
                    Some(p) => cx.background_executor().spawn(async move { images::decode_bytes(std::fs::read(&p).map_err(|e| e.to_string())?, images::fill((CARD_W * 2.) as u32, (CARD_H * 2.) as u32)).await }).await,
                    None => Err("no cached cover".into()),
                },
            };
            let _ = this.update(cx, |view, cx| {
                let covers = field(view);
                covers.requested.remove(&url);
                match result {
                    Ok(picture) => {
                        let evicted = cache.update(cx, |c, _| c.insert(url, picture));
                        drop_images(cx, None, evicted);
                        cx.notify();
                    }
                    Err(_) if offline => {
                        covers.missing.insert(url);
                    }
                    Err(e) => {
                        eprintln!("cover: {e}");
                        covers.failed.insert(url);
                    }
                }
            });
        })
        .detach();
        None
    }
}

/// Shows the title in display caps until the image loads. `badge` is the count in the corner.
pub fn cover(image: Option<Arc<RenderImage>>, title: &str, w: f32, h: f32, badge: usize) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .w(u(w))
        .h(u(h))
        .rounded(u(4.))
        .overflow_hidden()
        .bg(rgb(SURFACE))
        .map(|d| match image {
            Some(image) => d.child(img(image).size_full().object_fit(ObjectFit::Cover)),
            None => d.flex().items_end().p_3().child(
                div()
                    .font_family(crate::fonts::display())
                    .font_weight(FontWeight::EXTRA_BOLD)
                    .text_size(u((w / 8.).clamp(14., 26.)))
                    .line_height(relative(1.))
                    .text_color(rgb(TEXT))
                    .line_clamp(4)
                    .child(SharedString::from(title.to_uppercase())),
            ),
        })
        .when(badge > 0, |d| {
            d.child(
                div()
                    .absolute()
                    .top(u(8.))
                    .right(u(8.))
                    .min_w(u(24.))
                    .h(u(24.))
                    .px(u(7.))
                    .rounded(u(12.))
                    .bg(rgb(BG))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(crate::fonts::mono())
                    .text_size(u(12.))
                    .text_color(rgb(TEXT))
                    .child(badge.to_string()),
            )
        })
        .into_any_element()
}
