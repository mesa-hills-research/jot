use crate::actions::*;
use crate::chrome;
use crate::components::{
    Editor, JotTabBar, JotTitleBar, SettingsPanel, StatusBar, View, close_tab_with_prompt,
};
use crate::fonts;
use crate::launch::report_open_errors;
use crate::state::{AppState, WindowState};
use gpui_kit::component::{
    ActiveTheme, WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
    input::{self, Input, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;

/// A jot window: its tabs and documents, and the settings page.
pub struct JotApp {
    app_state: Entity<AppState>,
    window_state: Entity<WindowState>,
    title_bar: Entity<JotTitleBar>,
    tab_bar: Entity<JotTabBar>,
    editor: Entity<Editor>,
    status_bar: Entity<StatusBar>,
    settings_panel: Entity<SettingsPanel>,
    goto_line_input: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl JotApp {
    /// A window with the files at `paths` in its tabs, or a new document
    /// without any.
    pub fn new(
        app_state: Entity<AppState>,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let window_state = cx.new(|cx| WindowState::new(app_state.clone(), window, cx));
        // A font named in the settings may have gone, such as a font file
        // that was deleted. The editor then uses the default, and the first
        // window says so.
        let missing_font = app_state.update(cx, |state, _| state.take_missing_font());
        if let Some(family) = missing_font {
            cx.defer_in(window, move |_, window, cx| {
                window.push_notification(
                    Notification::warning(format!(
                        "The font \u{201c}{family}\u{201d} isn\u{2019}t available, so the editor uses {}.",
                        fonts::DEFAULT_EDITOR_FONT
                    )),
                    cx,
                );
            });
        }

        window.on_window_should_close(cx, chrome::can_close);

        // The View menu shows Word Wrap and Line Numbers.
        let settings_subscription = cx.observe_in(&app_state, window, |this, app_state, _, cx| {
            let settings = app_state.read(cx).settings.clone();
            set_menus(&settings, cx);
            this.title_bar
                .update(cx, |title_bar, cx| title_bar.reload_menus(cx));
        });
        // Files from a later launch open in the window used last.
        let handle = window.window_handle();
        app_state.update(cx, |state, _| state.window_activated(handle));
        let activation_subscription = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                let handle = window.window_handle();
                this.app_state
                    .update(cx, |state, _| state.window_activated(handle));
            }
        });

        let errors = window_state.update(cx, |state, cx| {
            let errors = state.open_paths(paths, window, cx);
            if state.documents.is_empty() {
                state.new_untitled_document(window, cx);
            }
            errors
        });
        cx.defer_in(window, move |this, window, cx| {
            report_open_errors(errors, window, cx);
            this.focus_editor(window, cx);
        });

        let title_bar = cx.new(|cx| JotTitleBar::new(window_state.clone(), cx));
        let tab_bar = cx.new(|_| JotTabBar::new(window_state.clone()));
        let editor = cx.new(|cx| Editor::new(window_state.clone(), window, cx));
        let status_bar = cx.new(|cx| StatusBar::new(window_state.clone(), cx));
        let settings_panel =
            cx.new(|cx| SettingsPanel::new(app_state.clone(), window_state.clone(), window, cx));
        let goto_line_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Line number..."));
        let focus_handle = cx.focus_handle();

        Self {
            app_state,
            window_state,
            title_bar,
            tab_bar,
            editor,
            status_bar,
            settings_panel,
            goto_line_input,
            focus_handle,
            _subscriptions: vec![settings_subscription, activation_subscription],
        }
    }

    /// Opens the files at `paths` in tabs, and says why any couldn't be
    /// opened.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let errors = self
            .window_state
            .update(cx, |state, cx| state.open_paths(paths, window, cx));
        report_open_errors(errors, window, cx);
        self.focus_editor(window, cx);
    }

    /// Shows the document in tab `index`.
    pub fn show_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.window_state.update(cx, |state, cx| {
            state.switch_to_tab(index, cx);
            if state.current_view != View::Editor {
                state.show_editor(cx);
            }
        });
        self.focus_editor(window, cx);
    }

    /// Moves the keyboard focus to the active document, unless the
    /// settings page is showing.
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_state.update(cx, |state, cx| {
            if state.current_view == View::Editor {
                state.focus_active_editor(window, cx);
            }
        });
    }

    fn show_goto_line_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_state = self.window_state.clone();
        let goto_line_input = self.goto_line_input.clone();

        goto_line_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });

        window.open_dialog(cx, move |dialog, _, _| {
            let go = {
                let window_state = window_state.clone();
                let input = goto_line_input.clone();
                move |window: &mut Window, cx: &mut App| {
                    let line_str = input.read(cx).value().to_string();
                    if let Ok(line) = line_str.trim().parse::<usize>() {
                        window_state.update(cx, |state, cx| {
                            state.goto_line(line, window, cx);
                        });
                    }
                }
            };
            let go_on_click = go.clone();

            dialog
                .title("Go to Line")
                .child(div().w(px(240.)).child(Input::new(&goto_line_input)))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").primary().label("OK").on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                go_on_click(window, cx);
                            },
                        )),
                )
                // Enter in the line number field.
                .on_ok(move |_, window, cx| {
                    go(window, cx);
                    true
                })
        });
        self.goto_line_input
            .update(cx, |state, cx| state.focus(window, cx));
    }

    fn trigger_open_file(window: &Window, window_state: Entity<WindowState>, cx: &App) {
        window
            .spawn(cx, async move |cx| {
                let file = rfd::AsyncFileDialog::new()
                    .add_filter(
                        "Text Files",
                        &["txt", "md", "json", "xml", "html", "css", "js"],
                    )
                    .add_filter("All Files", &["*"])
                    .pick_file()
                    .await;

                if let Some(file) = file {
                    let path = file.path().to_path_buf();

                    cx.update(|window, cx| {
                        let opened =
                            window_state.update(cx, |state, cx| state.open_file(path, window, cx));
                        if let Err(error) = opened {
                            report_open_errors(vec![error], window, cx);
                        }
                    })
                    .ok();

                    let window_state_clone = window_state.clone();
                    cx.on_next_frame(move |_window, _cx| {
                        let window_state_inner = window_state_clone.clone();
                        _window.on_next_frame(move |_window, cx| {
                            window_state_inner.update(cx, |state, cx| {
                                if let Some(doc) = state.active_document() {
                                    let editor_state = doc.read(cx).editor_state.clone();
                                    editor_state.update(cx, |_, cx| {
                                        cx.notify();
                                    });
                                }
                            });
                        });
                    });
                }
            })
            .detach();
    }

    fn trigger_new_from_template(window: &Window, window_state: Entity<WindowState>, cx: &App) {
        window
            .spawn(cx, async move |cx| {
                let file = rfd::AsyncFileDialog::new()
                    .set_title("Select Template")
                    .add_filter(
                        "Text Files",
                        &["txt", "md", "json", "xml", "html", "css", "js"],
                    )
                    .add_filter("All Files", &["*"])
                    .pick_file()
                    .await;

                if let Some(file) = file {
                    let path = file.path().to_path_buf();

                    cx.update(|window, cx| {
                        let opened = window_state
                            .update(cx, |state, cx| state.open_as_template(path, window, cx));
                        if let Err(error) = opened {
                            report_open_errors(vec![error], window, cx);
                        }
                    })
                    .ok();

                    let window_state_clone = window_state.clone();
                    cx.on_next_frame(move |_window, _cx| {
                        let window_state_inner = window_state_clone.clone();
                        _window.on_next_frame(move |_window, cx| {
                            window_state_inner.update(cx, |state, cx| {
                                if let Some(doc) = state.active_document() {
                                    let editor_state = doc.read(cx).editor_state.clone();
                                    editor_state.update(cx, |_, cx| {
                                        cx.notify();
                                    });
                                }
                            });
                        });
                    });
                }
            })
            .detach();
    }

    fn trigger_save_as(window: &Window, window_state: Entity<WindowState>, cx: &App) {
        window
            .spawn(cx, async move |cx| {
                let (doc, content) = cx
                    .update(|_, cx| {
                        window_state.update(cx, |state, cx| {
                            if let Some(doc) = state.active_document().cloned() {
                                let content = doc.read(cx).content(cx);
                                (Some(doc), content)
                            } else {
                                (None, String::new())
                            }
                        })
                    })
                    .unwrap_or((None, String::new()));

                let Some(doc) = doc else { return };

                let file = rfd::AsyncFileDialog::new()
                    .add_filter("Text Files", &["txt"])
                    .add_filter("All Files", &["*"])
                    .set_file_name("untitled.txt")
                    .save_file()
                    .await;

                if let Some(file) = file {
                    let path = file.path().to_path_buf();
                    if let Err(e) = std::fs::write(&path, &content) {
                        log::error!("Failed to save file: {}", e);
                        return;
                    }

                    cx.update(|_, cx| {
                        window_state.update(cx, |state, cx| {
                            state.document_saved(doc, path, cx);
                        });
                    })
                    .ok();
                }
            })
            .detach();
    }

    pub fn window_state(&self) -> &Entity<WindowState> {
        &self.window_state
    }

    /// Opens the active document's find panel, or find and replace.
    fn open_search(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.window_state.read(cx).active_document() else {
            return;
        };
        let editor_state = doc.read(cx).editor_state.clone();
        editor_state.update(cx, |state, cx| {
            state.focus(window, cx);
            state.open_search(replace, cx);
        });
    }

    fn bind_global_actions(&self, div: Div, cx: &mut Context<Self>) -> Div {
        div.key_context(APP_CONTEXT)
            .on_action(cx.listener(|this, action: &NewTab, window, cx| {
                this.window_state.update(cx, |state, cx| {
                    state.on_new_tab(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &NewFromTemplate, window, cx| {
                Self::trigger_new_from_template(window, this.window_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                Self::trigger_open_file(window, this.window_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &Save, window, cx| {
                let window_state = this.window_state.clone();

                let needs_save_as =
                    window_state.update(cx, |state, cx| match state.save_active_document(cx) {
                        Ok(saved) => !saved,
                        Err(e) => {
                            log::error!("Save failed: {}", e);
                            false
                        }
                    });

                if needs_save_as {
                    Self::trigger_save_as(window, window_state, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                Self::trigger_save_as(window, this.window_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let index = this.window_state.read(cx).active_index;
                close_tab_with_prompt(this.window_state.clone(), index, window, cx);
            }))
            // Not a listener: the close check reads this view.
            .on_action(|_: &CloseWindow, window, cx| chrome::close_window(window, cx))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // The settings are every window's: a change here reaches them all.
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.app_state.update(cx, |state, cx| state.zoom_in(cx));
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.app_state.update(cx, |state, cx| state.zoom_out(cx));
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.app_state.update(cx, |state, cx| state.reset_zoom(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleWordWrap, _, cx| {
                this.app_state
                    .update(cx, |state, cx| state.toggle_word_wrap(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleLineNumbers, _, cx| {
                this.app_state
                    .update(cx, |state, cx| state.toggle_line_numbers(cx));
            }))
            .on_action(cx.listener(|this, action: &OpenSettings, window, cx| {
                this.window_state.update(cx, |state, cx| {
                    state.on_open_settings(action, window, cx);
                });
            }))
            // The editor opens its own find and replace panel on Ctrl+F and
            // Ctrl+H. These run when the action comes from elsewhere, such as
            // the Edit menu.
            .on_action(cx.listener(|this, _: &input::Search, window, cx| {
                this.open_search(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &input::Replace, window, cx| {
                this.open_search(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| {
                this.show_goto_line_dialog(window, cx);
            }))
            .on_action(cx.listener(|this, action: &NextTab, window, cx| {
                this.window_state.update(cx, |state, cx| {
                    state.on_next_tab(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &PreviousTab, window, cx| {
                this.window_state.update(cx, |state, cx| {
                    state.on_previous_tab(action, window, cx);
                });
            }))
    }
}

impl Focusable for JotApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for JotApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_view = self.window_state.read(cx).current_view;

        v_flex()
            .id("jot-app")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(fonts::UI_FONT)
            .child(
                self.bind_global_actions(div(), cx)
                    .child(self.title_bar.clone()),
            )
            .child(
                // The space under the title bar, which a long settings page
                // scrolls within.
                self.bind_global_actions(v_flex(), cx)
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.focus_handle)
                    .when(current_view == View::Editor, |this| {
                        this.child(self.tab_bar.clone())
                            .child(self.editor.clone())
                            .child(self.status_bar.clone())
                    })
                    .when(current_view == View::Settings, |this| {
                        this.child(self.settings_panel.clone())
                    }),
            )
    }
}
