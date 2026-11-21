//! Three font slots, each with a bundled default. Other fonts come from Google Fonts into the data folder.

use gpui::{App, SharedString};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

/// Pref key, label, bundled default.
pub const SLOTS: [(&str, &str, &str); 3] = [
    ("display", "Headings", "Big Shoulders Display"),
    ("sans", "Text", "IBM Plex Sans"),
    ("mono", "Labels and numbers", "IBM Plex Mono"),
];

/// Popular Google Fonts, not all 1,500 of them.
pub const GOOGLE: [&str; 40] = [
    "Roboto", "Open Sans", "Inter", "Lato", "Montserrat", "Poppins", "Noto Sans", "Source Sans 3", "Nunito", "Work Sans",
    "DM Sans", "Rubik", "Manrope", "Outfit", "Lexend", "Merriweather", "Playfair Display", "Lora", "Source Serif 4", "Crimson Pro",
    "Bebas Neue", "Oswald", "Anton", "Archivo Black", "Barlow Condensed", "Fjalla One", "Teko", "Righteous", "Bangers", "Abril Fatface",
    "Roboto Mono", "JetBrains Mono", "Fira Code", "Source Code Pro", "Space Mono", "DM Mono", "Inconsolata", "Ubuntu Mono", "Red Hat Mono", "Sometype Mono",
];

static DIR: OnceLock<PathBuf> = OnceLock::new();
static CHOSEN: RwLock<[Option<String>; 3]> = RwLock::new([None, None, None]);
static LOADED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub fn dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_default()
}

/// Takes the choices from the saved prefs, keyed by slot.
pub fn choose(chosen: &HashMap<String, String>) {
    *CHOSEN.write().unwrap() = SLOTS.map(|(key, ..)| chosen.get(key).cloned());
}

fn family(slot: usize) -> SharedString {
    CHOSEN.read().unwrap()[slot].clone().unwrap_or_else(|| SLOTS[slot].2.to_string()).into()
}

pub fn display() -> SharedString {
    family(0)
}

pub fn sans() -> SharedString {
    family(1)
}

pub fn mono() -> SharedString {
    family(2)
}

/// Files are named after the family without spaces, e.g. `RobotoMono-400.ttf`.
fn files(name: &str) -> Vec<PathBuf> {
    let prefix = name.replace(' ', "").to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir()) else { return vec![] };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let file = p.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_lowercase();
            file.starts_with(&prefix) && (file.ends_with(".ttf") || file.ends_with(".otf"))
        })
        .collect()
}

pub fn installed(name: &str) -> bool {
    !files(name).is_empty()
}

/// Hands every font file in the folder to the text system, once each.
pub fn load(cx: &App) {
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    let mut loaded = LOADED.lock().unwrap();
    let new: Vec<(PathBuf, Vec<u8>)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ttf" || x == "otf") && !loaded.contains(p))
        .filter_map(|p| std::fs::read(&p).ok().map(|b| (p, b)))
        .collect();
    if new.is_empty() {
        return;
    }
    let bytes = new.iter().map(|(_, b)| Cow::Owned(b.clone())).collect();
    match cx.text_system().add_fonts(bytes) {
        Ok(()) => loaded.extend(new.into_iter().map(|(p, _)| p)),
        Err(e) => eprintln!("fonts: {e}"),
    }
}

/// Each `@font-face` block of a Google Fonts stylesheet as (weight, file url).
fn faces(css: &str) -> Vec<(u32, String)> {
    css.split("@font-face")
        .skip(1)
        .filter_map(|block| {
            let weight = block.split("font-weight:").nth(1)?.trim_start().split(';').next()?.trim().parse().ok()?;
            let url = block.split("url(").nth(1)?.split(')').next()?.to_string();
            Some((weight, url))
        })
        .collect()
}

/// Blocking. Run it off the main thread. Returns how many files it saved.
pub fn download(name: &str) -> Result<usize, String> {
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build());
    let family = name.replace(' ', "+");
    // Not every family has every weight, so fall back to just the regular one.
    let css = [format!("{family}:400,500,600,700"), family]
        .iter()
        .find_map(|f| agent.get(format!("https://fonts.googleapis.com/css?family={f}")).call().ok())
        .ok_or("couldn't reach Google Fonts")?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let faces = faces(&css);
    if faces.is_empty() {
        return Err("Google Fonts has no family by that name".into());
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    for (weight, url) in &faces {
        let bytes = agent.get(url).call().map_err(|e| e.to_string())?.body_mut().read_to_vec().map_err(|e| e.to_string())?;
        let file = dir().join(format!("{}-{weight}.ttf", name.replace(' ', "")));
        std::fs::write(file, bytes).map_err(|e| e.to_string())?;
    }
    Ok(faces.len())
}

pub fn folder() -> String {
    Path::new(&dir()).display().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_weights_and_urls_from_google_css() {
        let css = "@font-face {\n  font-weight: 400;\n  src: url(https://x/a.ttf) format('truetype');\n}\n@font-face {\n  font-weight: 700;\n  src: url(https://x/b.ttf) format('truetype');\n}";
        assert_eq!(super::faces(css), vec![(400, "https://x/a.ttf".into()), (700, "https://x/b.ttf".into())]);
        assert!(super::faces("nope").is_empty());
    }
}
