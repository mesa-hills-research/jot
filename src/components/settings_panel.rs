use crate::fonts;
use crate::state::{AppEvent, AppState};
use gpui_kit::component::{
    ActiveTheme, IconName, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    font_picker::{FontPicker, FontPickerEvent, FontPickerState},
    h_flex,
    input::Keymap,
    notification::Notification,
    radio::RadioGroup,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    v_flex,
};
use gpui_kit::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use std::borrow::Cow;

/// The keybinding schemes the editor offers, as the settings page names them.
const KEYMAPS: [(Keymap, &str); 3] = [
    (Keymap::Cua, "Standard"),
    (Keymap::Emacs, "Emacs"),
    (Keymap::Vim, "Vim"),
];

pub struct SettingsPanel {
    app_state: Entity<AppState>,
    theme_select: Entity<SelectState<SearchableVec<SharedString>>>,
    font_picker: Entity<FontPickerState>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(app_state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let themes = Self::get_theme_names(cx);
        let current_theme = app_state.read(cx).settings.theme.clone();

        let theme_select =
            cx.new(|cx| SelectState::new(SearchableVec::new(themes), None, window, cx));

        theme_select.update(cx, |state, cx| {
            state.set_selected_value(&current_theme, window, cx);
        });

        let subscription = cx.subscribe(&theme_select, {
            let app_state = app_state.clone();
            move |_, _, event, cx| {
                if let SelectEvent::Confirm(Some(theme)) = event {
                    let theme_name = theme.clone();

                    if let Some(theme_config) = gpui_kit::component::ThemeRegistry::global(cx)
                        .themes()
                        .get(&theme_name)
                        .cloned()
                    {
                        gpui_kit::component::Theme::global_mut(cx).apply_config(&theme_config);
                    }

                    app_state.update(cx, |state, cx| {
                        state.settings.theme = theme_name;
                        state.settings.save();
                        cx.notify();
                    });
                }
            }
        });

        // The installed fonts, jot's own, and the font files the user added.
        let editor_font = app_state.read(cx).settings.editor_font.clone();
        let font_picker = cx.new(|cx| {
            FontPickerState::new(window, cx)
                .app_fonts(fonts::bundled_fonts())
                .added_fonts(fonts::user_fonts())
                .default_settings(editor_font)
        });
        let font_subscription = cx.subscribe(&font_picker, {
            let app_state = app_state.clone();
            move |_, _, event: &FontPickerEvent, cx| {
                let FontPickerEvent::Change(font) = event else {
                    return;
                };
                app_state.update(cx, |state, cx| state.set_editor_font(font.clone(), cx));
            }
        });

        Self {
            app_state,
            theme_select,
            font_picker,
            _subscriptions: vec![subscription, font_subscription],
        }
    }

    /// Asks for a font file, copies it into jot's fonts folder, registers it
    /// and makes the editor use it.
    fn add_font_file(font_picker: Entity<FontPickerState>, window: &mut Window, cx: &mut App) {
        window
            .spawn(cx, async move |cx| {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title("Add Font File")
                    .add_filter("Fonts", &["ttf", "otf", "ttc", "otc"])
                    .pick_file()
                    .await
                else {
                    return;
                };
                let path = file.path().to_path_buf();
                cx.update(|window, cx| {
                    let added = fonts::import_font_file(&path).and_then(|imported| {
                        font_picker.update(cx, |picker, cx| {
                            let families = if imported.already_imported {
                                imported.families
                            } else {
                                picker.add_fonts(vec![Cow::Owned(imported.data)], window, cx)?
                            };
                            if let Some(family) = families.first() {
                                picker.choose_family_named(family, window, cx);
                            }
                            anyhow::Ok(())
                        })
                    });
                    if let Err(error) = added {
                        log::error!("Couldn't add the font {}: {error:#}", path.display());
                        window.push_notification(Notification::error(error.to_string()), cx);
                    }
                })
                .ok();
            })
            .detach();
    }

    fn get_theme_names(cx: &App) -> Vec<SharedString> {
        let mut theme_names: Vec<SharedString> = gpui_kit::component::ThemeRegistry::global(cx)
            .themes()
            .keys()
            .cloned()
            .collect();
        theme_names.sort();
        theme_names
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.app_state.read(cx).settings.clone();
        let current_theme = settings.theme.clone();

        let themes = Self::get_theme_names(cx);
        self.theme_select.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(themes), window, cx);
            if state.selected_value() != Some(&current_theme) {
                state.set_selected_value(&current_theme, window, cx);
            }
        });

        v_flex()
            .id("settings-panel")
            .size_full()
            .p_6()
            .gap_6()
            .overflow_y_scroll()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(Button::new("back").ghost().icon(IconName::Close).on_click({
                        let app_state = self.app_state.clone();
                        move |_, window, cx| {
                            app_state.update(cx, |state, cx| {
                                state.show_editor(cx);
                                // Typing and shortcuts go to the document again.
                                state.focus_active_editor(window, cx);
                            });
                        }
                    })),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(600.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child("Appearance"),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child("Theme"))
                            .child(Select::new(&self.theme_select).w(px(240.))),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(760.))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child("Editor Font"),
                            )
                            .child(
                                Button::new("add-font-file")
                                    .label("Add Font File\u{2026}")
                                    .on_click({
                                        let font_picker = self.font_picker.clone();
                                        move |_, window, cx| {
                                            Self::add_font_file(font_picker.clone(), window, cx)
                                        }
                                    }),
                            ),
                    )
                    .child(FontPicker::new(&self.font_picker)),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(600.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child("Editor"),
                    )
                    .child(
                        Checkbox::new("word-wrap")
                            .label("Word Wrap")
                            .checked(settings.word_wrap)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.settings.word_wrap = *checked;
                                        state.settings.save();
                                        cx.emit(AppEvent::SettingsChanged);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("line-numbers")
                            .label("Show Line Numbers")
                            .checked(settings.line_numbers)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.settings.line_numbers = *checked;
                                        state.settings.save();
                                        cx.emit(AppEvent::SettingsChanged);
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("xml-auto-complete")
                            .label("Auto-complete XML tags")
                            .checked(settings.xml_auto_complete)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.settings.xml_auto_complete = *checked;
                                        state.settings.save();
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("autocomplete")
                            .label("Inline Word Suggestions")
                            .checked(settings.autocomplete)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.settings.autocomplete = *checked;
                                        state.autocomplete_enabled.set(*checked);
                                        state.settings.save();
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("spell-check")
                            .label("Spell Check")
                            .checked(settings.spell_check)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.set_spell_check(*checked, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child("Keybindings"))
                            .child(
                                RadioGroup::horizontal("keymap")
                                    .children(KEYMAPS.iter().map(|(_, label)| *label))
                                    .selected_index(
                                        KEYMAPS
                                            .iter()
                                            .position(|(keymap, _)| *keymap == settings.keymap),
                                    )
                                    .on_change({
                                        let app_state = self.app_state.clone();
                                        move |ix: &usize, _, cx| {
                                            let Some((keymap, _)) = KEYMAPS.get(*ix) else {
                                                return;
                                            };
                                            app_state.update(cx, |state, cx| {
                                                state.set_keymap(*keymap, cx);
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
    }
}
