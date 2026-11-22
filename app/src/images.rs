//! One cache for every decoded image, with a byte budget. Over it, the least recently drawn go first.
//! Evicted images must go to `cx.drop_image` so the GPU copy is freed too.

use gpui::RenderImage;
use image::DynamicImage;
use image::imageops::FilterType;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// GPUI also keeps a GPU copy of what's on screen, so the peak is under twice this.
// Fixed for now. Could scale with RAM.
pub const BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Long webtoon strips are split into tiles no taller than this.
pub const TILE_H: u32 = 4096;

pub type Picture = Arc<Vec<Arc<RenderImage>>>;

pub fn picture_size(p: &Picture) -> (u32, u32) {
    let w = p.first().map_or(0, |t| t.size(0).width.0 as u32);
    (w, p.iter().map(|t| t.size(0).height.0 as u32).sum())
}

pub struct ImageCache {
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<String, Entry>,
}

struct Entry {
    picture: Picture,
    bytes: usize,
    last_used: u64,
}

impl ImageCache {
    pub fn new(budget: usize) -> Self {
        ImageCache { budget, used: 0, tick: 0, entries: HashMap::new() }
    }

    pub fn get(&mut self, key: &str) -> Option<Picture> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|e| {
            e.last_used = tick;
            e.picture.clone()
        })
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Returns the evicted tiles (never the new ones) for `cx.drop_image`.
    pub fn insert(&mut self, key: String, picture: Picture) -> Vec<Arc<RenderImage>> {
        let mut evicted: Vec<Arc<RenderImage>> = self.remove(&key).into_iter().flat_map(|p| p.to_vec()).collect();
        self.tick += 1;
        let bytes = picture.iter().map(|t| t.as_bytes(0).map_or(0, <[u8]>::len)).sum();
        self.used += bytes;
        self.entries.insert(key.clone(), Entry { picture, bytes, last_used: self.tick });
        while self.used > self.budget && self.entries.len() > 1 {
            // Linear scan for the oldest. Fine at a few hundred entries.
            let oldest = self
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
                .expect("more than one entry");
            evicted.extend(self.remove(&oldest).into_iter().flat_map(|p| p.to_vec()));
        }
        evicted
    }

    pub fn remove(&mut self, key: &str) -> Option<Picture> {
        self.entries.remove(key).map(|e| {
            self.used -= e.bytes;
            e.picture
        })
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }
}

/// A full-size page can take 15+ MB while it decodes.
static DECODE_SLOTS: async_lock::Semaphore = async_lock::Semaphore::new(2);

/// Reads and deletes a file the host downloaded, then decodes it. Run it off the main thread.
pub async fn decode_file(
    path: PathBuf,
    decode: impl FnOnce(DynamicImage) -> Vec<DynamicImage>,
) -> Result<Picture, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()));
    let _ = std::fs::remove_file(&path);
    decode_bytes(bytes?, decode).await
}

pub async fn decode_bytes(
    bytes: Vec<u8>,
    decode: impl FnOnce(DynamicImage) -> Vec<DynamicImage>,
) -> Result<Picture, String> {
    let _slot = DECODE_SLOTS.acquire().await;
    let image = open(&bytes)?;
    drop(bytes);
    Ok(Arc::new(decode(image).into_iter().map(to_render).collect()))
}

/// The image crate has no AVIF decoder, so those go through rav1d.
fn open(bytes: &[u8]) -> Result<DynamicImage, String> {
    if image::guess_format(bytes).is_ok_and(|f| f == image::ImageFormat::Avif) {
        return avif(bytes);
    }
    image::load_from_memory(bytes).map_err(|e| format!("not an image ({e})"))
}

fn avif(bytes: &[u8]) -> Result<DynamicImage, String> {
    use avif_decode::{Decoder, Image};
    let img = Decoder::from_avif(bytes).and_then(|d| d.to_image()).map_err(|e| format!("not an image ({e})"))?;
    let (w, h, rgba): (usize, usize, Vec<u8>) = match img {
        Image::Rgb8(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [p.r, p.g, p.b, 255]).collect()),
        Image::Rgba8(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [p.r, p.g, p.b, p.a]).collect()),
        Image::Gray8(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [p.value(); 3].into_iter().chain([255])).collect()),
        Image::Rgb16(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [(p.r >> 8) as u8, (p.g >> 8) as u8, (p.b >> 8) as u8, 255]).collect()),
        Image::Rgba16(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [(p.r >> 8) as u8, (p.g >> 8) as u8, (p.b >> 8) as u8, (p.a >> 8) as u8]).collect()),
        Image::Gray16(i) => (i.width(), i.height(), i.pixels().flat_map(|p| [(p.value() >> 8) as u8; 3].into_iter().chain([255])).collect()),
    };
    image::RgbaImage::from_raw(w as u32, h as u32, rgba).map(DynamicImage::ImageRgba8).ok_or_else(|| "not an image (AVIF size mismatch)".into())
}

// `thumbnail` instead of `resize`, which keeps a full-height f32 copy (30-40 MB for one page).

pub fn fill(w: u32, h: u32) -> impl FnOnce(DynamicImage) -> Vec<DynamicImage> {
    move |img| {
        let scale = (w as f64 / img.width() as f64).max(h as f64 / img.height() as f64);
        let (sw, sh) = ((img.width() as f64 * scale).ceil() as u32, (img.height() as f64 * scale).ceil() as u32);
        let scaled = if scale < 1.0 { img.thumbnail_exact(sw.max(w), sh.max(h)) } else { img.resize_exact(sw, sh, FilterType::Triangle) };
        vec![scaled.crop_imm((scaled.width() - w) / 2, (scaled.height() - h) / 2, w, h)]
    }
}

pub fn fit(w: u32, h: u32) -> impl FnOnce(DynamicImage) -> Vec<DynamicImage> {
    move |img| vec![if img.width() > w || img.height() > h { img.thumbnail(w, h) } else { img }]
}

pub fn fit_width(w: u32) -> impl FnOnce(DynamicImage) -> Vec<DynamicImage> {
    move |img| {
        let img = if img.width() > w { img.thumbnail(w, u32::MAX) } else { img };
        if img.height() <= TILE_H {
            return vec![img];
        }
        (0..img.height()).step_by(TILE_H as usize).map(|y| img.crop_imm(0, y, img.width(), TILE_H.min(img.height() - y))).collect()
    }
}

/// GPUI uploads BGRA frames as-is.
fn to_render(img: DynamicImage) -> Arc<RenderImage> {
    let mut rgba = img.into_rgba8();
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avif_covers_decode() {
        let img = open(include_bytes!("../testdata/red.avif")).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (16, 24));
        let p = img.get_pixel(8, 12);
        assert!(p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] == 255, "{p:?}");
    }

    fn red(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255])))
    }

    fn solid(w: u32, h: u32) -> Picture {
        Arc::new(vec![to_render(red(w, h))])
    }

    #[test]
    fn evicts_least_recently_used_within_budget() {
        let one = 10 * 10 * 4;
        let mut c = ImageCache::new(3 * one);
        assert!(c.insert("a".into(), solid(10, 10)).is_empty());
        assert!(c.insert("b".into(), solid(10, 10)).is_empty());
        assert!(c.insert("c".into(), solid(10, 10)).is_empty());
        c.get("a"); // a is now newer than b
        let evicted = c.insert("d".into(), solid(10, 10));
        assert_eq!(evicted.len(), 1);
        assert!(!c.contains("b") && c.contains("a") && c.contains("c") && c.contains("d"));
        assert_eq!(c.used_bytes(), 3 * one);
    }

    #[test]
    fn an_oversized_picture_is_kept_alone() {
        let mut c = ImageCache::new(100);
        c.insert("small".into(), solid(2, 2));
        let evicted = c.insert("big".into(), solid(50, 50));
        assert_eq!(evicted.len(), 1);
        assert!(c.contains("big"));
    }

    #[test]
    fn reinserting_a_key_replaces_it() {
        let mut c = ImageCache::new(1 << 20);
        c.insert("a".into(), solid(10, 10));
        let old = c.insert("a".into(), solid(20, 20));
        assert_eq!(old.len(), 1);
        assert_eq!(c.used_bytes(), 20 * 20 * 4);
    }

    #[test]
    fn decode_helpers_size_and_convert() {
        let t: Picture = Arc::new(fill(336, 504)(red(1000, 1500)).into_iter().map(to_render).collect());
        assert_eq!(picture_size(&t), (336, 504));
        assert_eq!(&t[0].as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]); // red in BGRA
        assert_eq!(fit(800, 800)(red(1000, 1500))[0].height(), 800);
        assert_eq!(fit(4000, 4000)(red(1000, 1500))[0].width(), 1000); // never upscaled
    }

    #[test]
    fn long_strips_are_tiled_for_the_gpu() {
        let tiles = fit_width(720)(red(800, 10_000));
        assert!(tiles.iter().all(|t| t.width() == 720 && t.height() <= TILE_H));
        assert_eq!(tiles.iter().map(|t| t.height()).sum::<u32>(), 9000); // 10000 * 720/800
        assert_eq!(fit_width(720)(red(500, 700)).len(), 1);
    }
}
