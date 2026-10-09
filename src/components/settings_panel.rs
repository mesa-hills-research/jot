use crate::autocomplete::AutocompleteMode;
use crate::fonts;
use crate::state::{AppState, WindowState, ZOOM_LEVELS};
use gpui_kit::component::{
    ActiveTheme, IconName, WindowExt,
    button::{Button, ButtonVariants, Toggle, ToggleGroup, ToggleVariants},
    font_picker::{FontPicker, FontPickerEvent, FontPickerState, FontSettings},
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

/// How eagerly word suggestions show, as the settings page names them.
const AUTOCOMPLETE_MODES: [(AutocompleteMode, &str); 3] = [
    (AutocompleteMode::Off, "Off"),
    (AutocompleteMode::Quiet, "Quiet"),
    (AutocompleteMode::Eager, "Eager"),
];

/// What the word suggestions row says about them, with the platform's key
/// for taking one word.
const WORD_SUGGESTIONS_HELP: &str = if cfg!(target_os = "macos") {
    "Tab finishes a word from your writing, and Cmd+Right takes one word of a longer \
     suggestion. Quiet waits until you pause."
} else {
    "Tab finishes a word from your writing, and Ctrl+Right takes one word of a longer \
     suggestion. Quiet waits until you pause."
};

/// What the Open with row says it does.
#[cfg(target_os = "windows")]
const OPEN_WITH_HELP: &str =
    "Lists Jot in File Explorer\u{2019}s Open with menu and in Default apps for text files.";

/// How wide the page's column of settings grows.
const PAGE_WIDTH: f32 = 720.;

/// The width of the dropdowns at the end of the rows, so that they line up.
const SELECT_WIDTH: f32 = 240.;

/// The settings page. The settings are every window's, so a change on one
/// window's page shows on the others' too.
pub struct SettingsPanel {
    app_state: Entity<AppState>,
    window_state: Entity<WindowState>,
    theme_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The theme names the theme menu lists.
    theme_names: Vec<SharedString>,
    zoom_select: Entity<SelectState<Vec<ZoomItem>>>,
    font_picker: Entity<FontPickerState>,
    /// The font the font picker was last given from the settings.
    picker_font: FontSettings,
    scroll_handle: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(
        app_state: Entity<AppState>,
        window_state: Entity<WindowState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
                    let theme = theme.clone();
                    app_state.update(cx, |state, cx| state.set_theme(theme, cx));
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
                .default_settings(editor_font.clone())
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
            window_state,
            theme_select,
            theme_names,
            zoom_select,
            font_picker,
            picker_font: editor_font,
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

    /// Shows the current theme, zoom and font in their controls. They can
    /// change elsewhere: themes load from files, the View menu zooms, and
    /// other windows have settings pages too.
    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.app_state.read(cx).settings;
        let (theme, zoom, font) = (
            settings.theme.clone(),
            settings.zoom_level,
            settings.editor_font.clone(),
        );
        if font != self.picker_font {
            self.picker_font = font.clone();
            self.font_picker
                .update(cx, |picker, cx| picker.set_settings(font, window, cx));
        }
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

    /// Side-by-side buttons that choose one of `choices` with `set`.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        choices: &'static [(T, &'static str)],
        current: T,
        set: fn(&mut AppState, T, &mut Context<AppState>),
    ) -> impl IntoElement {
        let current = choices
            .iter()
            .position(|(choice, _)| *choice == current)
            .unwrap_or_default();
        let app_state = self.app_state.clone();
        ToggleGroup::new(id)
            .segmented()
            .outline()
            .children(
                choices
                    .iter()
                    .enumerate()
                    .map(|(ix, (_, label))| Toggle::new(ix).label(*label).checked(ix == current)),
            )
            .on_click(move |checked: &Vec<bool>, _, cx| {
                // One is always chosen: clicking the chosen one keeps it.
                let chosen = checked
                    .iter()
                    .enumerate()
                    .find(|(ix, on)| **on && *ix != current)
                    .and_then(|(ix, _)| choices.get(ix));
                if let Some((choice, _)) = chosen {
                    app_state.update(cx, |state, cx| set(state, *choice, cx));
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

/// The button that adds Jot to File Explorer's Open with menu, or removes
/// it.
#[cfg(target_os = "windows")]
fn open_with_row(cx: &App) -> AnyElement {
    use crate::windows_shell::{OpenWith, toggle_open_with};
    let state = OpenWith::get(cx);
    let label = if state.added {
        "Remove Jot from Open with"
    } else {
        "Add Jot to Open with"
    };
    row(
        "Open with",
        Some(OPEN_WITH_HELP),
        Button::new("open-with")
            .outline()
            .label(label)
            .loading(state.busy)
            .on_click(|_, window, cx| toggle_open_with(window, cx)),
        cx,
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
                        let window_state = self.window_state.clone();
                        move |_, window, cx| {
                            window_state.update(cx, |state, cx| {
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
                    Some(WORD_SUGGESTIONS_HELP),
                    self.segmented(
                        "autocomplete",
                        &AUTOCOMPLETE_MODES,
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
                self.segmented("keymap", &KEYMAPS, settings.keymap, AppState::set_keymap),
                cx,
            )],
            cx,
        );

        let page = v_flex()
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
            .child(keyboard);
        #[cfg(target_os = "windows")]
        let page = page.child(section("Files", vec![open_with_row(cx)], cx));

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
                    .child(page),
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
