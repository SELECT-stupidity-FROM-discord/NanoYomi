//! The left rail: reading views, your sources, and the app's own pages. Collapses to short labels.

use crate::store::SyncStatus;
use crate::*;
use gpui::{Animation, AnimationExt, StyledImage};

pub const WIDE: f32 = 232.;
pub const NARROW: f32 = 64.;

pub(crate) struct Tip(pub SharedString);

pub(crate) struct Dragged(pub String);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(u(8.)).py(u(4.)).rounded(u(6.)).bg(rgb(SURFACE)).border_1().border_color(rgb(LINE)).text_size(u(12.)).text_color(rgb(TEXT)).child(self.0.clone())
    }
}

/// A row with a short label that stays when collapsed and a long one that hides.
pub(crate) fn item(id: impl Into<gpui::ElementId>, short: AnyElement, label: SharedString, on: bool, narrow: bool) -> gpui::Stateful<gpui::Div> {
    let tip = label.clone();
    div()
        .id(id)
        .flex_none()
        .h(u(32.))
        .px(u(10.))
        .rounded(u(7.))
        .flex()
        .items_center()
        .gap(u(10.))
        .when(narrow, |d| d.justify_center().px_0())
        .cursor_pointer()
        .text_color(rgb(if on { TEXT } else { BODY }))
        .when(on, |d| d.bg(rgb(SURFACE)).font_weight(FontWeight::MEDIUM))
        .hover(|s| s.bg(rgb(HOVER)))
        .child(short)
        .when(!narrow, |d| d.child(div().flex_1().min_w_0().truncate().child(label)))
        .when(narrow, |d| d.tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into()))
}

pub(crate) fn grip() -> impl IntoElement {
    let dots = || div().flex().flex_col().gap(u(2.)).children((0..3).map(|_| div().size(u(2.)).rounded(u(1.)).bg(rgb(FAINT))));
    div()
        .flex_none()
        .flex()
        .gap(u(2.))
        .cursor(gpui::CursorStyle::OpenHand)
        .opacity(0.35)
        .group_hover("source-row", |s| s.opacity(1.))
        .child(dots())
        .child(dots())
}

pub(crate) fn ja(text: &'static str, on: bool, narrow: bool) -> AnyElement {
    div()
        .font_family(CJK)
        .text_size(u(if narrow { 12. } else { 11. }))
        .text_color(rgb(if on { TEXT } else if narrow { MUTED } else { FAINT }))
        .when(!narrow, |d| d.w(u(26.)))
        .child(text)
        .into_any_element()
}

pub(crate) fn label(text: &'static str) -> impl IntoElement {
    div().flex_none().px(u(10.)).pb(u(6.)).font_family(crate::fonts::mono()).text_size(u(10.5)).text_color(rgb(FAINT)).child(text)
}

const MODE_IN: u64 = 480;
const MODE_OUT: u64 = 480;

/// The cover that fades over the window while Nano swaps between manga and light novels.
#[derive(Clone, Copy)]
pub struct ModeAnim {
    to_ln: bool,
    started: std::time::Instant,
    out_at: Option<std::time::Instant>,
}

impl Root {
    /// Covers the window, swaps the mode underneath, then lifts the cover.
    pub(crate) fn switch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_anim.is_some() {
            return;
        }
        let to = !self.store.read(cx).prefs().ln;
        self.mode_anim = Some(ModeAnim { to_ln: to, started: std::time::Instant::now(), out_at: None });
        cx.notify();
        self._mode_task = cx.spawn_in(window, async move |this, cx| {
            let ms = std::time::Duration::from_millis;
            cx.background_executor().timer(ms(MODE_IN)).await;
            this.update_in(cx, |this, window, cx| {
                if to {
                    this.make_ln(window, cx);
                }
                this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.ln = to));
                this.tab = Tab::Library;
                this.close_manga(cx);
                if let Some(a) = &mut this.mode_anim {
                    a.out_at = Some(std::time::Instant::now());
                }
            })
            .ok();
            cx.background_executor().timer(ms(MODE_OUT + 20)).await;
            this.update(cx, |this, cx| {
                this.mode_anim = None;
                cx.notify();
            })
            .ok();
        });
    }

    pub(crate) fn mode_overlay(&self, window: &mut Window) -> Option<impl IntoElement> {
        let a = self.mode_anim?;
        window.request_animation_frame();
        let since = |t: std::time::Instant| t.elapsed().as_secs_f32() * 1000.;
        let ramp = |from: f32, len: f32| gpui::ease_in_out(((since(a.started) - from) / len).clamp(0., 1.));
        let out = a.out_at.map_or(0., |t| gpui::ease_in_out((since(t) / MODE_OUT as f32).clamp(0., 1.)));
        let cover = ramp(0., MODE_IN as f32) * (1. - out);
        let word = ramp(120., 360.) * (1. - out);
        Some(
            div()
                .id("mode-splash")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(u(8.))
                .bg(rgb(BG))
                .opacity(cover)
                .child(div().opacity(word).font_family(crate::fonts::display()).font_weight(FontWeight::BLACK).text_size(u(44.)).child("NANO"))
                .child(div().opacity(word).font_family(crate::fonts::mono()).text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(if a.to_ln { "LIGHT NOVELS" } else { "MANGA" })),
        )
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let on = !self.store.read(cx).prefs().sidebar_collapsed;
        self.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.sidebar_collapsed = on));
        self.sidebar_moves += 1;
        cx.notify();
    }

    pub(crate) fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = self.store.read(cx).prefs().sidebar_collapsed;
        let in_screen = self.migrate.is_none();
        let sources = self.registry.read(cx).browsable(self.store.read(cx));
        let icon_urls: std::collections::HashMap<String, String> = self.registry.read(cx).index.iter().flat_map(|i| i.iter()).map(|e| (e.pkg.clone(), e.icon_url.clone())).collect();
        let icons: Vec<Option<std::sync::Arc<gpui::RenderImage>>> = sources
            .iter()
            .map(|s| {
                let url = icon_urls.get(&s.pkg).filter(|u| !u.is_empty());
                self.icons.get(&self.cache, Some(crate::covers::Fetch::Icon), url.map(String::as_str), |r: &mut Root| &mut r.icons, cx)
            })
            .collect();
        let nav = |tab: Tab, jp: &'static str, en: &'static str, cx: &mut Context<Self>| {
            let on = in_screen && self.tab == tab;
            item(en, ja(jp, on, narrow), en.into(), on, narrow).on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
        };

        let browsing = in_screen && self.tab == Tab::Browse;
        let current = self.browse.read(cx).source_id().map(str::to_string);
        let order: Vec<String> = sources.iter().map(|s| s.id.clone()).collect();
        let source_rows: Vec<_> = sources
            .into_iter()
            .zip(icons)
            .map(|(s, icon)| {
                let on = browsing && current.as_deref() == Some(s.id.as_str());
                let initial = s.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                let short = match (narrow, icon) {
                    (true, icon) => div()
                        .flex_none()
                        .size(u(24.))
                        .rounded(u(12.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if on { ACCENT } else { LINE }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(u(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if on { TEXT } else { MUTED }))
                        .map(|d| match icon {
                            Some(image) => d.child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Cover)),
                            None => d.child(initial),
                        })
                        .into_any_element(),
                    (false, Some(image)) => div()
                        .flex_none()
                        .size(u(18.))
                        .rounded(u(4.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if on { ACCENT } else { LINE }))
                        .child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Cover))
                        .into_any_element(),
                    (false, None) => div().flex_none().size(u(6.)).rounded(u(3.)).bg(rgb(if on { ACCENT } else { LINE })).into_any_element(),
                };
                let name = if s.lang.is_empty() || s.lang == "all" || s.lang == "en" { s.name.clone() } else { format!("{} · {}", s.name, s.lang) };
                let id = s.id.clone();
                let name: SharedString = name.into();
                let (grab, target, ids) = (s.id.clone(), s.id.clone(), order.clone());
                let (here, line) = (s.id.clone(), order.clone());
                item(SharedString::from(format!("side-{}", s.id)), short, name.clone(), on, narrow)
                    .group("source-row")
                    .when(!narrow, |d| d.child(grip()))
                    .on_drag(Dragged(grab), move |_, _, _, cx| cx.new(|_| Tip(name.clone())))
                    .drag_over::<Dragged>(move |d, from, _, _| {
                        let at = |id: &str| line.iter().position(|i| i == id);
                        if from.0 == here {
                            d
                        } else if at(&from.0) < at(&here) {
                            d.border_b_2().border_color(rgb(ACCENT))
                        } else {
                            d.border_t_2().border_color(rgb(ACCENT))
                        }
                    })
                    .on_drop(cx.listener(move |this, from: &Dragged, _, cx| {
                        let mut ids = ids.clone();
                        let (Some(a), Some(b)) = (ids.iter().position(|i| *i == from.0), ids.iter().position(|i| *i == target)) else { return };
                        let moved = ids.remove(a);
                        ids.insert(b, moved);
                        this.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.source_order = ids));
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show(Tab::Browse, window, cx);
                        this.browse.update(cx, |b, cx| b.select(&id, cx));
                    }))
            })
            .collect();

        let sync = match &self.store.read(cx).sync_status {
            SyncStatus::Idle => None,
            SyncStatus::Syncing => Some((0xC9A24Bu32, "Syncing…".to_string())),
            SyncStatus::Done(ms) => Some((0x6FBF73, format!("Synced {}", store::ago(*ms, store::now_ms())))),
            SyncStatus::Failed(_) => Some((ACCENT, "Sync failed".to_string())),
        };

        let search = div()
            .id("side-search")
            .flex_none()
            .h(u(34.))
            .px(u(10.))
            .rounded(u(8.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SURFACE))
            .flex()
            .items_center()
            .gap(u(8.))
            .when(narrow, |d| d.justify_center().px_0())
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER)))
            .child(search_icon())
            .when(!narrow, |d| {
                d.child(div().flex_1().child("Search")).child(
                    div().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).px(u(5.)).border_1().border_color(rgb(LINE)).rounded(u(4.)).child(if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl K" }),
                )
            })
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)));

        let brand = div()
            .flex()
            .items_center()
            .justify_between()
            .when(narrow, |d| d.flex_col().gap(u(10.)))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .when(!narrow, |d| d.px(u(8.)))
                    .child(div().font_family(CJK).font_weight(FontWeight::BLACK).text_size(u(22.)).text_color(rgb(ACCENT)).child("読"))
                    .when(!narrow, |d| d.child(div().font_family(crate::fonts::display()).font_weight(FontWeight::BLACK).text_size(u(24.)).child("NANO"))),
            )
            .child(
                div()
                    .id("collapse")
                    .size(u(28.))
                    .rounded(u(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .child(gpui::svg().path("icons/sidebar.svg").size(u(16.)).text_color(rgb(FAINT)))
                    .tooltip(move |_, cx| {
                        let key = if cfg!(target_os = "macos") { "⌘\\" } else { "Ctrl \\" };
                        cx.new(|_| Tip(format!("{} sidebar ({key})", if narrow { "Expand" } else { "Collapse" }).into())).into()
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            );

        let (from, to) = if narrow { (WIDE, NARROW) } else { (NARROW, WIDE) };
        // The first frame starts settled, so launching doesn't play the slide.
        let from = if self.sidebar_moves == 0 { to } else { from };
        div()
            .flex_none()
            .overflow_hidden()
            .h_full()
            .bg(rgb(0x11100E))
            .border_r_1()
            .border_color(rgb(RULE))
            .flex()
            .flex_col()
            .gap(u(18.))
            .px(u(12.))
            .pt(u(18.))
            .pb(u(14.))
            .child(brand)
            .child(search)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(u(2.))
                    .when(!narrow, |d| d.child(label("READING")))
                    .child(nav(Tab::Library, "書庫", "Library", cx))
                    .child(nav(Tab::Updates, "新着", "Updates", cx))
                    .child(nav(Tab::History, "履歴", "History", cx))
                    .child(nav(Tab::Saved, "取得", "Downloaded", cx)),
            )
            .when(source_rows.is_empty(), |d| d.child(div().flex_1()))
            .when(!source_rows.is_empty(), |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when(!narrow, |d| d.child(label("SOURCES")))
                        .child(div().id("side-sources").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(u(2.)).children(source_rows)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(u(2.))
                    .child(nav(Tab::Downloads, "保存", "Downloads", cx))
                    .child(nav(Tab::Extensions, "拡張", "Extensions", cx))
                    .child(nav(Tab::Settings, "設定", "Settings", cx))
                    .child(item("side-mode", ja("切替", false, narrow), "Switch to LN".into(), false, narrow).on_click(cx.listener(|this, _, window, cx| this.switch_mode(window, cx)))),
            )
            .children(sync.map(|(color, text)| {
                div()
                    .flex()
                    .items_center()
                    .gap(u(8.))
                    .pt(u(8.))
                    .px(u(10.))
                    .when(narrow, |d| d.justify_center().px_0())
                    .border_t_1()
                    .border_color(rgb(RULE))
                    .font_family(crate::fonts::mono())
                    .text_size(u(11.))
                    .text_color(rgb(FAINT))
                    .child(div().flex_none().size(u(6.)).rounded(u(3.)).bg(rgb(color)))
                    .when(!narrow, |d| d.child(text))
            }))
            .with_animation(("sidebar", self.sidebar_moves), Animation::new(SLIDE).with_easing(gpui::ease_out_quint()), move |d, t| {
                let w = from + (to - from) * t;
                set_sidebar_width(w);
                d.w(u(w))
            })
    }
}
