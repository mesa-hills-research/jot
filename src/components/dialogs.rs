use crate::chrome;
use crate::state::WindowState;
use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
};
use gpui_kit::{App, Entity, ParentElement, Styled, Window, div};

pub fn close_tab_with_prompt(
    window_state: Entity<WindowState>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    window_state.update(cx, |state, cx| {
        if let Some(doc) = state.documents.get(index) {
            doc.update(cx, |doc, cx| {
                doc.check_dirty(cx);
            });
        }
    });

    let is_dirty = window_state
        .read(cx)
        .documents
        .get(index)
        .map(|d| d.read(cx).dirty)
        .unwrap_or(false);

    if is_dirty {
        window_state.update(cx, |state, cx| {
            state.switch_to_tab(index, cx);
        });
        show_save_dialog_for_tab(window_state, index, window, cx);
    } else {
        window_state.update(cx, |state, cx| {
            state.force_close_tab(index, window, cx);
        });
        let is_closing = window_state.read(cx).is_closing_window;
        if is_closing {
            prompt_next_dirty_tab(window_state, window, cx);
        }
    }
}

pub fn attempt_close_window(
    window_state: Entity<WindowState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    window_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
    });

    let has_dirty = window_state.read(cx).has_dirty_documents(cx);

    if has_dirty {
        window_state.update(cx, |state, _| {
            state.is_closing_window = true;
        });
        prompt_next_dirty_tab(window_state, window, cx);
        false
    } else {
        window_state
            .read(cx)
            .app_state
            .read(cx)
            .save_vocabulary_on_close(cx);
        true
    }
}

pub fn prompt_next_dirty_tab(window_state: Entity<WindowState>, window: &mut Window, cx: &mut App) {
    let dirty_index = window_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
        state.documents.iter().position(|d| d.read(cx).dirty)
    });

    if let Some(index) = dirty_index {
        window_state.update(cx, |state, cx| {
            state.switch_to_tab(index, cx);
        });
        show_save_dialog_for_tab(window_state, index, window, cx);
    } else {
        let state = window_state.read(cx);
        let (is_closing, quitting) = (state.is_closing_window, state.quit_after_close);
        if is_closing {
            window.remove_window();
            // Quitting goes on to the other windows.
            if quitting {
                chrome::quit(cx);
            }
        }
    }
}

fn show_save_dialog_for_tab(
    window_state: Entity<WindowState>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let title = window_state
        .read(cx)
        .documents
        .get(index)
        .map(|d| d.read(cx).title.clone())
        .unwrap_or_else(|| "Untitled".into());

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let window_state_cancel = window_state.clone();

        dialog
            .title("Save changes?")
            .overlay_closable(false)
            .close_button(true)
            .keyboard(true)
            .on_cancel(move |_, _, cx| {
                window_state_cancel.update(cx, |state, _| {
                    state.cancel_closing();
                });
                true
            })
            .child(
                div()
                    .text_sm()
                    .child(format!("Do you want to save changes to {}?", title)),
            )
            .footer({
                let window_state_cancel_click = window_state.clone();
                let window_state_discard_click = window_state.clone();
                let window_state_save_click = window_state.clone();

                DialogFooter::new()
                    .child(Button::new("cancel").outline().label("Cancel").on_click(
                        move |_, window, cx| {
                            window_state_cancel_click.update(cx, |state, _| {
                                state.cancel_closing();
                            });
                            window.close_dialog(cx);
                        },
                    ))
                    .child(
                        Button::new("dont-save")
                            .outline()
                            .label("Don't Save")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                window_state_discard_click.update(cx, |state, cx| {
                                    state.force_close_tab(index, window, cx);
                                });
                                let is_closing =
                                    window_state_discard_click.read(cx).is_closing_window;
                                if is_closing {
                                    prompt_next_dirty_tab(
                                        window_state_discard_click.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            }),
                    )
                    .child(Button::new("save").primary().label("Save").on_click(
                        move |_, window, cx| {
                            window.close_dialog(cx);
                            perform_save_and_close_tab(
                                window,
                                window_state_save_click.clone(),
                                index,
                                cx,
                            );
                        },
                    ))
            })
    });
}

fn perform_save_and_close_tab(
    window: &Window,
    window_state: Entity<WindowState>,
    index: usize,
    cx: &App,
) {
    window
        .spawn(cx, async move |cx| {
            let doc_info = cx
                .update(|_, cx| {
                    window_state.update(cx, |state, cx| {
                        state.documents.get(index).cloned().map(|doc| {
                            let content = doc.read(cx).content(cx);
                            let path = doc.read(cx).path.clone();
                            (doc, path, content)
                        })
                    })
                })
                .ok()
                .flatten();

            let Some((doc, path, content)) = doc_info else {
                cx.update(|_window, cx| {
                    window_state.update(cx, |state, _cx| {
                        state.cancel_closing();
                    });
                })
                .ok();
                return;
            };

            let saved_path = if let Some(existing_path) = path {
                cx.update(|_, cx| {
                    window_state.update(cx, |state, cx| {
                        if let Err(e) = state.perform_save(
                            doc.clone(),
                            existing_path.clone(),
                            content.clone(),
                            cx,
                        ) {
                            log::error!("Failed to save: {}", e);
                            None
                        } else {
                            Some(existing_path)
                        }
                    })
                })
                .ok()
                .flatten()
            } else {
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
                        None
                    } else {
                        cx.update(|_, cx| {
                            window_state.update(cx, |state, cx| {
                                state.document_saved(doc.clone(), path.clone(), cx);
                            });
                        })
                        .ok();
                        Some(path)
                    }
                } else {
                    None
                }
            };

            if saved_path.is_some() {
                cx.update(|window, cx| {
                    window_state.update(cx, |state, cx| {
                        state.force_close_tab(index, window, cx);
                    });
                    let is_closing = window_state.read(cx).is_closing_window;
                    if is_closing {
                        prompt_next_dirty_tab(window_state.clone(), window, cx);
                    }
                })
                .ok();
            } else {
                cx.update(|_window, cx| {
                    window_state.update(cx, |state, _cx| {
                        state.cancel_closing();
                    });
                })
                .ok();
            }
        })
        .detach();
}
