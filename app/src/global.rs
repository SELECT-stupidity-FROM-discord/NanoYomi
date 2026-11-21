//! One search across every source you browse, each source's first page as a row of covers.

use crate::browse::OpenManga;
use crate::covers::{CARD_H, CARD_W, Covers, Fetch, cover};
use crate::images::ImageCache;
use crate::input::{InputEvent, TextInput};
use crate::registry::Registry;
use crate::store::Store;
use crate::{FAINT, MUTED, Source, heading, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, rgb,
};
use serde_json::{Value, json};

const PAD: f32 = 40.;
const CARD: f32 = 120.;

enum Results {
    Loading,
    Found(Vec<Value>),
    Failed(String),
}

pub struct GlobalSearch {
    registry: Entity<Registry>,
    store: Entity<Store>,
    cache: Entity<ImageCache>,
    pub input: Entity<TextInput>,
    query: String,
    rows: Vec<(Source, Results)>,
    covers: Covers,
    /// Bumped per search so late answers from the last one are dropped.
    generation: u64,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OpenManga> for GlobalSearch {}

impl GlobalSearch {
    pub fn new(registry: Entity<Registry>, store: Entity<Store>, cache: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search every source…", cx));
        let subs = vec![cx.subscribe(&input, |this, input, _: &InputEvent, cx| {
            let q = input.read(cx).text.trim().to_string();
            this.search(q, cx);
        })];
        GlobalSearch { registry, store, cache, input, query: String::new(), rows: vec![], covers: Covers::default(), generation: 0, _subs: subs }
    }

    pub fn search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            return;
        }
        self.input.update(cx, |i, cx| {
            i.text = query.clone();
            cx.notify();
        });
        self.query = query.clone();
        self.generation += 1;
        let generation = self.generation;
        let registry = self.registry.read(cx);
        let sources: Vec<Source> = registry.browsable(self.store.read(cx)).iter().filter_map(|s| registry.source(&s.id)).collect();
        self.rows = sources.iter().map(|s| (s.clone(), Results::Loading)).collect();
        // All at once, so slow sites don't hold up the rest.
        for (i, source) in sources.into_iter().enumerate() {
            let query = query.clone();
            cx.spawn(async move |this, cx| {
                let result = source.call(cx, "search", json!({ "page": 1, "query": query })).await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.rows[i].1 = match result {
                        Ok(v) => Results::Found(v["mangas"].as_array().cloned().unwrap_or_default()),
                        Err(e) => Results::Failed(e),
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn row(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let source = self.rows[i].0.clone();
        let label = if source.lang.is_empty() || source.lang == "en" { source.name.clone() } else { format!("{} · {}", source.name, source.lang) };
        let body: AnyElement = match &self.rows[i].1 {
            Results::Loading => div().h(u(CARD * CARD_H / CARD_W)).flex().items_center().text_color(rgb(FAINT)).child("Searching…").into_any_element(),
            Results::Failed(e) => div().text_size(u(13.)).text_color(rgb(crate::ACCENT_TEXT)).child(e.clone()).into_any_element(),
            Results::Found(list) if list.is_empty() => div().text_size(u(13.)).text_color(rgb(FAINT)).child("Nothing found").into_any_element(),
            Results::Found(list) => {
                let list = list.clone();
                let cards: Vec<AnyElement> = list
                    .into_iter()
                    .enumerate()
                    .map(|(j, manga)| {
                        let title = manga["title"].as_str().unwrap_or("Untitled").to_string();
                        let url = manga["thumbnailUrl"].as_str().map(str::to_string);
                        let image = self.covers.get(&self.cache, Some(Fetch::Cover(source.clone())), url.as_deref(), |g: &mut GlobalSearch| &mut g.covers, cx);
                        let source = source.clone();
                        div()
                            .id(SharedString::from(format!("g-{i}-{j}")))
                            .w(u(CARD))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .cursor_pointer()
                            .child(cover(image, &title, CARD, CARD * CARD_H / CARD_W, 0))
                            .child(div().text_size(u(12.5)).font_weight(FontWeight::MEDIUM).line_clamp(2).child(title))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenManga { source: source.clone(), manga: manga.clone() })))
                            .into_any_element()
                    })
                    .collect();
                div().id(SharedString::from(format!("g-row-{i}"))).flex().gap(u(14.)).overflow_x_scroll().pb(u(6.)).children(cards).into_any_element()
            }
        };
        div()
            .pt(u(24.))
            .flex()
            .flex_col()
            .gap(u(10.))
            .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(MUTED)).child(label.to_uppercase()))
            .child(body)
            .into_any_element()
    }
}

impl Render for GlobalSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let done = self.rows.iter().filter(|(_, r)| !matches!(r, Results::Loading)).count();
        let note = if self.query.is_empty() {
            String::new()
        } else if done < self.rows.len() {
            format!("“{}” · {done} of {} sources", self.query, self.rows.len())
        } else {
            format!("“{}” · {} sources", self.query, self.rows.len())
        };
        // Sources with results first, so the useful rows are at the top.
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by_key(|&i| match &self.rows[i].1 {
            Results::Found(l) if !l.is_empty() => 0,
            Results::Loading => 1,
            _ => 2,
        });
        let rows: Vec<AnyElement> = order.into_iter().map(|i| self.row(i, cx)).collect();
        div()
            .id("global-search")
            .size_full()
            .overflow_y_scroll()
            .px(u(PAD))
            .pt(u(28.))
            .pb(u(40.))
            .child(div().flex().flex_wrap().items_end().gap_4().child(div().flex_1().min_w_0().child(heading("Search", note))).child(div().w(u(360.)).max_w_full().child(self.input.clone())))
            .when(self.query.is_empty(), |d| {
                d.child(div().pt(u(24.)).text_color(rgb(MUTED)).child("Type a title and press Enter to search every source you browse at once."))
            })
            .children(rows)
    }
}
