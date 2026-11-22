use crate::catalog::small_button;
use crate::input::{InputEvent, TextInput};
use crate::store::{Store, SyncStatus};
use crate::{FAINT, HOVER, LINE, MUTED, RULE, TEXT, ACCENT_TEXT, fonts, heading, u};
use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, rgb, uniform_list,
};

const PAD: f32 = 40.;

pub struct Settings {
    store: Entity<Store>,
    /// Shows "Cleared" on the button until the next visit.
    covers_cleared: bool,
    pages_cleared: bool,
    sync_host: Entity<TextInput>,
    sync_key: Entity<TextInput>,
    solver: Entity<TextInput>,
    new_category: Entity<TextInput>,
    /// The tracker waiting on the browser, and what the last sign-in said.
    signing_in: Option<i64>,
    track_note: String,
    /// What the last export or import said.
    backup_note: String,
    font_open: Option<usize>,
    font_files: Vec<String>,
    font_matches: Vec<&'static str>,
    font_search: Entity<TextInput>,
    downloading: Vec<&'static str>,
    font_failed: Option<(&'static str, String)>,
}

impl Settings {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let plain = |placeholder: &str, text: &str, cx: &mut Context<Self>| {
            let (placeholder, text) = (placeholder.to_string(), text.to_string());
            cx.new(|cx| {
                let mut t = TextInput::new(placeholder, cx);
                t.text = text;
                t.icon = false;
                t
            })
        };
        let field = |placeholder: &str, text: &str, cx: &mut Context<Self>| {
            let input = plain(placeholder, text, cx);
            cx.subscribe(&input, |this, _, _: &InputEvent, cx| this.sync_now(cx)).detach();
            input
        };
        let cfg = store.read(cx).sync_config().clone();
        let sync_host = field("Server, e.g. http://192.168.0.3:8282", &cfg.host, cx);
        let sync_key = field("API key", &cfg.key, cx);
        let url = store.read(cx).solver_url().to_string();
        let solver = plain("Optional, e.g. http://192.168.0.5:8191", &url, cx);
        // The field has no change event, so save it whenever it redraws.
        cx.observe(&solver, |this, input, cx| {
            let url = input.read(cx).text.clone();
            this.store.update(cx, |s, cx| s.set_solver_url(&url, cx));
        })
        .detach();
        let new_category = plain("New category name", "", cx);
        cx.subscribe(&new_category, |this, _, _: &InputEvent, cx| this.add_category(cx)).detach();
        let font_search = plain("Search Google Fonts", "", cx);
        cx.observe(&font_search, |this, _, cx| this.filter_fonts(cx)).detach();
        Settings { store, covers_cleared: false, pages_cleared: false, sync_host, sync_key, solver, new_category, signing_in: None, track_note: String::new(), backup_note: String::new(), font_open: None, font_files: vec![], font_matches: vec![], font_search, downloading: vec![], font_failed: None }
    }

    fn add_category(&mut self, cx: &mut Context<Self>) {
        let name = self.new_category.read(cx).text.clone();
        self.store.update(cx, |s, cx| s.add_category(&name, cx));
        self.new_category.update(cx, |i, cx| {
            i.text.clear();
            cx.notify();
        });
        crate::sync::run(self.store.clone(), cx);
    }

    /// Opens the site's sign-in page and waits on its own thread for the browser to come back.
    fn sign_in(&mut self, tracker: i64, cx: &mut Context<Self>) {
        let (url, state, verifier) = match crate::track::login_url(tracker) {
            Ok(x) => x,
            Err(e) => {
                self.track_note = e;
                return cx.notify();
            }
        };
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::track::finish_login(tracker, &state, &verifier));
        });
        cx.open_url(&url);
        self.signing_in = Some(tracker);
        self.track_note = format!("Finish signing in to {} in your browser…", crate::track::name(tracker));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Signing in stopped.".into()));
            let _ = this.update(cx, |this, cx| {
                this.signing_in = None;
                this.track_note = match result {
                    Ok(login) => {
                        this.store.update(cx, |s, cx| s.set_login(tracker, Some(login), cx));
                        String::new()
                    }
                    Err(e) => e,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn export_backup(&mut self, cx: &mut Context<Self>) {
        let (bytes, count) = match self.store.read(cx).backup() {
            Ok(x) => x,
            Err(e) => {
                self.backup_note = e;
                return cx.notify();
            }
        };
        let home = std::env::home_dir().unwrap_or_default();
        let rx = cx.prompt_for_new_path(&home, Some("nano.tachibk"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let note = match std::fs::write(&path, bytes) {
                Ok(()) => format!("Saved {count} series to {}", path.display()),
                Err(e) => format!("Couldn't save the backup: {e}"),
            };
            let _ = this.update(cx, |this, cx| {
                this.backup_note = note;
                cx.notify();
            });
        })
        .detach();
    }

    fn import_backup(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                let restored = std::fs::read(&path).map_err(|e| format!("Couldn't open the file: {e}")).and_then(|b| this.store.update(cx, |s, cx| s.restore(b, cx)));
                this.backup_note = match restored {
                    Ok(n) => {
                        crate::sync::run(this.store.clone(), cx);
                        format!("Restored {n} series")
                    }
                    Err(e) => e,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn scan_fonts(&mut self, cx: &mut Context<Self>) {
        fonts::load(cx);
        self.font_files = fonts::files();
        self.filter_fonts(cx);
    }

    fn filter_fonts(&mut self, cx: &mut Context<Self>) {
        let query = self.font_search.read(cx).text.trim().to_lowercase();
        self.font_matches = fonts::google().iter().copied().filter(|n| n.to_lowercase().contains(&query)).collect();
        cx.notify();
    }

    fn download_font(&mut self, name: &'static str, cx: &mut Context<Self>) {
        if self.downloading.contains(&name) {
            return;
        }
        self.downloading.push(name);
        self.font_failed = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { fonts::download(name) }).await;
            let _ = this.update(cx, |this, cx| {
                this.downloading.retain(|n| *n != name);
                match result {
                    Ok(_) => this.scan_fonts(cx),
                    Err(e) => this.font_failed = Some((name, e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_font(&mut self, slot: usize, family: Option<&'static str>, cx: &mut Context<Self>) {
        let key = fonts::SLOTS[slot].0.to_string();
        self.font_open = None;
        self.store.update(cx, |s, cx| {
            s.edit_prefs(cx, |p| match family {
                Some(f) => drop(p.fonts.insert(key, f.to_string())),
                None => drop(p.fonts.remove(&key)),
            })
        });
        fonts::choose(&self.store.read(cx).prefs().fonts);
        cx.refresh_windows();
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        let (host, key) = (self.sync_host.read(cx).text.clone(), self.sync_key.read(cx).text.clone());
        self.store.update(cx, |s, cx| s.set_sync_account(&host, &key, cx));
        crate::sync::run(self.store.clone(), cx);
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clear = small_button("covers-clear", if self.covers_cleared { "Cleared" } else { "Clear" }, false).on_click(cx.listener(|this, _, _, cx| {
            crate::covers::clear_cache();
            this.covers_cleared = true;
            cx.notify();
        }));
        let storage = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Cover cache"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                        "Covers are kept for a week, so sites aren’t asked (or verified) again. Clear to fetch every cover fresh.",
                    )),
            )
            .child(clear);
        let cap = self.store.read(cx).page_cache_mb();
        let caps = crate::page_cache::CAPS_MB.map(|mb| {
            let label = if mb >= 1024 { format!("{} GB", mb / 1024) } else { format!("{mb} MB") };
            let store = self.store.clone();
            small_button(("page-cap", mb as usize), "", mb == cap)
                .child(label)
                .on_click(move |_, _, cx| store.update(cx, |s, cx| s.set_page_cache_mb(mb, cx)))
        });
        let clear_pages = small_button("pages-clear", if self.pages_cleared { "Cleared" } else { "Clear" }, false).on_click(cx.listener(|this, _, _, cx| {
            crate::page_cache::clear();
            this.pages_cleared = true;
            cx.notify();
        }));
        let pages = div()
            .w_full()
            .py(u(14.))
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Page cache"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                        "Read pages stay on disk, up to the limit.",
                    )),
            )
            .child(div().flex_none().flex().gap(u(6.)).children(caps))
            .child(clear_pages);
        let categories = self.store.read(cx).categories();
        let category_rows = categories.into_iter().map(|c| {
            let order = c.order;
            div()
                .py(u(10.))
                .flex()
                .items_center()
                .gap_4()
                .border_b_1()
                .border_color(rgb(RULE))
                .child(div().flex_1().min_w_0().truncate().child(c.name.clone()))
                .child(small_button(("delete-category", order as usize), "Delete", false).on_click(cx.listener(move |this, _, _, cx| {
                    this.store.update(cx, |s, cx| s.remove_category(order, cx));
                    crate::sync::run(this.store.clone(), cx);
                })))
        });
        let categories = div()
            .w_full()
            .border_t_1()
            .border_color(rgb(RULE))
            .children(category_rows)
            .child(
                div()
                    .py(u(12.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().max_w(u(420.)).child(self.new_category.clone()))
                    .child(small_button("add-category", "Add", true).on_click(cx.listener(|this, _, _, cx| this.add_category(cx)))),
            )
            .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                "Shelves for your library, shown as tabs above it. Put a series in one from its page. They sync with your phone through SyncYomi.",
            ));
        let auto = self.store.read(cx).prefs().auto_download;
        let auto_chip = |id: &'static str, label: &'static str, value: bool, store: Entity<Store>| {
            small_button(id, label, auto == value).on_click(move |_, _, cx| store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.auto_download = value)))
        };
        let folder = crate::downloads::dir().map(|d| d.display().to_string()).unwrap_or_default();
        let downloads = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Download new chapters"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(format!(
                        "Downloads new library chapters to {folder}"
                    ))),
            )
            .child(div().flex_none().flex().gap(u(6.)).child(auto_chip("auto-off", "Off", false, self.store.clone())).child(auto_chip("auto-on", "On", true, self.store.clone())));
        let prefs = self.store.read(cx).prefs();
        let current = (prefs.vertical_reader, prefs.right_to_left && !prefs.vertical_reader);
        let mode_chip = |id: &'static str, label: &'static str, value: (bool, bool), store: Entity<Store>| {
            small_button(id, label, current == value).on_click(move |_, _, cx| {
                store.update(cx, |s, cx| {
                    s.edit_prefs(cx, |p| {
                        p.vertical_reader = value.0;
                        if !value.0 {
                            p.right_to_left = value.1;
                        }
                    })
                })
            })
        };
        let reading = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Reading mode"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                        "How chapters open. Press V in the reader to switch between paged and vertical.",
                    )),
            )
            .child(div().flex_none().flex().gap(u(6.)).child(mode_chip("mode-ltr", "Left to right", (false, false), self.store.clone())).child(mode_chip("mode-rtl", "Right to left", (false, true), self.store.clone())).child(mode_chip("mode-vertical", "Vertical", (true, false), self.store.clone())));
        let full = self.store.read(cx).prefs().reader_fullscreen;
        let full_chip = |id: &'static str, label: &'static str, value: bool, store: Entity<Store>| {
            small_button(id, label, full == value).on_click(move |_, _, cx| store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.reader_fullscreen = value)))
        };
        let fullscreen = div()
            .w_full()
            .py(u(14.))
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Fullscreen reader"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child("Go fullscreen when a chapter opens, and back when it closes. Press F in the reader to toggle it.")),
            )
            .child(div().flex_none().flex().gap(u(6.)).child(full_chip("full-off", "Off", false, self.store.clone())).child(full_chip("full-on", "On", true, self.store.clone())));

        let updates = {
            let s = self.store.read(cx);
            let off = s.prefs().updates_off;
            let (found, note) = (s.update.clone(), s.update_note);
            let have = crate::update::current().map(|v| v.to_string()).unwrap_or_default();
            let chip = |id: &'static str, label: &'static str, value: bool| {
                let store = self.store.clone();
                small_button(id, label, off == value).on_click(move |_, _, cx| store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.updates_off = value)))
            };
            let status = match &found {
                Some(u) => format!("Nano {} is available", u.version),
                None if note.is_empty() => format!("You have Nano {have}"),
                None => note.to_string(),
            };
            let store = self.store.clone();
            div()
                .w_full()
                .border_t_1()
                .border_b_1()
                .border_color(rgb(RULE))
                .flex()
                .flex_col()
                .child(
                    div()
                        .w_full()
                        .py(u(14.))
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(u(4.))
                                .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Check for updates"))
                                .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child("Look for a new Nano release when the app starts, at most once a day. A banner links to the download. Nothing is installed for you.")),
                        )
                        .child(div().flex_none().flex().gap(u(6.)).child(chip("updates-off", "Off", true)).child(chip("updates-on", "On", false))),
                )
                .child(
                    div()
                        .w_full()
                        .py(u(14.))
                        .border_t_1()
                        .border_color(rgb(RULE))
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(div().flex_1().min_w_0().font_family(fonts::mono()).text_size(u(13.)).child(status))
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .gap(u(8.))
                                .children(found.map(|u| small_button("update-download", "Download", true).on_click(move |_, _, cx| cx.open_url(&u.url))))
                                .child(small_button("update-check", "Check now", false).on_click(move |_, _, cx| crate::update::check(store.clone(), true, cx))),
                        ),
                )
        };

        let font_rows = fonts::SLOTS.into_iter().enumerate().map(|(slot, (key, label, default))| {
            let current = self.store.read(cx).prefs().fonts.get(key).cloned();
            let open = self.font_open == Some(slot);
            let list = open.then(|| {
                let lead = usize::from(self.font_search.read(cx).text.trim().is_empty());
                let rows = uniform_list(
                    ("font-list", slot),
                    self.font_matches.len() + lead,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let current = this.store.read(cx).prefs().fonts.get(key).cloned();
                        range
                            .map(|ix| {
                                let row = div().id(("font", ix)).w_full().h(u(34.)).px(u(12.)).rounded(u(6.)).flex().items_center().gap_3();
                                let pick = |row: gpui::Stateful<gpui::Div>, on: bool| {
                                    row.cursor_pointer()
                                        .when(on, |d| d.bg(rgb(HOVER)).font_weight(FontWeight::SEMIBOLD))
                                        .when(!on, |d| d.text_color(rgb(MUTED)).hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT))))
                                };
                                if ix < lead {
                                    return pick(row, current.is_none())
                                        .child(format!("{default} (default)"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.choose_font(slot, None, cx)))
                                        .into_any_element();
                                }
                                let name = this.font_matches[ix - lead];
                                let row = row.child(div().flex_1().min_w_0().truncate().child(name));
                                if fonts::installed(&this.font_files, name) {
                                    pick(row, current.as_deref() == Some(name))
                                        .on_click(cx.listener(move |this, _, _, cx| this.choose_font(slot, Some(name), cx)))
                                        .into_any_element()
                                } else if this.downloading.contains(&name) {
                                    row.text_color(rgb(FAINT)).child(div().text_size(u(12.)).child("Downloading…")).into_any_element()
                                } else {
                                    row.text_color(rgb(FAINT))
                                        .child(
                                            div()
                                                .id(("font-download", ix))
                                                .flex_none()
                                                .size(u(26.))
                                                .rounded(u(13.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|s| s.bg(rgb(HOVER)))
                                                .child(gpui::svg().path("icons/download.svg").size(u(16.)).text_color(rgb(MUTED)))
                                                .on_click(cx.listener(move |this, _, _, cx| this.download_font(name, cx))),
                                        )
                                        .into_any_element()
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full();
                div()
                    .flex()
                    .flex_col()
                    .gap(u(8.))
                    .child(self.font_search.clone())
                    .child(div().w_full().h(u(280.)).p(u(6.)).rounded(u(10.)).border_1().border_color(rgb(LINE)).child(rows))
            });
            let note = open.then(|| self.font_failed.clone()).flatten().map(|(name, why)| {
                let page = format!("https://fonts.google.com/specimen/{}", name.replace(' ', "+"));
                div()
                    .flex()
                    .flex_col()
                    .gap(u(8.))
                    .child(div().text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(format!(
                        "Couldn’t download {name} ({why}). Download it yourself from Google Fonts and put its .ttf files in {}, then open this list again.",
                        fonts::folder()
                    )))
                    .child(
                        div()
                            .flex()
                            .gap(u(6.))
                            .child(small_button(("font-page", slot), "Open Google Fonts", true).on_click(cx.listener(move |_, _, _, cx| cx.open_url(&page))))
                            .child(small_button(("font-folder", slot), "Open fonts folder", false).on_click(cx.listener(|_, _, _, cx| {
                                let _ = std::fs::create_dir_all(fonts::dir());
                                cx.reveal_path(&fonts::dir());
                            }))),
                    )
            });
            div()
                .w_full()
                .py(u(14.))
                .border_b_1()
                .border_color(rgb(RULE))
                .flex()
                .flex_col()
                .gap(u(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(div().flex_1().min_w_0().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child(label))
                        .child(small_button(("font-slot", slot), "", open).child(current.clone().unwrap_or_else(|| default.to_string())).on_click(cx.listener(move |this, _, _, cx| {
                            this.font_open = if this.font_open == Some(slot) { None } else { Some(slot) };
                            if this.font_open.is_some() {
                                this.font_search.update(cx, |i, cx| {
                                    i.text.clear();
                                    cx.notify();
                                });
                                this.scan_fonts(cx);
                            }
                            cx.notify();
                        }))),
                )
                .children(note)
                .children(list)
        });
        let fonts_note = div().pt(u(10.)).text_size(u(13.)).text_color(rgb(MUTED)).child("Search all of Google Fonts, then click a font’s download icon before using it. It saves to the Nano fonts folder.");

        let chosen = self.store.read(cx).prefs().host_heap_mb;
        let auto = crate::host::auto_heap_mb();
        let effective = chosen.unwrap_or(auto);
        let restart = crate::host::LAUNCH_HEAP_MB.get().is_some_and(|m| *m != effective);
        let store = self.store.clone();
        let heap_choice = move |id: (&'static str, usize), label: String, value: Option<u32>| {
            let store = store.clone();
            small_button(id, "", chosen == value).child(label).on_click(move |_, _, cx| store.update(cx, |s, cx| s.edit_prefs(cx, |p| p.host_heap_mb = value)))
        };
        let heap_chips: Vec<_> = std::iter::once(heap_choice(("heap", 0), format!("Auto · {auto} MB"), None))
            .chain(crate::host::HEAP_CHOICES_MB.map(|mb| heap_choice(("heap", mb as usize), format!("{mb} MB"), Some(mb))))
            .collect();
        let heap = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Extension memory"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                        "How much memory extensions can use.",
                    ))
                    .when(restart, |d| d.child(div().text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child("Restart Nano to apply"))),
            )
            .child(div().flex_none().flex().gap(u(6.)).children(heap_chips));

        let (sync_label, sync_failed) = match &self.store.read(cx).sync_status {
            SyncStatus::Idle => (if self.store.read(cx).sync_config().key.is_empty() { "Not set up".to_string() } else { "Not synced yet".into() }, false),
            SyncStatus::Syncing => ("Syncing…".into(), false),
            SyncStatus::Done(at) => (format!("Synced {}", crate::store::ago(*at, crate::store::now_ms())), false),
            SyncStatus::Failed(e) => (e.clone(), true),
        };
        let sync = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .flex_col()
            .gap(u(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(4.))
                            .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("SyncYomi"))
                            .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                                "Syncs library and progress. Use the same server and API key as on your phone.",
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(u(320.))
                            .truncate()
                            .font_family(crate::fonts::mono())
                            .text_size(u(12.))
                            .text_color(rgb(if sync_failed { ACCENT_TEXT } else { FAINT }))
                            .child(sync_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(u(10.))
                    .child(div().flex_1().min_w_0().child(self.sync_host.clone()))
                    .child(div().w(u(300.)).child(self.sync_key.clone()))
                    .child(small_button("sync-now", "Sync now", true).on_click(cx.listener(|this, _, _, cx| this.sync_now(cx)))),
            );

        let backup = div()
            .w_full()
            .py(u(14.))
            .border_t_1()
            .border_b_1()
            .border_color(rgb(RULE))
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(u(4.))
                    .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Backup"))
                    .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                        "Library, categories, progress and tracking as a Mihon .tachibk file.",
                    ))
                    .when(!self.backup_note.is_empty(), |d| d.child(div().text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(self.backup_note.clone()))),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(u(6.))
                    .child(small_button("backup-export", "Export", false).on_click(cx.listener(|this, _, _, cx| this.export_backup(cx))))
                    .child(small_button("backup-import", "Import", false).on_click(cx.listener(|this, _, _, cx| this.import_backup(cx)))),
            );

        let trackers = crate::track::TRACKERS.into_iter().filter(|t| crate::track::configured(*t)).map(|tracker| {
            let user = self.store.read(cx).login(tracker).map(|l| l.user.clone());
            let waiting = self.signing_in == Some(tracker);
            let note = match &user {
                Some(name) => format!("Signed in as {name}"),
                None if waiting => "Waiting for the browser…".into(),
                None => "Not signed in".into(),
            };
            let button = match user {
                Some(_) => small_button(("tracker-out", tracker as usize), "Sign out", false).on_click(cx.listener(move |this, _, _, cx| {
                    this.store.update(cx, |s, cx| s.set_login(tracker, None, cx))
                })),
                None => small_button(("tracker-in", tracker as usize), "Sign in", true).on_click(cx.listener(move |this, _, _, cx| this.sign_in(tracker, cx))),
            };
            div()
                .py(u(12.))
                .flex()
                .items_center()
                .gap_4()
                .border_b_1()
                .border_color(rgb(RULE))
                .child(div().flex_1().min_w_0().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child(crate::track::name(tracker)))
                .child(div().font_family(crate::fonts::mono()).text_size(u(12.)).text_color(rgb(FAINT)).child(note))
                .child(button)
        });
        let tracking = div()
            .w_full()
            .border_t_1()
            .border_color(rgb(RULE))
            .children(trackers)
            .when(!self.track_note.is_empty(), |d| d.child(div().pt(u(10.)).text_size(u(13.)).text_color(rgb(ACCENT_TEXT)).child(self.track_note.clone())))
            .child(div().pt(u(10.)).text_size(u(13.)).text_color(rgb(MUTED)).child(
                "Link a series from its page under Tracking. Chapters you read in Nano then count on your list there.",
            ));

        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(u(PAD))
            .pt(u(28.))
            .pb(u(40.))
            .child(heading("Settings", ""))
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("READING"))
            .child(reading)
            .child(fullscreen)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("FONTS"))
            .child(div().w_full().border_t_1().border_color(rgb(RULE)).children(font_rows))
            .child(fonts_note)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("CATEGORIES"))
            .child(categories)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("DOWNLOADS"))
            .child(downloads)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("TRACKING"))
            .child(tracking)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("SYNC"))
            .child(sync)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("BACKUP"))
            .child(backup)
            .when(crate::update::current().is_some(), |d| {
                d.child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("UPDATES")).child(updates)
            })
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("CLOUDFLARE"))
            .child(
                div()
                    .w_full()
                    .py(u(14.))
                    .border_t_1()
                    .border_b_1()
                    .border_color(rgb(RULE))
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(u(4.))
                            .child(div().text_size(u(15.)).font_weight(FontWeight::SEMIBOLD).child("Solver"))
                            .child(div().text_size(u(13.)).text_color(rgb(MUTED)).child(
                                "Optional FlareSolverr or Byparr address, tried first.",
                            )),
                    )
                    .child(div().flex_none().w(u(340.)).child(self.solver.clone())),
            )
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("STORAGE"))
            .child(storage)
            .child(pages)
            .child(div().pt(u(24.)).pb(u(10.)).font_family(crate::fonts::mono()).text_size(u(11.)).text_color(rgb(FAINT)).child("MEMORY"))
            .child(heap)
    }
}
