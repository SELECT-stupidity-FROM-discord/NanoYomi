//! ⌘K for light novels: jump to a novel in your library, a source, or a page, or search the current source.

use super::{LnApp, Screen};
use crate::input::{InputEvent, TextInput};
use crate::{BODY, FAINT, HOVER, LINE, MUTED, SURFACE, TEXT, u};
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, rgb,
};

#[derive(Clone)]
enum Go {
    Novel(String, String),
    Source(String),
    Screen(Screen),
    Search(String),
}

struct Entry {
    group: &'static str,
    label: String,
    hint: String,
    go: Go,
}

pub struct LnPalette {
    input: Entity<TextInput>,
    selected: usize,
    _sub: Subscription,
}

impl LnApp {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Novels, sources, or something to do…", cx);
            i.icon = false;
            i
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, _: &InputEvent, window, cx| this.pick(None, window, cx));
        self.palette = Some(LnPalette { input, selected: 0, _sub: sub });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn entries(&self, cx: &App) -> Vec<Entry> {
        let Some(p) = &self.palette else { return vec![] };
        let query = p.input.read(cx).text.trim().to_string();
        let q = query.to_lowercase();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut out: Vec<Entry> = self
            .store
            .read(cx)
            .library()
            .into_iter()
            .filter(|n| hit(&n.name) || hit(&n.source_name))
            .map(|n| Entry {
                group: "YOUR NOVELS",
                label: n.name.clone(),
                hint: match n.unread() {
                    0 => n.source_name.clone(),
                    k => format!("{k} new"),
                },
                go: Go::Novel(n.source.clone(), n.path.clone()),
            })
            .collect();
        let plugins = self.plugins.read(cx);
        out.extend(plugins.installed.iter().filter(|e| hit(&format!("Browse {}", e.name))).map(|e| Entry {
            group: "BROWSE",
            label: format!("Browse {}", e.name),
            hint: e.lang.trim_matches('\u{200e}').to_string(),
            go: Go::Source(e.id.clone()),
        }));
        let pages = [
            ("Library", Screen::Library, ""),
            ("Updates", Screen::Updates, ""),
            ("History", Screen::History, ""),
            ("Extensions", Screen::Extensions, ""),
            ("Settings", Screen::Settings, if cfg!(target_os = "macos") { "⌘," } else { "" }),
        ];
        out.extend(pages.into_iter().filter(|(l, _, _)| hit(l)).map(|(l, screen, hint)| Entry { group: "GO TO", label: l.into(), hint: hint.into(), go: Go::Screen(screen) }));
        if !query.is_empty()
            && let Some(name) = self.browse.read(cx).source_id().and_then(|id| plugins.entry(id)).map(|e| e.name.clone())
        {
            out.push(Entry { group: "SEARCH", label: format!("Search {name} for “{query}”"), hint: "⏎".into(), go: Go::Search(query) });
        }
        out
    }

    fn pick(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.entries(cx);
        let i = index.unwrap_or_else(|| self.palette.as_ref().map_or(0, |p| p.selected)).min(entries.len().saturating_sub(1));
        let Some(go) = entries.get(i).map(|e| e.go.clone()) else { return };
        self.close_palette(window, cx);
        match go {
            Go::Novel(source, path) => self.open_novel(&source, &path, window, cx),
            Go::Source(id) => {
                self.go(Screen::Browse, cx);
                self.browse.update(cx, |b, cx| b.select(&id, cx));
            }
            Go::Screen(screen) => self.go(screen, cx),
            Go::Search(query) => {
                self.go(Screen::Browse, cx);
                let search = self.browse.read(cx).search();
                search.update(cx, |s, cx| {
                    s.text = query;
                    cx.emit(InputEvent::Submit);
                });
            }
        }
    }

    pub(super) fn palette(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = self.palette.as_ref()?;
        let entries = self.entries(cx);
        let selected = p.selected.min(entries.len().saturating_sub(1));
        let mut rows: Vec<AnyElement> = vec![];
        let mut last = "";
        for (i, e) in entries.into_iter().enumerate() {
            if e.group != last {
                last = e.group;
                rows.push(div().px(u(10.)).pt(u(10.)).pb(u(4.)).font_family(crate::fonts::mono()).text_size(u(10.5)).text_color(rgb(FAINT)).child(e.group).into_any_element());
            }
            let on = i == selected;
            rows.push(
                div()
                    .id(("ln-pick", i))
                    .flex()
                    .items_center()
                    .gap(u(10.))
                    .px(u(10.))
                    .py(u(8.))
                    .rounded(u(8.))
                    .text_color(rgb(if on { TEXT } else { BODY }))
                    .when(on, |d| d.bg(rgb(HOVER)))
                    .hover(|d| d.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .child(div().flex_1().min_w_0().truncate().child(e.label))
                    .child(div().flex_none().font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(e.hint))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(Some(i), window, cx)))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();
        Some(
            div()
                .id("ln-palette")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000008C))
                .flex()
                .justify_center()
                .pt(u(110.))
                .occlude()
                .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                .capture_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, window, cx| {
                    let Some(p) = this.palette.as_mut() else { return };
                    match ev.keystroke.key.as_str() {
                        "escape" => this.close_palette(window, cx),
                        "down" => p.selected = selected + 1,
                        "up" => p.selected = selected.saturating_sub(1),
                        _ => {
                            // Typing changes the list, so start from the top again.
                            p.selected = 0;
                            return cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .id("ln-palette-box")
                        .w(u(620.))
                        .max_w_full()
                        .max_h(u(520.))
                        .flex()
                        .flex_col()
                        .rounded(u(14.))
                        .border_1()
                        .border_color(rgb(LINE))
                        .bg(rgb(SURFACE))
                        .shadow_lg()
                        .overflow_hidden()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(div().p(u(10.)).border_b_1().border_color(rgb(LINE)).child(p.input.clone()))
                        .child(
                            div()
                                .id("ln-palette-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p(u(6.))
                                .when(empty, |d| d.child(div().p(u(24.)).flex().justify_center().text_color(rgb(MUTED)).child("Nothing matches.")))
                                .children(rows),
                        ),
                )
                // The state is dropped while closed, so this plays on every open.
                .with_animation("ln-palette-in", Animation::new(std::time::Duration::from_millis(160)).with_easing(gpui::ease_out_quint()), |d, t| d.opacity(t).pt(u(110. + 12. * (1. - t)))),
        )
    }
}
