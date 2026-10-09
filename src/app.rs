use crate::actions::*;
use crate::chrome;
use crate::components::{
    Editor, JotTabBar, JotTitleBar, SettingsPanel, StatusBar, View, close_tab_with_prompt,
};
use crate::state::{AppEvent, AppState};
use gpui_kit::component::{
    ActiveTheme, WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
    input::{self, Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

pub struct JotApp {
    app_state: Entity<AppState>,
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
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app_state = cx.new(AppState::new);

        window.on_window_should_close(cx, chrome::can_close);

        let event_subscription =
            cx.subscribe_in(&app_state, window, |this, _, event, window, cx| {
                this.handle_app_event(event, window, cx);
            });

        app_state.update(cx, |state, cx| {
            state.new_untitled_document(window, cx);
        });

        let title_bar = cx.new(|cx| JotTitleBar::new(app_state.clone(), cx));
        let tab_bar = cx.new(|_| JotTabBar::new(app_state.clone()));
        let editor = cx.new(|cx| Editor::new(app_state.clone(), window, cx));
        let status_bar = cx.new(|_| StatusBar::new(app_state.clone()));
        let settings_panel = cx.new(|cx| SettingsPanel::new(app_state.clone(), window, cx));
        let goto_line_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Line number..."));
        let focus_handle = cx.focus_handle();

        Self {
            app_state,
            title_bar,
            tab_bar,
            editor,
            status_bar,
            settings_panel,
            goto_line_input,
            focus_handle,
            _subscriptions: vec![event_subscription],
        }
    }

    fn handle_app_event(&mut self, event: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let AppEvent::SettingsChanged = event {
            self.app_state.update(cx, |state, cx| {
                state.apply_settings_to_all_docs(window, cx);
            });
            // The View menu shows Word Wrap and Line Numbers.
            let settings = self.app_state.read(cx).settings.clone();
            set_menus(&settings, cx);
            self.title_bar
                .update(cx, |title_bar, cx| title_bar.reload_menus(cx));
        }
    }

    fn show_goto_line_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_state = self.app_state.clone();
        let goto_line_input = self.goto_line_input.clone();

        goto_line_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });

        window.open_dialog(cx, move |dialog, _, _| {
            let go = {
                let app_state = app_state.clone();
                let input = goto_line_input.clone();
                move |window: &mut Window, cx: &mut App| {
                    let line_str = input.read(cx).value().to_string();
                    if let Ok(line) = line_str.trim().parse::<usize>() {
                        app_state.update(cx, |state, cx| {
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

    fn trigger_open_file(window: &Window, app_state: Entity<AppState>, cx: &App) {
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
                        app_state.update(cx, |state, cx| {
                            state.open_file(path, window, cx);
                        });
                    })
                    .ok();

                    let app_state_clone = app_state.clone();
                    cx.on_next_frame(move |_window, _cx| {
                        let app_state_inner = app_state_clone.clone();
                        _window.on_next_frame(move |_window, cx| {
                            app_state_inner.update(cx, |state, cx| {
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

    fn trigger_new_from_template(window: &Window, app_state: Entity<AppState>, cx: &App) {
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
                        app_state.update(cx, |state, cx| {
                            state.open_as_template(path, window, cx);
                        });
                    })
                    .ok();

                    let app_state_clone = app_state.clone();
                    cx.on_next_frame(move |_window, _cx| {
                        let app_state_inner = app_state_clone.clone();
                        _window.on_next_frame(move |_window, cx| {
                            app_state_inner.update(cx, |state, cx| {
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

    fn trigger_save_as(window: &Window, app_state: Entity<AppState>, cx: &App) {
        window
            .spawn(cx, async move |cx| {
                let (doc, content) = cx
                    .update(|_, cx| {
                        app_state.update(cx, |state, cx| {
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
                        app_state.update(cx, |state, cx| {
                            state.document_saved(doc, path, cx);
                        });
                    })
                    .ok();
                }
            })
            .detach();
    }

    pub fn app_state(&self) -> &Entity<AppState> {
        &self.app_state
    }

    /// Opens the active document's find panel, or find and replace.
    fn open_search(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.app_state.read(cx).active_document() else {
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
                this.app_state.update(cx, |state, cx| {
                    state.on_new_tab(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &NewFromTemplate, window, cx| {
                Self::trigger_new_from_template(window, this.app_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                Self::trigger_open_file(window, this.app_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &Save, window, cx| {
                let app_state = this.app_state.clone();

                let needs_save_as =
                    app_state.update(cx, |state, cx| match state.save_active_document(cx) {
                        Ok(saved) => !saved,
                        Err(e) => {
                            log::error!("Save failed: {}", e);
                            false
                        }
                    });

                if needs_save_as {
                    Self::trigger_save_as(window, app_state, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                Self::trigger_save_as(window, this.app_state.clone(), cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let index = this.app_state.read(cx).active_index;
                close_tab_with_prompt(this.app_state.clone(), index, window, cx);
            }))
            // Not a listener: the close check reads this view.
            .on_action(|_: &CloseWindow, window, cx| chrome::close_window(window, cx))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .on_action(cx.listener(|this, action: &ZoomIn, window, cx| {
                this.app_state.update(cx, |state, cx| {
                    state.on_zoom_in(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &ZoomOut, window, cx| {
                this.app_state.update(cx, |state, cx| {
                    state.on_zoom_out(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &ResetZoom, window, cx| {
                this.app_state.update(cx, |state, cx| {
                    state.on_reset_zoom(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &ToggleWordWrap, window, cx| {
                this.app_state.update(cx, |state, cx| {
                    state.on_toggle_word_wrap(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &ToggleLineNumbers, window, cx| {
                this.app_state.update(cx, |state, cx| {
                    state.on_toggle_line_numbers(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &OpenSettings, window, cx| {
                this.app_state.update(cx, |state, cx| {
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
                this.app_state.update(cx, |state, cx| {
                    state.on_next_tab(action, window, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &PreviousTab, window, cx| {
                this.app_state.update(cx, |state, cx| {
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
        let current_view = self.app_state.read(cx).current_view;

        v_flex()
            .id("jot-app")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family("Work Sans")
            .child(
                self.bind_global_actions(div(), cx)
                    .child(self.title_bar.clone()),
            )
            .child(
                self.bind_global_actions(v_flex(), cx)
                    .size_full()
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
