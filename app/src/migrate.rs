//! Migration, as in Mihon: move or copy a saved series to another source, keeping your place.

use crate::catalog::small_button;
use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::registry::Registry;
use crate::store::{Progress, Series, Store};
use crate::{FAINT, LINE, MUTED, RULE, Source, TEXT, heading, u};
use gpui::{
    AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, rgb,
};
use serde_json::{Value, json};

const PER_SOURCE: usize = 8;
const CARD: f32 = 0.7; // result covers at 70% of a library card

pub enum MigrateEvent {
    Cancel,
    Done { source: Source, manga: Value },
}

enum Found {
    Searching,
    Results(Vec<Value>),
    Failed(String),
}

pub struct Migrate {
    store: Entity<Store>,
    cache: Entity<ImageCache>,
    from: Series,
    targets: Vec<(Source, Found)>,
    picked: Option<(usize, usize)>,
    query: Entity<TextInput>,
    covers: Covers,
    status: SharedString,
    busy: bool,
    generation: u64,
    _subs: Vec<Subscription>,
}

impl EventEmitter<MigrateEvent> for Migrate {}

impl Migrate {
    pub fn new(from: Series, registry: &Entity<Registry>, store: Entity<Store>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let title = from.title().to_string();
        let query = cx.new(|cx| {
            let mut t = TextInput::new("Search for…", cx);
            t.text = title;
            t
        });
        let targets = {
            let r = registry.read(cx);
            r.browsable(store.read(cx)).iter().filter(|s| s.id != from.source).filter_map(|s| r.source(&s.id)).map(|s| (s, Found::Searching)).collect()
        };
        let subs = vec![cx.subscribe(&query, |this, _, _: &InputEvent, cx| this.search(cx))];
        let mut this = Migrate { store, cache, from, targets, picked: None, query, covers: Covers::default(), status: "".into(), busy: false, generation: 0, _subs: subs };
        this.search(cx);
        this
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.picked = None;
        let query = self.query.read(cx).text.trim().to_string();
        for (i, (source, found)) in self.targets.iter_mut().enumerate() {
            *found = Found::Searching;
            let (source, query, generation) = (source.clone(), query.clone(), self.generation);
            cx.spawn(async move |this, cx| {
                let result = source.call(cx, "search", json!({ "page": 1, "query": query })).await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.targets[i].1 = match result {
                        Ok(v) => Found::Results(v["mangas"].as_array().into_iter().flatten().take(PER_SOURCE).cloned().collect()),
                        Err(e) => Found::Failed(e),
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Saves the picked series, and drops the old record unless `keep_old` is set.
    fn migrate(&mut self, keep_old: bool, cx: &mut Context<Self>) {
        let Some((t, r)) = self.picked else { return };
        let Found::Results(results) = &self.targets[t].1 else { return };
        let (source, manga, from) = (self.targets[t].0.clone(), results[r].clone(), self.from.clone());
        self.busy = true;
        self.status = format!("Fetching chapters from {}…", source.name).into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = source.call(cx, "update", json!({ "manga": manga, "chapters": [] })).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let v = match result {
                    Ok(v) => v,
                    Err(e) => {
                        this.status = format!("Couldn't load it: {e}").into();
                        return cx.notify();
                    }
                };
                // The host keeps the search result's URL and title when the details leave them out.
                let manga = v["manga"].clone();
                let chapters = v["chapters"].as_array().cloned().unwrap_or_default();
                let progress = from.progress.as_ref().and_then(|p| map_progress(p, &from.chapters, &chapters));
                this.store.update(cx, |store, cx| {
                    store.update(&source.id, &source.name, &manga, cx, |s| {
                        s.manga = manga.clone();
                        s.chapters = std::sync::Arc::new(chapters);
                        s.in_library = from.in_library || s.in_library;
                        s.added_at = from.added_at;
                        if progress.is_some() {
                            s.progress = progress;
                        }
                    });
                    if !keep_old {
                        // A record with no library entry and no progress gets dropped.
                        store.update(&from.source, &from.source_name, &from.manga, cx, |s| {
                            s.in_library = false;
                            s.progress = None;
                        });
                    }
                });
                cx.emit(MigrateEvent::Done { source, manga });
            });
        })
        .detach();
    }
}

/// The source's chapter number, or one read from the name when it's unset (as Mihon does).
pub fn chapter_number(c: &Value) -> Option<f64> {
    c["chapterNumber"].as_f64().filter(|n| *n >= 0.).or_else(|| c["name"].as_str().and_then(name_number))
}

/// Reads the number after "ch", "chapter", "ep" or "episode", or else the first number in the name.
pub fn name_number(name: &str) -> Option<f64> {
    let lower = name.to_lowercase();
    let number_at = |s: &str| -> Option<f64> {
        let s = s.trim_start_matches(|c: char| c == '.' || c == ':' || c == '#' || c.is_whitespace());
        let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
        s[..end].trim_end_matches('.').parse().ok()
    };
    ["chapter", "episode", "ch", "ep"]
        .iter()
        .find_map(|k| lower.find(k).and_then(|i| number_at(&lower[i + k.len()..])))
        .or_else(|| lower.find(|c: char| c.is_ascii_digit()).and_then(|i| number_at(&lower[i..])))
}

/// Finds the chapter with the same number, or else the same name.
pub fn map_progress(p: &Progress, old: &[Value], new: &[Value]) -> Option<Progress> {
    let old_ch = old.iter().find(|c| c["url"] == p.chapter_url.as_str());
    let number = old_ch.and_then(chapter_number).or_else(|| name_number(&p.chapter_name));
    let name = old_ch.and_then(|c| c["name"].as_str()).unwrap_or(&p.chapter_name).to_lowercase();
    let hit = number
        .and_then(|n| new.iter().find(|c| chapter_number(c).is_some_and(|m| (m - n).abs() < 0.001)))
        .or_else(|| new.iter().find(|c| c["name"].as_str().is_some_and(|s| s.to_lowercase() == name)))?;
    Some(Progress {
        chapter_url: hit["url"].as_str()?.to_string(),
        chapter_name: hit["name"].as_str().unwrap_or("Chapter").to_string(),
        page: p.page,
        pages: p.pages,
        at: p.at,
    })
}

impl Render for Migrate {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (w, h) = (CARD_W * CARD, CARD_H * CARD);
        let picked = self.picked;
        let mut rows = vec![];
        for t in 0..self.targets.len() {
            let name = self.targets[t].0.name.clone();
            let body = match &self.targets[t].1 {
                Found::Searching => div().text_size(u(13.)).text_color(rgb(FAINT)).child("searching…").into_any_element(),
                Found::Failed(e) => div().text_size(u(13.)).text_color(rgb(FAINT)).truncate().child(e.clone()).into_any_element(),
                Found::Results(list) if list.is_empty() => div().text_size(u(13.)).text_color(rgb(FAINT)).child("nothing found").into_any_element(),
                Found::Results(list) => {
                    let list = list.clone();
                    let source = self.targets[t].0.clone();
                    let cards: Vec<_> = list
                        .iter()
                        .enumerate()
                        .map(|(r, m)| {
                            let title = m["title"].as_str().unwrap_or("Untitled").to_string();
                            let url = m["thumbnailUrl"].as_str().map(str::to_string);
                            let image = self.covers.get(&self.cache, Some(Fetch::Cover(source.clone())), url.as_deref(), |m: &mut Migrate| &mut m.covers, cx);
                            let on = picked == Some((t, r));
                            div()
                                .id(("pick", t * 1000 + r))
                                .w(u(w))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap(u(6.))
                                .cursor_pointer()
                                .p(u(4.))
                                .rounded(u(6.))
                                .border_2()
                                .border_color(rgb(if on { TEXT } else { 0x0D0C0B }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.picked = Some((t, r));
                                    cx.notify();
                                }))
                                .child(cover(image, &title, w, h, 0))
                                .child(div().text_size(u(12.5)).line_clamp(2).child(title))
                        })
                        .collect();
                    div().id(("results", t)).flex().gap(u(12.)).overflow_x_scroll().children(cards).into_any_element()
                }
            };
            rows.push(
                div()
                    .py(u(14.))
                    .border_b_1()
                    .border_color(rgb(RULE))
                    .flex()
                    .flex_col()
                    .gap(u(10.))
                    .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(MUTED)).child(name.to_uppercase()))
                    .child(body),
            );
        }
        let can = picked.is_some() && !self.busy;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(u(40.))
                    .pt(u(28.))
                    .flex()
                    .items_end()
                    .gap_4()
                    .child(div().flex_1().min_w_0().child(heading("Migrate", format!("{} · from {}", self.from.title(), self.from.source_name))))
                    .child(div().w(u(320.)).child(self.query.clone())),
            )
            .child(
                div()
                    .id("migrate-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(u(40.))
                    .pt(u(8.))
                    .when(self.targets.is_empty(), |d| d.child(div().pt(u(24.)).text_color(rgb(MUTED)).child("No other sources installed. Add one in Extensions.")))
                    .children(rows),
            )
            .child(
                div()
                    .px(u(40.))
                    .py(u(16.))
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .items_center()
                    .gap(u(10.))
                    .child(div().flex_1().text_size(u(13.)).text_color(rgb(MUTED)).child(if self.status.is_empty() {
                        SharedString::from("Pick the matching series. Progress carries over by chapter number.")
                    } else {
                        self.status.clone()
                    }))
                    .child(small_button("migrate-cancel", "Cancel", false).on_click(cx.listener(|_, _, _, cx| cx.emit(MigrateEvent::Cancel))))
                    .child(small_button("migrate-copy", "Copy", false).when(can, |d| d.on_click(cx.listener(|this, _, _, cx| this.migrate(true, cx)))).when(!can, |d| d.opacity(0.4)))
                    .child(
                        small_button("migrate-move", "Migrate", true)
                            .when(can, |d| d.on_click(cx.listener(|this, _, _, cx| this.migrate(false, cx))))
                            .when(!can, |d| d.opacity(0.4))
                            .font_weight(FontWeight::SEMIBOLD),
                    ),
            )
    }
}

#[cfg(test)]
#[test]
fn progress_maps_by_number_then_name() {
    let p = Progress { chapter_url: "/a/5".into(), chapter_name: "Ch. 5".into(), page: 3, pages: 20, at: 1 };
    let old = vec![json!({"url":"/a/5","name":"Ch. 5","chapterNumber":5.0})];
    let new = vec![json!({"url":"/b/6","name":"Episode 6","chapterNumber":6.0}), json!({"url":"/b/5","name":"Episode 5","chapterNumber":5.0})];
    let m = map_progress(&p, &old, &new).unwrap();
    assert_eq!((m.chapter_url.as_str(), m.page), ("/b/5", 3));
    let by_name = vec![json!({"url":"/c/x","name":"ch. 5","chapterNumber":-1.0})];
    let old_unnumbered = vec![json!({"url":"/a/5","name":"Ch. 5","chapterNumber":-1.0})];
    assert_eq!(map_progress(&p, &old_unnumbered, &by_name).unwrap().chapter_url, "/c/x");
    assert!(map_progress(&p, &old, &[json!({"url":"/z","name":"Other","chapterNumber":9.0})]).is_none());
    // MangaDex leaves numbers unset, but the name still has them.
    let md = vec![json!({"url":"/a/5","name":"Ch.5 - Things I Memorized","chapterNumber":-1.0})];
    let comix = vec![json!({"url":"/c/5","name":"Chapter 5: Things I Memorized","chapterNumber":5.0})];
    assert_eq!(map_progress(&p, &md, &comix).unwrap().chapter_url, "/c/5");
    assert_eq!(name_number("Episode 12.5"), Some(12.5));
    assert_eq!(name_number("Vol.2 Ch.10"), Some(10.));
}
