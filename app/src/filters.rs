//! A source's search filters, drawn as a side panel in Browse.
//! The host sends them as JSON and the same JSON goes back with each search.

use crate::browse::Browse;
use crate::input::TextInput;
use crate::{ACCENT_TEXT, BG, FAINT, HOVER, LINE, MUTED, TEXT, u};
use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, rgb,
};
use serde_json::{Value, json};
use std::collections::HashMap;

/// A top-level index, and an index inside its group.
pub type Path = (usize, Option<usize>);

pub fn get_mut(filters: &mut [Value], (i, j): Path) -> Option<&mut Value> {
    let f = filters.get_mut(i)?;
    match j {
        Some(j) => f["filters"].get_mut(j),
        None => Some(f),
    }
}

pub fn paths(filters: &[Value]) -> Vec<(Path, &Value)> {
    filters
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            std::iter::once(((i, None), f))
                .chain(f["filters"].as_array().into_iter().flatten().enumerate().map(move |(j, c)| ((i, Some(j)), c)))
        })
        .collect()
}

pub fn text_inputs(filters: &[Value], cx: &mut Context<Browse>) -> HashMap<Path, Entity<TextInput>> {
    paths(filters)
        .into_iter()
        .filter(|(_, f)| f["type"] == "text")
        .map(|(p, f)| {
            let name = f["name"].as_str().unwrap_or("").to_string();
            let state = f["state"].as_str().unwrap_or("").to_string();
            (p, cx.new(|cx| {
                let mut t = TextInput::new(name, cx);
                t.text = state;
                t
            }))
        })
        .collect()
}

/// The count on the Filters button.
pub fn changed(filters: &[Value], defaults: &[Value]) -> usize {
    let (a, b) = (paths(filters), paths(defaults));
    a.iter().zip(b.iter()).filter(|((_, x), (_, y))| x["type"] != "group" && x["state"] != y["state"]).count()
}

/// `value` is the chip's option, for select and sort.
pub fn next_state(f: &Value, value: usize) -> Value {
    match f["type"].as_str() {
        Some("checkbox") => json!(!f["state"].as_bool().unwrap_or(false)),
        Some("tristate") => json!((f["state"].as_u64().unwrap_or(0) + 1) % 3),
        Some("select") => json!(value),
        // Picking the current sort flips it. Another sort starts descending, like Mihon.
        Some("sort") => match f["state"]["index"].as_u64() {
            Some(i) if i as usize == value => json!({ "index": value, "ascending": !f["state"]["ascending"].as_bool().unwrap_or(false) }),
            _ => json!({ "index": value, "ascending": false }),
        },
        _ => f["state"].clone(),
    }
}

fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, on: bool, mark: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(u(30.))
        .px(u(12.))
        .rounded(u(15.))
        .flex()
        .items_center()
        .flex_none()
        .text_size(u(13.))
        .cursor_pointer()
        .border_1()
        .when(on, |d| d.bg(rgb(TEXT)).border_color(rgb(TEXT)).text_color(rgb(BG)).font_weight(FontWeight::SEMIBOLD))
        .when(!on, |d| d.border_color(rgb(LINE)).text_color(rgb(MUTED)).hover(|s| s.bg(rgb(HOVER))))
        .gap(u(6.))
        .when(mark, |d| d.child(crate::check(13., BG)))
        .child(label.into())
}

fn title(name: &str) -> impl IntoElement {
    div().text_size(u(13.)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(TEXT)).child(name.to_string())
}

fn id(p: Path, k: usize) -> SharedString {
    format!("f{}-{}-{k}", p.0, p.1.map_or(-1, |j| j as i64)).into()
}

fn control(p: Path, f: &Value, inputs: &HashMap<Path, Entity<TextInput>>, cx: &mut Context<Browse>) -> AnyElement {
    let name = f["name"].as_str().unwrap_or("");
    let e = cx.entity().downgrade();
    let set = |k: usize, _: &mut Context<Browse>| {
        let e = e.clone();
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            let _ = e.update(cx, |this, cx| this.set_filter(p, k, cx));
        }
    };
    let options = |f: &Value| f["values"].as_array().cloned().unwrap_or_default();
    match f["type"].as_str().unwrap_or("") {
        "header" => div().pt(u(6.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child(name.to_uppercase()).into_any_element(),
        "separator" => div().h(u(1.)).bg(rgb(LINE)).into_any_element(),
        "text" => div()
            .flex()
            .flex_col()
            .gap(u(8.))
            .child(title(name))
            .children(inputs.get(&p).cloned())
            .into_any_element(),
        "checkbox" => {
            let on = f["state"].as_bool().unwrap_or(false);
            chip(id(p, 0), name.to_string(), on, on).on_click(set(0, cx)).into_any_element()
        }
        "tristate" => {
            let state = f["state"].as_u64().unwrap_or(0);
            let c = chip(id(p, 0), if state == 2 { format!("✕ {name}") } else { name.to_string() }, state == 1, state == 1);
            let c = if state == 2 { c.border_color(rgb(ACCENT_TEXT)).text_color(rgb(ACCENT_TEXT)).line_through() } else { c };
            c.on_click(set(0, cx)).into_any_element()
        }
        kind @ ("select" | "sort") => {
            let values = options(f);
            let (current, ascending) = match kind {
                "select" => (f["state"].as_u64().map(|v| v as usize), false),
                _ => (f["state"]["index"].as_u64().map(|v| v as usize), f["state"]["ascending"].as_bool().unwrap_or(false)),
            };
            let chips: Vec<_> = values
                .iter()
                .enumerate()
                .map(|(k, v)| {
                    let label = v.as_str().unwrap_or("").to_string();
                    let on = current == Some(k);
                    let label = if kind == "sort" && on { format!("{label} {}", if ascending { "↑" } else { "↓" }) } else { label };
                    chip(id(p, k), label, on, false).on_click(set(k, cx))
                })
                .collect();
            div().flex().flex_col().gap(u(8.)).child(title(name)).child(div().flex().flex_wrap().gap(u(6.)).children(chips)).into_any_element()
        }
        "group" => {
            let children: Vec<Value> = f["filters"].as_array().cloned().unwrap_or_default();
            // Groups of checkboxes (genres, tags) read best as one cloud of chips.
            let chips = children.iter().all(|c| matches!(c["type"].as_str(), Some("checkbox" | "tristate")));
            let items: Vec<_> = children.iter().enumerate().map(|(j, c)| control((p.0, Some(j)), c, inputs, cx)).collect();
            div()
                .flex()
                .flex_col()
                .gap(u(8.))
                .child(title(name))
                .child(if chips { div().flex().flex_wrap().gap(u(6.)).children(items) } else { div().flex().flex_col().gap(u(12.)).children(items) })
                .into_any_element()
        }
        _ => div().into_any_element(),
    }
}

pub fn list(filters: &[Value], inputs: &HashMap<Path, Entity<TextInput>>, cx: &mut Context<Browse>) -> AnyElement {
    div().flex().flex_col().gap(u(16.)).children(filters.iter().enumerate().map(|(i, f)| control((i, None), f, inputs, cx))).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_cycle_and_paths_reach_groups() {
        assert_eq!(next_state(&json!({"type":"tristate","state":2}), 0), json!(0));
        assert_eq!(next_state(&json!({"type":"checkbox","state":false}), 0), json!(true));
        let sort = json!({"type":"sort","state":{"index":1,"ascending":false}});
        assert_eq!(next_state(&sort, 1), json!({"index":1,"ascending":true}));
        assert_eq!(next_state(&sort, 0), json!({"index":0,"ascending":false}));
        let mut fs = vec![json!({"type":"header"}), json!({"type":"group","filters":[{"type":"checkbox","state":false}]})];
        let defaults = fs.clone();
        *get_mut(&mut fs, (1, Some(0))).unwrap().get_mut("state").unwrap() = json!(true);
        assert_eq!(paths(&fs).len(), 3);
        assert_eq!(changed(&fs, &defaults), 1);
    }
}
