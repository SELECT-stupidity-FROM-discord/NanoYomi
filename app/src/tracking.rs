//! The tracking drawer: find a series on MyAnimeList or AniList, then set its status, chapters and score.

use crate::catalog::{chip, small_button};
use crate::input::{InputEvent, TextInput};
use crate::store::{Store, Track};
use crate::track::{self, Found};
use crate::{ACCENT_TEXT, FAINT, LINE, MUTED, RULE, SURFACE, TEXT, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, rgb,
};

pub struct Close;

pub struct Tracking {
    store: Entity<Store>,
    /// The series' source id and url.
    key: (String, String),
    title: String,
    query: Entity<TextInput>,
    /// The tracker being searched, and what it found.
    searching: Option<i64>,
    results: Vec<Found>,
    status: SharedString,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Close> for Tracking {}

impl Tracking {
    pub fn new(store: Entity<Store>, key: (String, String), title: String, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut i = TextInput::new("Search by title", cx);
            i.text = title.clone();
            i
        });
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&query, |this, _, _: &InputEvent, cx| {
                if let Some(t) = this.searching {
                    this.search(t, cx)
                }
            }),
        ];
        store.update(cx, |s, _| s.track_error = None);
        Tracking { store, key, title, query, searching: None, results: vec![], status: "".into(), _subs: subs }
    }

    fn track(&self, tracker: i64, cx: &Context<Self>) -> Option<Track> {
        let s = self.store.read(cx).get(&self.key.0, &self.key.1)?;
        s.tracks.iter().find(|t| t.tracker == tracker).cloned()
    }

    fn search(&mut self, tracker: i64, cx: &mut Context<Self>) {
        let Some(mut login) = self.store.read(cx).login(tracker).cloned() else { return };
        let query = self.query.read(cx).text.trim().to_string();
        self.searching = Some(tracker);
        self.results.clear();
        self.status = "Searching…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let refreshed = track::refresh(tracker, &mut login)?;
                    track::search(tracker, &login, &query).map(|r| (r, refreshed.then_some(login)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((found, login)) => {
                        if let Some(l) = login {
                            this.store.update(cx, |s, cx| s.set_login(tracker, Some(l), cx));
                        }
                        this.status = if found.is_empty() { "Nothing found. Try another title.".into() } else { "".into() };
                        this.results = found;
                    }
                    Err(e) => this.status = e.into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn bind(&mut self, tracker: i64, found: Found, cx: &mut Context<Self>) {
        let Some(mut login) = self.store.read(cx).login(tracker).cloned() else { return };
        let read = self.store.read(cx).get(&self.key.0, &self.key.1).map_or(0., |s| s.read_number());
        self.status = "Adding…".into();
        cx.notify();
        let key = self.key.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let refreshed = track::refresh(tracker, &mut login)?;
                    track::bind(tracker, &login, &found, read).map(|t| (t, refreshed.then_some(login)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((t, login)) => {
                        this.store.update(cx, |s, cx| {
                            if let Some(l) = login {
                                s.set_login(tracker, Some(l), cx);
                            }
                            s.put_track(&key, t, cx);
                        });
                        this.searching = None;
                        this.results.clear();
                        this.status = "".into();
                        crate::sync::run(this.store.clone(), cx);
                    }
                    Err(e) => this.status = e.into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Edits a linked entry here and on the site.
    fn change(&mut self, mut t: Track, f: impl FnOnce(&mut Track), cx: &mut Context<Self>) {
        f(&mut t);
        let key = self.key.clone();
        self.store.update(cx, |s, cx| s.push_track(key, t, cx));
        crate::sync::run(self.store.clone(), cx);
    }

    fn linked(&self, t: Track, cx: &mut Context<Self>) -> AnyElement {
        let id = |name: &str, i: i64| SharedString::from(format!("{name}-{}-{i}", t.tracker));
        let statuses = track::statuses(t.tracker).into_iter().map(|(value, label)| {
            let t = t.clone();
            chip(id("status", value), label, t.status == value).on_click(cx.listener(move |this, _, _, cx| this.change(t.clone(), |t| t.status = value, cx)))
        });
        let score = track::score_of(&t);
        let scores = (0..=10).map(|n| {
            let t = t.clone();
            chip(id("score", n), if n == 0 { "–".to_string() } else { n.to_string() }, score == n)
                .on_click(cx.listener(move |this, _, _, cx| this.change(t.clone(), |t| track::set_score(t, n), cx)))
        });
        let read = t.last_read as i64;
        let total = if t.total > 0 { t.total.to_string() } else { "?".into() };
        let step = |name: &'static str, label: &'static str, by: i64, t: &Track| {
            let t = t.clone();
            small_button(id(name, 0), label, false).on_click(cx.listener(move |this, _, _, cx| {
                this.change(t.clone(), |t| t.last_read = (t.last_read as i64 + by).max(0) as f32, cx)
            }))
        };
        let (url, tracker) = (t.url.clone(), t.tracker);
        let label = |s: &'static str| div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(s);
        div()
            .flex()
            .flex_col()
            .gap(u(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id(id("open", 0))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .child(t.title.clone())
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(small_button(id("change", 0), "Change", false).on_click(cx.listener(move |this, _, _, cx| this.search(tracker, cx))))
                    .child(small_button(id("remove", 0), "Remove", false).on_click(cx.listener(move |this, _, _, cx| {
                        // Local only, like Mihon's default. The site keeps its entry.
                        let key = this.key.clone();
                        this.store.update(cx, |s, cx| s.put_track(&key, Track { tracker, ..Track::default() }, cx));
                        crate::sync::run(this.store.clone(), cx);
                    }))),
            )
            .child(label("STATUS"))
            .child(div().flex().flex_wrap().gap(u(6.)).children(statuses))
            .child(label("CHAPTERS"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(step("less", "−", -1, &t))
                    .child(div().font_family(crate::fonts::mono()).text_size(u(14.)).child(format!("{read} / {total}")))
                    .child(step("more", "+", 1, &t)),
            )
            .child(label("SCORE"))
            .child(div().flex().flex_wrap().gap(u(6.)).children(scores))
            .into_any_element()
    }

    fn results(&self, tracker: i64, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.results.iter().enumerate().map(|(i, f)| {
            let found = f.clone();
            let detail = [f.kind.clone(), f.year.clone(), if f.chapters > 0 { format!("{} chapters", f.chapters) } else { String::new() }]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            div()
                .id(SharedString::from(format!("found-{tracker}-{i}")))
                .py(u(9.))
                .px(u(10.))
                .rounded(u(6.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(crate::HOVER)))
                .child(div().text_size(u(14.)).font_weight(FontWeight::MEDIUM).line_clamp(2).child(f.title.clone()))
                .child(div().text_size(u(12.)).text_color(rgb(MUTED)).child(detail))
                .on_click(cx.listener(move |this, _, _, cx| this.bind(tracker, found.clone(), cx)))
        });
        div()
            .flex()
            .flex_col()
            .gap(u(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.query.clone()))
                    .child(small_button(SharedString::from(format!("cancel-{tracker}")), "Cancel", false).on_click(cx.listener(|this, _, _, cx| {
                        this.searching = None;
                        this.results.clear();
                        this.status = "".into();
                        cx.notify();
                    }))),
            )
            .children(rows)
            .into_any_element()
    }
}

impl Render for Tracking {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.store.read(cx).track_error.clone();
        let sections: Vec<AnyElement> = track::TRACKERS
            .into_iter()
            .filter(|t| track::configured(*t))
            .map(|tracker| {
                let signed_in = self.store.read(cx).login(tracker).is_some();
                let body = if self.searching == Some(tracker) {
                    self.results(tracker, cx)
                } else if let Some(t) = self.track(tracker, cx) {
                    self.linked(t, cx)
                } else if signed_in {
                    small_button(SharedString::from(format!("find-{tracker}")), "", true)
                        .child(format!("Find on {}", track::name(tracker)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let title = this.title.clone();
                            this.query.update(cx, |i, _| i.text = title);
                            this.search(tracker, cx)
                        }))
                        .into_any_element()
                } else {
                    div().text_size(u(13.)).text_color(rgb(MUTED)).child(format!("Sign in to {} in Settings to track this series.", track::name(tracker))).into_any_element()
                };
                div()
                    .py(u(16.))
                    .flex()
                    .flex_col()
                    .gap(u(12.))
                    .border_b_1()
                    .border_color(rgb(RULE))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child(track::name(tracker)))
                    .child(body)
                    .into_any_element()
            })
            .collect();
        div()
            .id("tracking")
            .occlude()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(rgb(SURFACE))
            .border_l_1()
            .border_color(rgb(LINE))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(u(24.))
                    .pt(u(24.))
                    .pb(u(8.))
                    .flex()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().text_size(u(17.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(TEXT)).child("Tracking"))
                    .child(small_button("tracking-close", "Close", false).on_click(cx.listener(|_, _, _, cx| cx.emit(Close)))),
            )
            .child(
                div()
                    .id("tracking-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(u(24.))
                    .pb(u(24.))
                    .when(!self.status.is_empty(), |d| d.child(div().py(u(8.)).text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(self.status.clone())))
                    .when_some(error, |d, e| d.child(div().py(u(8.)).text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(e)))
                    .children(sections)
                    .child(div().pt(u(14.)).text_size(u(12.)).text_color(rgb(FAINT)).child(
                        "Chapters you read here update your list there. Tracking syncs with your phone through SyncYomi.",
                    )),
            )
    }
}
