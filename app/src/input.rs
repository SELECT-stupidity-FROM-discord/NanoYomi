//! A one-line text field, since GPUI doesn't come with one.
// No IME yet.

use crate::u;
use crate::{FAINT, LINE, TEXT, ACCENT};
use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    Hsla, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ShapedLine, SharedString, StatefulInteractiveElement, Styled, TextRun, Window, canvas, div, rgb,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

pub enum InputEvent {
    Submit,
}

pub struct TextInput {
    focus: FocusHandle,
    pub text: String,
    pub placeholder: SharedString,
    pub icon: bool,
    pub numeric: bool,
    pub compact: bool,
    /// Shows dots instead of the text, for passwords.
    pub secret: bool,
    back: usize,
    anchor: Option<usize>,
    dragging: bool,
    blink_on: bool,
    blink_gen: u64,
    blink_task: Option<gpui::Task<()>>,
    shaped: Rc<RefCell<Option<(Pixels, ShapedLine)>>>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        TextInput {
            focus: cx.focus_handle(),
            text: String::new(),
            placeholder: placeholder.into(),
            icon: true,
            numeric: false,
            compact: false,
            secret: false,
            back: 0,
            anchor: None,
            dragging: false,
            blink_on: true,
            blink_gen: 0,
            blink_task: None,
            shaped: Rc::default(),
        }
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.reset_blink(cx);
    }

    /// Drives the 2 Hz cursor blink with a timer instead of a per-frame display animation.
    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_on = true;
        self.blink_gen = self.blink_gen.wrapping_add(1);
        let stamp = self.blink_gen;
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if this.blink_gen != stamp {
                            return false;
                        }
                        this.blink_on = !this.blink_on;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn byte_at(&self, back: usize) -> usize {
        let n = self.text.chars().count();
        self.text.char_indices().nth(n - back.min(n)).map_or(self.text.len(), |(i, _)| i)
    }

    fn caret(&self) -> usize {
        self.byte_at(self.back)
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let (a, b) = (self.byte_at(self.anchor?), self.caret());
        (a != b).then_some((a.min(b), a.max(b)))
    }

    /// What's drawn, which is dots for a secret.
    fn shown(&self) -> String {
        if self.secret { "•".repeat(self.text.chars().count()) } else { self.text.clone() }
    }

    fn back_at(&self, x: Pixels) -> usize {
        let at = self.shaped.borrow().as_ref().map(|(left, line)| line.closest_index_for_x(x - *left));
        let shown = self.shown();
        at.filter(|&i| shown.is_char_boundary(i)).map_or(0, |i| shown[i..].chars().count())
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.replace_range(a..b, "");
        self.back = self.text[a..].chars().count();
        self.anchor = None;
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        let at = self.caret();
        self.text.insert_str(at, s);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let len = self.text.chars().count();
        self.back = self.back.min(len);
        if let Some((a, b)) = self.selection() {
            let collapsed = match k.key.as_str() {
                "backspace" | "delete" => self.delete_selection().then_some(self.back),
                "left" => Some(self.text[a..].chars().count()),
                "home" => Some(len),
                "right" => Some(self.text[b..].chars().count()),
                "end" => Some(0),
                _ => None,
            };
            if let Some(back) = collapsed {
                (self.back, self.anchor) = (back, None);
                self.reset_blink(cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        let mut keep_selection = false;
        match (k.key.as_str(), k.modifiers.platform || k.modifiers.control) {
            ("backspace", true) | ("escape", _) => {
                self.text.clear();
                self.back = 0;
            }
            ("backspace", false) => {
                let at = self.caret();
                if let Some(c) = self.text[..at].chars().next_back() {
                    self.text.remove(at - c.len_utf8());
                }
            }
            ("delete", _) => {
                let at = self.caret();
                if at < self.text.len() {
                    self.text.remove(at);
                    self.back -= 1;
                }
            }
            ("left", false) => self.back = (self.back + 1).min(len),
            ("right", false) => self.back = self.back.saturating_sub(1),
            ("left", true) | ("home", _) => self.back = len,
            ("right", true) | ("end", _) => self.back = 0,
            ("enter", _) => cx.emit(InputEvent::Submit),
            ("a", true) => {
                keep_selection = len > 0;
                (self.anchor, self.back) = (keep_selection.then_some(len), 0);
            }
            ("v", true) => {
                if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    let numeric = self.numeric;
                    let line: String = t.lines().next().unwrap_or("").chars().filter(|c| !numeric || c.is_ascii_digit()).collect();
                    self.insert(&line);
                }
            }
            (_, false) => match &k.key_char {
                Some(ch) if !ch.chars().any(char::is_control) && (!self.numeric || ch.chars().all(|c| c.is_ascii_digit())) => {
                    let flip = cfg!(target_os = "macos") && window.capslock().on && ch.chars().count() == 1 && ch.chars().all(char::is_alphabetic);
                    if flip {
                        let flipped: String = ch.chars().flat_map(|c| if c.is_uppercase() { c.to_lowercase().collect::<Vec<_>>() } else { c.to_uppercase().collect() }).collect();
                        self.insert(&flipped);
                    } else {
                        self.insert(ch);
                    }
                }
                _ => return,
            },
            _ => return,
        }
        if !keep_selection {
            self.anchor = None;
        }
        self.reset_blink(cx);
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        if focused && self.blink_task.is_none() {
            self.reset_blink(cx);
        } else if !focused && self.blink_task.is_some() {
            self.blink_task = None;
            self.blink_on = true;
            self.blink_gen = self.blink_gen.wrapping_add(1);
        }
        let selection = self.selection();
        let (from, to) = selection.unwrap_or((self.caret(), self.caret()));
        let shown = self.shown();
        // Moves a byte offset in the text onto the dots.
        let at = |b: usize| if self.secret { self.text[..b].chars().count() * '•'.len_utf8() } else { b };
        let part = |range: std::ops::Range<usize>| SharedString::from(shown[range].to_string());
        let (before, middle, after) = (part(0..at(from)), part(at(from)..at(to)), part(at(to)..shown.len()));
        div()
            .id("input")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                this.back = this.back_at(ev.position.x);
                (this.anchor, this.dragging) = (Some(this.back), true);
                this.reset_blink(cx);
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.dragging && ev.pressed_button == Some(MouseButton::Left) {
                    this.back = this.back_at(ev.position.x);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                this.dragging = false;
                if this.anchor == Some(this.back) {
                    this.anchor = None;
                }
                cx.notify();
            }))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                this.dragging = false;
                if this.anchor == Some(this.back) {
                    this.anchor = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.focus(window, cx)))
            .h(u(if self.compact { 30. } else { 40. }))
            .px(u(if self.compact { 12. } else { 16. }))
            .rounded(u(if self.compact { 15. } else { 20. }))
            .border_1()
            .border_color(rgb(if focused { 0x5A544B } else { LINE }))
            .bg(rgb(0x141210))
            .flex()
            .items_center()
            .gap_2()
            .cursor_text()
            .children(self.icon.then(crate::search_icon))
            .child({
                let blink_on = self.blink_on;
                let caret = move || {
                    div().flex_none().w(u(2.)).h(u(18.)).bg(rgb(ACCENT)).opacity(if blink_on { 1. } else { 0. })
                };
                let empty = self.text.is_empty();
                let highlight: Hsla = rgb(ACCENT).into();
                let (shaped, text) = (self.shaped.clone(), SharedString::from(shown.clone()));
                let measure = canvas(|_, _, _| (), move |bounds, _, window, _| {
                    if text.is_empty() {
                        *shaped.borrow_mut() = None;
                        return;
                    }
                    let style = window.text_style();
                    let run = TextRun { len: text.len(), font: style.font(), color: style.color, background_color: None, underline: None, strikethrough: None };
                    let line = window.text_system().shape_line(text, style.font_size.to_pixels(window.rem_size()), &[run], None);
                    *shaped.borrow_mut() = Some((bounds.origin.x, line));
                })
                .absolute()
                .size_full();
                let plain = |s: SharedString| div().flex_none().text_color(rgb(TEXT)).child(s);
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(measure)
                    .children((focused && empty).then(caret))
                    .children(empty.then(|| div().flex_none().text_color(rgb(FAINT)).child(self.placeholder.clone())))
                    .children((!empty).then(|| plain(before)))
                    .children(selection.map(|_| plain(middle).bg(highlight.opacity(0.35))))
                    .children((focused && !empty && selection.is_none()).then(caret))
                    .children((!empty).then(|| plain(after)))
            })
    }
}
