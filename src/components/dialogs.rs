use crate::chrome;
use crate::state::{Document, WindowState};
use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
};
use gpui_kit::{App, Entity, ParentElement, Styled, Window, div};

/// Closes the tab at `index`, as [`close_document`] does.
pub fn close_tab_with_prompt(
    window_state: Entity<WindowState>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let doc = window_state.read(cx).documents.get(index).cloned();
    if let Some(doc) = doc {
        close_document(window_state, doc, window, cx);
    }
}

/// Closes `doc`'s tab, asking first about unsaved changes. Its saves end
/// first, so that one in progress isn't taken for changes it would lose,
/// and one that fails is asked about. A window that's closing goes on to
/// its other documents once the tab has closed.
pub fn close_document(
    window_state: Entity<WindowState>,
    doc: Entity<Document>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(save) = doc.read(cx).pending_save() {
        window
            .spawn(cx, async move |cx| {
                let _ = save.await;
                cx.update(|window, cx| close_document(window_state, doc, window, cx))
                    .ok();
            })
            .detach();
        return;
    }
    // Closed meanwhile, such as by another close that waited too.
    let Some(index) = window_state.read(cx).index_of(&doc) else {
        return;
    };
    doc.update(cx, |doc, cx| doc.check_dirty(cx));
    if doc.read(cx).dirty {
        window_state.update(cx, |state, cx| state.switch_to_tab(index, cx));
        show_save_dialog(window_state, doc, window, cx);
    } else {
        window_state.update(cx, |state, cx| state.force_close_tab(index, window, cx));
        if window_state.read(cx).is_closing_window {
            close_step(window_state, window, cx);
        }
    }
}

/// Whether the window can close now. When it can't, because a document has
/// unsaved changes or is being saved, it goes on closing once its saves
/// have ended and the user has answered about each change.
pub fn attempt_close_window(
    window_state: Entity<WindowState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    window_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
    });

    let state = window_state.read(cx);
    if state.has_dirty_documents(cx) || has_pending_saves(state, cx) {
        window_state.update(cx, |state, _| {
            state.is_closing_window = true;
        });
        close_step(window_state, window, cx);
        false
    } else {
        state.app_state.read(cx).save_vocabulary_on_close(cx);
        true
    }
}

/// Whether a save the window waits for before it closes hasn't ended. It
/// waits for its documents' saves, and for the settings' when it's the last
/// window, which jot may quit with.
fn has_pending_saves(state: &WindowState, cx: &App) -> bool {
    !state.pending_saves(cx).is_empty()
        || (cx.windows().len() <= 1 && state.app_state.read(cx).is_saving_settings())
}

/// A step of closing the window, taken when closing starts and again after
/// each wait and answer. It looks at the window as it is now. Once the
/// saves it waits for have ended, it asks about the next document with
/// unsaved changes, or closes the window when, in this same step, none has
/// any. Quitting then goes on to the other windows.
pub fn close_step(window_state: Entity<WindowState>, window: &mut Window, cx: &mut App) {
    let state = window_state.read(cx);
    if has_pending_saves(state, cx) {
        let saves = state.pending_saves(cx);
        let settings_saved = state.app_state.read(cx).settings_saved();
        let window_state = window_state.clone();
        window
            .spawn(cx, async move |cx| {
                for save in saves {
                    let _ = save.await;
                }
                settings_saved.await;
                cx.update(|window, cx| close_step(window_state, window, cx))
                    .ok();
            })
            .detach();
        return;
    }

    let dirty = window_state.update(cx, |state, cx| {
        state.refresh_all_dirty_states(cx);
        state.documents.iter().find(|d| d.read(cx).dirty).cloned()
    });

    if let Some(doc) = dirty {
        if let Some(index) = window_state.read(cx).index_of(&doc) {
            window_state.update(cx, |state, cx| {
                state.switch_to_tab(index, cx);
            });
        }
        show_save_dialog(window_state, doc, window, cx);
    } else {
        if window_state.read(cx).is_closing_window {
            window.remove_window();
            cx.defer(chrome::quit_step);
        }
    }
}

/// Asks whether to save `doc`'s changes before its tab closes.
fn show_save_dialog(
    window_state: Entity<WindowState>,
    doc: Entity<Document>,
    window: &mut Window,
    cx: &mut App,
) {
    // One question at a time, such as when two closes waited for one save.
    if window.has_active_dialog(cx) {
        return;
    }
    let title = doc.read(cx).title.clone();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let window_state_cancel = window_state.clone();

        dialog
            .title("Save changes?")
            .overlay_closable(false)
            .close_button(true)
            .keyboard(true)
            .on_cancel(move |_, _, cx| {
                window_state_cancel.update(cx, |state, cx| {
                    state.cancel_closing(cx);
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
                let doc_discard_click = doc.clone();
                let doc_save_click = doc.clone();

                DialogFooter::new()
                    .child(Button::new("cancel").outline().label("Cancel").on_click(
                        move |_, window, cx| {
                            window_state_cancel_click.update(cx, |state, cx| {
                                state.cancel_closing(cx);
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
                                let state = &window_state_discard_click;
                                if let Some(index) = state.read(cx).index_of(&doc_discard_click) {
                                    state.update(cx, |state, cx| {
                                        state.force_close_tab(index, window, cx);
                                    });
                                }
                                if state.read(cx).is_closing_window {
                                    close_step(state.clone(), window, cx);
                                }
                            }),
                    )
                    .child(Button::new("save").primary().label("Save").on_click(
                        move |_, window, cx| {
                            window.close_dialog(cx);
                            save_and_close(
                                window,
                                window_state_save_click.clone(),
                                doc_save_click.clone(),
                                cx,
                            );
                        },
                    ))
            })
    });
}

/// Save, in the question about unsaved changes. Saves `doc`, by Save As
/// when it has no file yet, and once the save has ended closes its tab, or
/// asks again about changes typed meanwhile. A save that fails, or a Save
/// As that's cancelled, stops the closing.
pub(crate) fn save_and_close(
    window: &Window,
    window_state: Entity<WindowState>,
    doc: Entity<Document>,
    cx: &App,
) {
    window
        .spawn(cx, async move |cx| {
            let path = match doc.read_with(cx, |doc, _| doc.save_path()) {
                Some(path) => Some(path),
                None => rfd::AsyncFileDialog::new()
                    .add_filter("Text Files", &["txt"])
                    .add_filter("All Files", &["*"])
                    .set_file_name("untitled.txt")
                    .save_file()
                    .await
                    .map(|file| file.path().to_path_buf()),
            };
            let save = path.and_then(|path| {
                cx.update(|window, cx| {
                    window_state.update(cx, |state, cx| {
                        state.save_document(doc.clone(), path, window, cx);
                    });
                    doc.read(cx).pending_save()
                })
                .ok()
                .flatten()
            });
            let saved = match save {
                Some(save) => save.await.is_ok(),
                None => false,
            };
            cx.update(|window, cx| {
                if saved {
                    close_document(window_state, doc, window, cx);
                } else {
                    window_state.update(cx, |state, cx| state.cancel_closing(cx));
                }
            })
            .ok();
        })
        .detach();
}
