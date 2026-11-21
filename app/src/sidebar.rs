//! The left rail: reading views, your sources, and the app's own pages. Collapses to short labels.

use crate::store::SyncStatus;
use crate::*;
use gpui::{Animation, AnimationExt};

pub const WIDE: f32 = 232.;
pub const NARROW: f32 = 64.;

struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(u(8.)).py(u(4.)).rounded(u(6.)).bg(rgb(SURFACE)).border_1().border_color(rgb(LINE)).text_size(u(12.)).child(self.0.clone())
    }
}

/// A row with a short label that stays when collapsed and a long one that hides.
fn item(id: impl Into<gpui::ElementId>, short: AnyElement, label: SharedString, on: bool, narrow: bool) -> gpui::Stateful<gpui::Div> {
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

fn ja(text: &'static str, on: bool, narrow: bool) -> AnyElement {
    div()
        .font_family(CJK)
        .text_size(u(if narrow { 12. } else { 11. }))
        .text_color(rgb(if on { TEXT } else if narrow { MUTED } else { FAINT }))
        .when(!narrow, |d| d.w(u(26.)))
        .child(text)
        .into_any_element()
}

fn label(text: &'static str) -> impl IntoElement {
    div().flex_none().px(u(10.)).pb(u(6.)).font_family(crate::fonts::mono()).text_size(u(10.5)).text_color(rgb(FAINT)).child(text)
}

impl Root {
    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let on = !self.store.read(cx).prefs().sidebar_collapsed;
        self.store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.sidebar_collapsed = on));
        self.sidebar_moves += 1;
        cx.notify();
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = self.store.read(cx).prefs().sidebar_collapsed;
        let in_screen = self.migrate.is_none();
        let nav = |tab: Tab, jp: &'static str, en: &'static str, cx: &mut Context<Self>| {
            let on = in_screen && self.tab == tab;
            item(en, ja(jp, on, narrow), en.into(), on, narrow).on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
        };

        let browsing = in_screen && self.tab == Tab::Browse;
        let current = self.browse.read(cx).source_id().map(str::to_string);
        let sources = self.registry.read(cx).browsable(self.store.read(cx));
        let source_rows: Vec<_> = sources
            .into_iter()
            .map(|s| {
                let on = browsing && current.as_deref() == Some(s.id.as_str());
                let initial = s.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                let short = if narrow {
                    div()
                        .size(u(24.))
                        .rounded(u(12.))
                        .border_1()
                        .border_color(rgb(if on { ACCENT } else { LINE }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(u(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if on { TEXT } else { MUTED }))
                        .child(initial)
                        .into_any_element()
                } else {
                    div().flex_none().size(u(6.)).rounded(u(3.)).bg(rgb(if on { ACCENT } else { LINE })).into_any_element()
                };
                let name = if s.lang.is_empty() || s.lang == "all" || s.lang == "en" { s.name.clone() } else { format!("{} · {}", s.name, s.lang) };
                let id = s.id.clone();
                item(SharedString::from(format!("side-{}", s.id)), short, name.into(), on, narrow).on_click(cx.listener(move |this, _, window, cx| {
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
                    .child(nav(Tab::History, "履歴", "History", cx)),
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
                    .child(nav(Tab::Settings, "設定", "Settings", cx)),
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
