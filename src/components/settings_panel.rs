use crate::state::{AppEvent, AppState};
use gpui::{
    div, px, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    ActiveTheme, IconName, h_flex, v_flex,
};

pub struct SettingsPanel {
    app_state: Entity<AppState>,
    theme_select: Entity<SelectState<SearchableVec<SharedString>>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

                    if let Some(theme_config) = gpui_component::ThemeRegistry::global(cx)
                        .themes()
                        .get(&theme_name)
                        .cloned()
                    {
                        gpui_component::Theme::global_mut(cx).apply_config(&theme_config);
                    }

                    app_state.update(cx, |state, cx| {
                        state.settings.theme = theme_name;
                        state.settings.save();
                        cx.notify();
                    });
                }
            }
        });

        Self {
            app_state,
            theme_select,
            _subscriptions: vec![subscription],
        }
    }

    fn get_theme_names(cx: &App) -> Vec<SharedString> {
        let mut theme_names: Vec<SharedString> = gpui_component::ThemeRegistry::global(cx)
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
        let (current_theme, word_wrap, line_numbers, xml_auto_complete, autocomplete, spell_check) = {
            let state = self.app_state.read(cx);
            (
                state.settings.theme.clone(),
                state.settings.word_wrap,
                state.settings.line_numbers,
                state.settings.xml_auto_complete,
                state.settings.autocomplete,
                state.settings.spell_check,
            )
        };

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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        Button::new("back")
                            .ghost()
                            .icon(IconName::Close)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |_, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.show_editor(cx);
                                    });
                                }
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(600.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::MEDIUM)
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
                    .max_w(px(600.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Editor"),
                    )
                    .child(
                        Checkbox::new("word-wrap")
                            .label("Word Wrap")
                            .checked(word_wrap)
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
                            .checked(line_numbers)
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
                            .checked(xml_auto_complete)
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
                            .checked(autocomplete)
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
                            .checked(spell_check)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |checked, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.set_spell_check(*checked, cx);
                                    });
                                }
                            }),
                    ),
            )
    }
}