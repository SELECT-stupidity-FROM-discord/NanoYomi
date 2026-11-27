//! Everything downloaded, as covers: only series with chapters on disk, and only those chapters inside.

use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover, grid};
use crate::images::ImageCache;
use crate::registry::Registry;
use crate::store::Store;
use crate::{FAINT, MUTED, Source, heading, u};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, rgb, uniform_list,
};
use serde_json::{Value, json};
use std::ops::Range;

const PAD: f32 = 40.;
const GAP: f32 = 24.;

struct Row {
    source: Source,
    manga: Value,
    chapters: Vec<Value>,
}

pub struct Saved {
    store: Entity<Store>,
    registry: Entity<Registry>,
    cache: Entity<ImageCache>,
    covers: Covers,
    rows: Vec<Row>,
    /// Indexes into `rows` that aren't hidden as NSFW.
    shown: Vec<usize>,
    scanned: bool,
    cols: usize,
    card_w: f32,
    _subs: Vec<Subscription>,
}

/// Opens a series from its downloads: only these chapters, no network.
pub struct OpenSaved {
    pub source: Source,
    pub manga: Value,
    pub chapters: Vec<Value>,
}

impl EventEmitter<OpenSaved> for Saved {}

impl Saved {
    pub fn new(store: Entity<Store>, registry: Entity<Registry>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let downloads = crate::downloads::get(cx);
        let subs = vec![cx.observe(&downloads, |this, _, cx| this.rescan(cx)), cx.observe(&store, |_, _, cx| cx.notify())];
        let mut this = Saved { store, registry, cache, covers: Covers::default(), rows: vec![], shown: vec![], scanned: false, cols: 1, card_w: CARD_W, _subs: subs };
        this.rescan(cx);
        this
    }

    /// Looks at the disk again, off the main thread.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { crate::downloads::scan() }).await;
            let _ = this.update(cx, |this, cx| {
                this.rows = found.into_iter().map(|f| this.resolve(f, cx)).collect();
                this.scanned = true;
                cx.notify();
            });
        })
        .detach();
    }

    /// Pairs a folder on disk with its source and series details: from the files' own record, else the library's, else just the folder names.
    fn resolve(&self, found: crate::downloads::Found, cx: &Context<Self>) -> Row {
        let registry = self.registry.read(cx);
        let store = self.store.read(cx);
        let title = found.title.as_str();
        let name = |t: &str| crate::downloads::valid_name(t, 240);
        let from_library = store.all_series().iter().find(|s| found.source_dir.starts_with(&format!("{} (", name(&s.source_name))) && name(s.title()) == name(title));
        let source = registry.by_folder(&found.source_dir).unwrap_or_else(|| match from_library {
            Some(s) => registry.source_or_saved(&s.source, &s.source_name),
            None => {
                let label = found.source_dir.rsplit_once(" (").map_or(found.source_dir.as_str(), |(n, _)| n);
                registry.source_or_saved(label, label)
            }
        });
        let manga = match (&found.saved["manga"], from_library) {
            (m, _) if m.is_object() => m.clone(),
            (_, Some(s)) => s.manga.clone(),
            _ => json!({ "title": title, "url": format!("local:{}/{}", found.source_dir, title) }),
        };
        let known = found.saved["chapters"].as_array().cloned().unwrap_or_default().into_iter().chain(from_library.map(|s| (*s.chapters).clone()).unwrap_or_default());
        let mut have = std::collections::HashSet::new();
        let mut chapters: Vec<Value> = known
            .filter(|c| {
                let n = crate::downloads::chapter_name(c);
                found.names.contains(&n) && have.insert(n)
            })
            .collect();
        let mut rest: Vec<&String> = found.names.iter().filter(|n| !have.contains(*n)).collect();
        rest.sort();
        chapters.extend(rest.into_iter().map(|n| json!({ "name": n, "url": format!("local:{}/{}/{}", found.source_dir, title, n), "scanlator": "" })));
        Row { source, manga, chapters }
    }

    fn card(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[i];
        let how = row.source.installed.then(|| Fetch::Cover(row.source.clone()));
        let url = row.manga["thumbnailUrl"].as_str().map(str::to_string);
        let title = row.manga["title"].as_str().unwrap_or_default().to_string();
        let count = row.chapters.len();
        let image = self.covers.get(&self.cache, how, url.as_deref(), |s: &mut Saved| &mut s.covers, cx);
        let w = self.card_w;
        div()
            .id(("saved", i))
            .w(u(w))
            .flex()
            .flex_col()
            .gap(u(4.))
            .cursor_pointer()
            .child(div().mb(u(4.)).child(cover(image, &title, w, w * CARD_H / CARD_W, 0)))
            .child(div().text_size(u(13.5)).font_weight(FontWeight::SEMIBOLD).truncate().child(title))
            .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).truncate().child(format!("{count} {}", if count == 1 { "chapter" } else { "chapters" })))
            .on_click(cx.listener(move |this, _, _, cx| {
                let row = &this.rows[i];
                cx.emit(OpenSaved { source: row.source.clone(), manga: row.manga.clone(), chapters: row.chapters.clone() });
            }))
            .into_any_element()
    }
}

impl Render for Saved {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.cols, self.card_w) = grid(crate::content_width(window), PAD, GAP, CARD_W);
        let store = self.store.read(cx);
        self.shown = (0..self.rows.len()).filter(|&i| {
            let row = &self.rows[i];
            let saved = store.get(&row.source.id, row.manga["url"].as_str().unwrap_or_default());
            !(store.hides(crate::store::Area::Downloaded, &row.source.id) || saved.is_some_and(|s| store.hides_series(crate::store::Area::Downloaded, s)))
        }).collect();
        let (count, cols) = (self.shown.len(), self.cols);
        let chapters: usize = self.shown.iter().map(|&i| self.rows[i].chapters.len()).sum();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(u(PAD)).pt(u(28.)).pb(u(12.)).child(heading("Downloaded", format!("{count} series · {chapters} chapters"))))
            .children((count == 0 && self.scanned).then(|| div().px(u(PAD)).pt(u(20.)).text_color(rgb(MUTED)).child("Nothing downloaded yet. Right-click a chapter on a series' page to download it.")))
            .child(
                uniform_list(
                    "saved",
                    count.div_ceil(cols),
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row| {
                                let cards: Vec<AnyElement> = (row * cols..((row + 1) * cols).min(this.shown.len())).map(|k| this.card(this.shown[k], cx)).collect();
                                div().flex().gap(u(GAP)).px(u(PAD)).py(u(GAP / 2.)).children(cards)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
    }
}
