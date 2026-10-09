use super::Settings;
use crate::autocomplete::{AutocompleteMode, SharedVocabulary, SuggestionPacing};
use crate::fonts::{self, DEFAULT_EDITOR_FONT};
use gpui_kit::component::font_picker::FontSettings;
use gpui_kit::component::input::Keymap;
use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Context, Entity, FontWeight, Global, SharedString, Task,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// How often the word suggestions' vocabulary is saved while something new
/// has been learned, besides when a file is saved, a tab or a window closes,
/// and jot quits.
const VOCABULARY_SAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// What every window shares: the settings, the word suggestions' vocabulary
/// and pacing, and which window was used last.
///
/// A change to the settings notifies the entity, and each window applies it
/// to its own documents.
pub struct AppState {
    pub settings: Settings,
    /// The word suggestions' mode, and how the user has been answering them
    /// this session.
    pub suggestion_pacing: Rc<SuggestionPacing>,
    pub shared_vocabulary: Rc<RefCell<SharedVocabulary>>,
    /// Where the settings are saved: the settings file, or nowhere in tests.
    settings_file: Option<PathBuf>,
    /// A font the settings named that the text system lacks. The first
    /// window says so.
    missing_font: Option<SharedString>,
    /// The windows, the most recently active first. Closed ones drop out
    /// when the list is read.
    recent_windows: Vec<AnyWindowHandle>,
    _save_on_quit: gpui_kit::Subscription,
    _vocabulary_saves: Task<()>,
}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    /// Loads the settings and the vocabulary, for every window to share.
    pub fn init(cx: &mut App) -> Entity<AppState> {
        let mut settings = Settings::load();
        let missing_font = use_an_available_editor_font(&mut settings, cx);
        let state = Self::create(
            settings,
            SharedVocabulary::load(),
            Settings::config_path(),
            cx,
        );
        state.update(cx, |state, _| state.missing_font = missing_font);
        state
    }

    /// App-wide state that saves no settings, for tests.
    #[cfg(test)]
    pub fn init_for_test(
        settings: Settings,
        vocabulary: SharedVocabulary,
        cx: &mut App,
    ) -> Entity<AppState> {
        Self::create(settings, vocabulary, None, cx)
    }

    fn create(
        settings: Settings,
        vocabulary: SharedVocabulary,
        settings_file: Option<PathBuf>,
        cx: &mut App,
    ) -> Entity<AppState> {
        let state = cx.new(|cx: &mut Context<Self>| {
            // Learning is saved on the way out, at logoff or shutdown too, and
            // every few minutes in case jot doesn't get to quit.
            let save_on_quit = cx.on_app_quit(|this: &mut Self, _| {
                this.shared_vocabulary.borrow_mut().save();
                async {}
            });
            let vocabulary_saves = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(VOCABULARY_SAVE_INTERVAL)
                        .await;
                    let saved = this.update(cx, |this, cx| this.save_vocabulary_in_background(cx));
                    if saved.is_err() {
                        break;
                    }
                }
            });
            Self {
                suggestion_pacing: Rc::new(SuggestionPacing::new(settings.autocomplete)),
                settings,
                shared_vocabulary: Rc::new(RefCell::new(vocabulary)),
                settings_file,
                missing_font: None,
                recent_windows: Vec::new(),
                _save_on_quit: save_on_quit,
                _vocabulary_saves: vocabulary_saves,
            }
        });
        cx.set_global(GlobalAppState(state.clone()));
        state
    }

    /// The state every window shares.
    pub fn global(cx: &App) -> Entity<AppState> {
        cx.global::<GlobalAppState>().0.clone()
    }

    /// The font the settings named that isn't available, the first time
    /// it is asked for.
    pub fn take_missing_font(&mut self) -> Option<SharedString> {
        self.missing_font.take()
    }

    /// Notes that `window` is the one in use now.
    pub fn window_activated(&mut self, window: AnyWindowHandle) {
        self.recent_windows.retain(|recent| *recent != window);
        self.recent_windows.insert(0, window);
    }

    /// The open windows, the most recently active first.
    pub fn recent_windows(&mut self, cx: &App) -> Vec<AnyWindowHandle> {
        let open = cx.windows();
        self.recent_windows.retain(|window| open.contains(window));
        // A window that was never active, such as one just opened, last.
        for window in open {
            if !self.recent_windows.contains(&window) {
                self.recent_windows.push(window);
            }
        }
        self.recent_windows.clone()
    }

    /// Saves the vocabulary without holding up the windows.
    pub fn save_vocabulary_in_background(&self, cx: &App) {
        SharedVocabulary::save_in_background(&self.shared_vocabulary, cx).detach();
    }

    /// Saves the vocabulary as a window closes. When jot quits with it, the
    /// last window on Windows and Linux, the save finishes first. Otherwise
    /// it runs in the background, and quitting saves later.
    pub fn save_vocabulary_on_close(&self, cx: &App) {
        let quits = cx.windows().len() <= 1 && !cfg!(target_os = "macos");
        if quits {
            self.shared_vocabulary.borrow_mut().save();
        } else {
            self.save_vocabulary_in_background(cx);
        }
    }

    /// Saves the settings and tells every window, which applies them to its
    /// documents.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.settings_file {
            self.settings.save_to(path);
        }
        cx.notify();
    }

    pub fn set_spell_check(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.spell_check = enabled;
        self.settings_changed(cx);
    }

    /// Draws every document in `font`.
    pub fn set_editor_font(&mut self, font: FontSettings, cx: &mut Context<Self>) {
        if self.settings.editor_font == font {
            return;
        }
        self.settings.editor_font = font;
        self.settings_changed(cx);
    }

    /// Switches every open document, and new ones, to `keymap`.
    pub fn set_keymap(&mut self, keymap: Keymap, cx: &mut Context<Self>) {
        self.settings.keymap = keymap;
        self.settings_changed(cx);
    }

    pub fn set_line_numbers(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.line_numbers = enabled;
        self.settings_changed(cx);
    }

    pub fn set_word_wrap(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.word_wrap = enabled;
        self.settings_changed(cx);
    }

    pub fn toggle_line_numbers(&mut self, cx: &mut Context<Self>) {
        self.set_line_numbers(!self.settings.line_numbers, cx);
    }

    pub fn toggle_word_wrap(&mut self, cx: &mut Context<Self>) {
        self.set_word_wrap(!self.settings.word_wrap, cx);
    }

    /// Scales the editor's text, in percent of the font size.
    pub fn set_zoom_level(&mut self, zoom_level: u32, cx: &mut Context<Self>) {
        self.settings.zoom_level = zoom_level;
        self.settings_changed(cx);
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.settings.zoom_in();
        self.settings_changed(cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.settings.zoom_out();
        self.settings_changed(cx);
    }

    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.settings.reset_zoom();
        self.settings_changed(cx);
    }

    /// Sets how eagerly every document offers word suggestions, from its
    /// next keystroke.
    pub fn set_autocomplete(&mut self, mode: AutocompleteMode, cx: &mut Context<Self>) {
        self.settings.autocomplete = mode;
        self.suggestion_pacing.set_mode(mode);
        self.settings_changed(cx);
    }

    /// Turns the closing of XML tags on or off.
    pub fn set_xml_auto_complete(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.xml_auto_complete = enabled;
        self.settings_changed(cx);
    }

    /// Turns the smooth caret on or off in every open document, and new ones.
    pub fn set_smooth_caret(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.smooth_caret = enabled;
        self.settings_changed(cx);
    }

    /// Switches every window to the theme named `name`.
    pub fn set_theme(&mut self, name: SharedString, cx: &mut Context<Self>) {
        if let Some(theme_config) = ThemeRegistry::global(cx).themes().get(&name).cloned() {
            // `update` also rebuilds the Base layer's copy of the theme,
            // which draws the scrollbars.
            Theme::update(cx, |theme| theme.apply_config(&theme_config));
        }
        self.settings.theme = name;
        self.settings_changed(cx);
    }
}

/// Switches the editor to the default font when the settings name one the
/// text system doesn't have, such as a font file that was deleted. Returns
/// the missing family.
fn use_an_available_editor_font(settings: &mut Settings, cx: &App) -> Option<SharedString> {
    let family = settings.editor_font.family().clone();
    if fonts::is_available(&family, cx) {
        return None;
    }
    settings.editor_font = settings
        .editor_font
        .clone()
        .with_family(DEFAULT_EDITOR_FONT)
        .with_weight(FontWeight::NORMAL)
        .with_italic(false);
    Some(family)
}
