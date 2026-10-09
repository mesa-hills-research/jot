use crate::fonts;
use crate::state::{AppState, ZOOM_LEVELS};
use gpui_kit::component::{
    ActiveTheme, IconName, WindowExt,
    button::{Button, ButtonVariants, Toggle, ToggleGroup, ToggleVariants},
    font_picker::{FontPicker, FontPickerEvent, FontPickerState},
    h_flex,
    input::Keymap,
    notification::Notification,
    scroll::{Scrollbar, ScrollbarMode},
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    switch::Switch,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::FluentBuilder, px,
};
use std::borrow::Cow;

/// The keybinding schemes the editor offers, as the settings page names them.
const KEYMAPS: [(Keymap, &str); 3] = [
    (Keymap::Cua, "Standard"),
    (Keymap::Emacs, "Emacs"),
    (Keymap::Vim, "Vim"),
];

/// How wide the page's column of settings grows.
const PAGE_WIDTH: f32 = 720.;

/// The width of the dropdowns at the end of the rows, so that they line up.
const SELECT_WIDTH: f32 = 240.;

pub struct SettingsPanel {
    app_state: Entity<AppState>,
    theme_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The theme names the theme menu lists.
    theme_names: Vec<SharedString>,
    zoom_select: Entity<SelectState<Vec<ZoomItem>>>,
    font_picker: Entity<FontPickerState>,
    scroll_handle: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(app_state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme_names = Self::get_theme_names(cx);
        let current_theme = app_state.read(cx).settings.theme.clone();

        let theme_select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(theme_names.clone()), None, window, cx)
                .searchable(true)
        });
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

        let zoom = app_state.read(cx).settings.zoom_level;
        let zoom_select = cx.new(|cx| {
            let items = ZOOM_LEVELS.iter().copied().map(ZoomItem::new).collect();
            SelectState::new(items, None, window, cx)
        });
        zoom_select.update(cx, |state, cx| state.set_selected_value(&zoom, window, cx));
        let zoom_subscription = cx.subscribe(&zoom_select, {
            let app_state = app_state.clone();
            move |_, _, event: &SelectEvent<Vec<ZoomItem>>, cx| {
                if let SelectEvent::Confirm(Some(zoom)) = event {
                    let zoom = *zoom;
                    app_state.update(cx, |state, cx| state.set_zoom_level(zoom, cx));
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
            theme_names,
            zoom_select,
            font_picker,
            scroll_handle: ScrollHandle::new(),
            _subscriptions: vec![subscription, zoom_subscription, font_subscription],
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

    /// Shows the current theme and zoom in their menus. Both can change
    /// elsewhere: themes load from files, and the View menu zooms.
    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.app_state.read(cx).settings;
        let (theme, zoom) = (settings.theme.clone(), settings.zoom_level);
        let theme_names = Self::get_theme_names(cx);
        let themes_changed = theme_names != self.theme_names;
        if themes_changed {
            self.theme_names = theme_names.clone();
        }
        self.theme_select.update(cx, |state, cx| {
            if themes_changed {
                state.set_items(SearchableVec::new(theme_names), window, cx);
            }
            if themes_changed || state.selected_value() != Some(&theme) {
                state.set_selected_value(&theme, window, cx);
            }
        });
        self.zoom_select.update(cx, |state, cx| {
            if state.selected_value() != Some(&zoom) {
                state.set_selected_value(&zoom, window, cx);
            }
        });
    }

    /// A switch that turns a setting on and off with `set`.
    fn switch(
        &self,
        id: &'static str,
        label: &'static str,
        checked: bool,
        set: fn(&mut AppState, bool, &mut Context<AppState>),
    ) -> Switch {
        let app_state = self.app_state.clone();
        Switch::new(id)
            .checked(checked)
            .accessibility_label(label)
            .on_change(move |checked, _, cx| {
                app_state.update(cx, |state, cx| set(state, *checked, cx));
            })
    }

    fn keymap_toggles(&self, current: Keymap) -> impl IntoElement {
        let current = KEYMAPS
            .iter()
            .position(|(keymap, _)| *keymap == current)
            .unwrap_or_default();
        let app_state = self.app_state.clone();
        ToggleGroup::new("keymap")
            .segmented()
            .outline()
            .children(
                KEYMAPS
                    .iter()
                    .enumerate()
                    .map(|(ix, (_, label))| Toggle::new(ix).label(*label).checked(ix == current)),
            )
            .on_click(move |checked: &Vec<bool>, _, cx| {
                // One scheme is always chosen: clicking the chosen one keeps it.
                let chosen = checked
                    .iter()
                    .enumerate()
                    .find(|(ix, on)| **on && *ix != current)
                    .and_then(|(ix, _)| KEYMAPS.get(ix));
                if let Some((keymap, _)) = chosen {
                    app_state.update(cx, |state, cx| state.set_keymap(*keymap, cx));
                }
            })
    }
}

/// A section of the page: a heading over a card of rows.
fn section(title: &'static str, rows: Vec<AnyElement>, cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
        .child(
            v_flex()
                .w_full()
                .p_4()
                .gap_4()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
                .bg(cx.theme().group_box)
                .children(rows),
        )
}

/// A setting: its name, with a line of help under it when `help` is given,
/// at the start of the row, and its control at the end. The compact font
/// picker lays out its rows the same way.
fn row(
    title: &'static str,
    help: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .gap_4()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(title))
                .when_some(help, |this, help| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(help),
                    )
                }),
        )
        .child(h_flex().flex_shrink_0().child(control))
        .into_any_element()
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_selects(window, cx);
        let settings = self.app_state.read(cx).settings.clone();

        let header = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
            .child(
                Button::new("back")
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip("Close Settings")
                    .on_click({
                        let app_state = self.app_state.clone();
                        move |_, window, cx| {
                            app_state.update(cx, |state, cx| {
                                state.show_editor(cx);
                                // Typing and shortcuts go to the document again.
                                state.focus_active_editor(window, cx);
                            });
                        }
                    }),
            );

        let appearance = section(
            "Appearance",
            vec![
                row(
                    "Theme",
                    None,
                    Select::new(&self.theme_select)
                        .id("theme")
                        .search_placeholder("Search themes")
                        .w(px(SELECT_WIDTH)),
                    cx,
                ),
                row(
                    "Zoom",
                    Some("Scales the editor\u{2019}s text. The View menu zooms too."),
                    Select::new(&self.zoom_select)
                        .id("zoom")
                        .w(px(SELECT_WIDTH)),
                    cx,
                ),
            ],
            cx,
        );

        let font = section(
            "Font",
            vec![
                FontPicker::new(&self.font_picker)
                    .compact()
                    .into_any_element(),
                row(
                    "Font files",
                    Some("Adds a .ttf or .otf file to the fonts. Jot keeps a copy."),
                    Button::new("add-font-file")
                        .outline()
                        .label("Add Font File\u{2026}")
                        .on_click({
                            let font_picker = self.font_picker.clone();
                            move |_, window, cx| {
                                Self::add_font_file(font_picker.clone(), window, cx)
                            }
                        }),
                    cx,
                ),
            ],
            cx,
        );

        let editor = section(
            "Editor",
            vec![
                row(
                    "Line numbers",
                    None,
                    self.switch(
                        "line-numbers",
                        "Line numbers",
                        settings.line_numbers,
                        AppState::set_line_numbers,
                    ),
                    cx,
                ),
                row(
                    "Word wrap",
                    Some("Wraps long lines at the edge of the window."),
                    self.switch(
                        "word-wrap",
                        "Word wrap",
                        settings.word_wrap,
                        AppState::set_word_wrap,
                    ),
                    cx,
                ),
                row(
                    "Smooth caret",
                    Some("The caret glides to where it moves."),
                    self.switch(
                        "smooth-caret",
                        "Smooth caret",
                        settings.smooth_caret,
                        AppState::set_smooth_caret,
                    ),
                    cx,
                ),
            ],
            cx,
        );

        let writing = section(
            "Writing",
            vec![
                row(
                    "Spell check",
                    Some("Underlines misspelled words. Right-click one for suggestions."),
                    self.switch(
                        "spell-check",
                        "Spell check",
                        settings.spell_check,
                        AppState::set_spell_check,
                    ),
                    cx,
                ),
                row(
                    "Word suggestions",
                    Some(
                        "Offers the rest of a word as you type, learned from your writing. \
                         Tab accepts it.",
                    ),
                    self.switch(
                        "autocomplete",
                        "Word suggestions",
                        settings.autocomplete,
                        AppState::set_autocomplete,
                    ),
                    cx,
                ),
                row(
                    "Close XML tags",
                    Some("Typing the > of an opening tag adds its closing tag."),
                    self.switch(
                        "xml-auto-complete",
                        "Close XML tags",
                        settings.xml_auto_complete,
                        AppState::set_xml_auto_complete,
                    ),
                    cx,
                ),
            ],
            cx,
        );

        let keyboard = section(
            "Keyboard",
            vec![row(
                "Keybindings",
                Some("The keys for moving around and editing a document."),
                self.keymap_toggles(settings.keymap),
                cx,
            )],
            cx,
        );

        div()
            .id("settings-panel")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .id("settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .items_center()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(PAGE_WIDTH))
                            .px_8()
                            .py_6()
                            .gap_6()
                            .child(header)
                            .child(appearance)
                            .child(font)
                            .child(editor)
                            .child(writing)
                            .child(keyboard),
                    ),
            )
            .child(
                // Always shown, so the page reads as one that scrolls.
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("settings-scrollbar")
                        .mode(ScrollbarMode::Always),
                ),
            )
    }
}

/// A zoom level in the zoom menu, such as "125%".
#[derive(Clone, Debug)]
struct ZoomItem {
    zoom: u32,
    title: SharedString,
}

impl ZoomItem {
    fn new(zoom: u32) -> Self {
        Self {
            zoom,
            title: format!("{zoom}%").into(),
        }
    }
}

impl SelectItem for ZoomItem {
    type Value = u32;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &u32 {
        &self.zoom
    }
}
