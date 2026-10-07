use crate::state::AppState;
use gpui::{
    div, App, AsyncWindowContext, Entity, IntoElement, ParentElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    WindowExt,
};

pub fn close_tab_with_prompt(
    app_state: Entity<AppState>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    app_state.update(cx, |state, cx| {
        if let Some(doc) = state.documents.get(index) {
            doc.update(cx, |doc, cx| {
                doc.check_dirty(cx);
            });
        }
    });

    let is_dirty = app_state
        .read(cx)
        .documents
        .get(index)
        .map(|d| d.read(cx).dirty)
        .unwrap_or(false);

    if is_dirty {
        app_state.update(cx, |state, cx| {
            state.switch_to_tab(index, cx);
        });
        show_save_dialog_for_tab(app_state, index, window, cx);
    } else {
        app_state.update(cx, |state, cx| {
            state.force_close_tab(index, window, cx);
        });
        let is_closing = app_state.read(cx).is_closing_window;
        if is_closing {
            prompt_next_dirty_tab(app_state, window, cx);
        }
    }
}

pub fn attempt_close_window(
    app_state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    app_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
    });

    let has_dirty = app_state.read(cx).has_dirty_documents(cx);

    if has_dirty {
        app_state.update(cx, |state, _| {
            state.is_closing_window = true;
        });
        prompt_next_dirty_tab(app_state, window, cx);
        false
    } else {
        app_state.update(cx, |state, _| {
            state.shared_vocabulary.borrow_mut().save();
        });
        true
    }
}

pub fn prompt_next_dirty_tab(
    app_state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) {
    let dirty_index = app_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
        state.documents.iter().position(|d| d.read(cx).dirty)
    });

    if let Some(index) = dirty_index {
        app_state.update(cx, |state, cx| {
            state.switch_to_tab(index, cx);
        });
        show_save_dialog_for_tab(app_state, index, window, cx);
    } else {
        let is_closing = app_state.read(cx).is_closing_window;
        if is_closing {
            window.remove_window();
        }
    }
}

fn show_save_dialog_for_tab(
    app_state: Entity<AppState>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let title = app_state
        .read(cx)
        .documents
        .get(index)
        .map(|d| d.read(cx).title.clone())
        .unwrap_or_else(|| "Untitled".into());

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let app_state_cancel = app_state.clone();

        dialog
            .title("Save changes?")
            .overlay_closable(false)
            .close_button(true)
            .keyboard(true)
            .on_cancel(move |_, _, cx| {
                app_state_cancel.update(cx, |state, _| {
                    state.is_closing_window = false;
                });
                true
            })
            .child(
                div()
                    .text_sm()
                    .child(format!("Do you want to save changes to {}?", title)),
            )
            .footer({
                let app_state_footer_cancel = app_state.clone();
                let app_state_footer_discard = app_state.clone();
                let app_state_footer_save = app_state.clone();
                
                move |_ok, _cancel, _window, _cx| {
                    let app_state_cancel_click = app_state_footer_cancel.clone();
                    let app_state_discard_click = app_state_footer_discard.clone();
                    let app_state_save_click = app_state_footer_save.clone();
                    
                    vec![
                        Button::new("cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(move |_, window, cx| {
                                app_state_cancel_click.update(cx, |state, _| {
                                    state.is_closing_window = false;
                                });
                                window.close_dialog(cx);
                            })
                            .into_any_element(),
                        Button::new("dont-save")
                            .outline()
                            .label("Don't Save")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                app_state_discard_click.update(cx, |state, cx| {
                                    state.force_close_tab(index, window, cx);
                                });
                                let is_closing = app_state_discard_click.read(cx).is_closing_window;
                                if is_closing {
                                    prompt_next_dirty_tab(app_state_discard_click.clone(), window, cx);
                                }
                            })
                            .into_any_element(),
                        Button::new("save")
                            .primary()
                            .label("Save")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                perform_save_and_close_tab(
                                    window,
                                    app_state_save_click.clone(),
                                    index,
                                    cx,
                                );
                            })
                            .into_any_element(),
                    ]
                }
            })
    });
}

fn perform_save_and_close_tab(
    window: &Window,
    app_state: Entity<AppState>,
    index: usize,
    cx: &App,
) {
    window
        .spawn(cx, move |cx: &mut AsyncWindowContext| {
            let mut cx = cx.clone();
            let app_state = app_state.clone();
            async move {
                let doc_info = cx
                    .update(|_, cx| {
                        app_state.update(cx, |state, cx| {
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
                        app_state.update(cx, |state, _cx| {
                            state.is_closing_window = false;
                        });
                    }).ok();
                    return;
                };

                let saved_path = if let Some(existing_path) = path {
                    cx.update(|_, cx| {
                        app_state.update(cx, |state, cx| {
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
                                app_state.update(cx, |state, cx| {
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
                        app_state.update(cx, |state, cx| {
                            state.force_close_tab(index, window, cx);
                        });
                        let is_closing = app_state.read(cx).is_closing_window;
                        if is_closing {
                            prompt_next_dirty_tab(app_state.clone(), window, cx);
                        }
                    })
                    .ok();
                } else {
                    cx.update(|_window, cx| {
                        app_state.update(cx, |state, _cx| {
                            state.is_closing_window = false;
                        });
                    }).ok();
                }
            }
        })
        .detach();
}