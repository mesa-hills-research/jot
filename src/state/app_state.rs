use super::Settings;
use crate::fonts::{self, DEFAULT_EDITOR_FONT};
use crate::theme;
use futures::FutureExt as _;
use futures::future::Shared;
use gpui_kit::component::font_picker::FontSettings;
use gpui_kit::component::input::Keymap;
use gpui_kit::writing::{SuggestionMode, WordList, Writing, WritingOptions};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Context, Entity, FontWeight, Global, SharedString, Task,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How often the word suggestions' vocabulary is saved while something new
/// has been learned, besides when a file is saved, a tab or a window closes,
/// and jot quits.
const VOCABULARY_SAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How jot writes a file whole, given its path and its new contents. Tests
/// put a slower or failing one in its place.
pub type FileWriter = Arc<dyn Fn(&Path, &[u8]) -> io::Result<()> + Send + Sync>;

/// What every window shares: the settings, the spelling dictionary, the word
/// suggestions' vocabulary and pacing, and which window was used last.
///
/// A change to the settings notifies the entity, and each window applies it
/// to its own documents.
pub struct AppState {
    pub settings: Settings,
    /// The spelling dictionary, the word suggestions' vocabulary, their
    /// mode, and how the user has been answering them this session.
    pub writing: Writing,
    /// The settings file, or `None` in tests and when the file couldn't be
    /// read, so that jot doesn't save over it.
    settings_file: Option<PathBuf>,
    /// Writes the settings while they keep changing, one write at a time,
    /// and ends once the file has the latest.
    settings_save: Option<Shared<Task<()>>>,
    /// Whether `settings_save` is running.
    saving_settings: bool,
    /// Whether the settings changed since `settings_save` last took them.
    settings_unsaved: bool,
    /// How files are written.
    pub file_writer: FileWriter,
    /// Whether Quit is under way. Cancel in a question about unsaved
    /// changes stops it.
    pub quitting: bool,
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
    /// Loads the settings, and starts loading the dictionary and the
    /// vocabulary, for every window to share.
    pub fn init(cx: &mut App) -> Entity<AppState> {
        let (mut settings, settings_file) = load_settings();
        let missing_font = use_an_available_editor_font(&mut settings, cx);
        let writing = Writing::load(writing_options(), cx);
        let state = Self::create(settings, writing, settings_file, cx);
        state.update(cx, |state, _| state.missing_font = missing_font);
        state
    }

    /// App-wide state that saves no settings, for tests.
    #[cfg(test)]
    pub fn init_for_test(settings: Settings, writing: Writing, cx: &mut App) -> Entity<AppState> {
        Self::create(settings, writing, None, cx)
    }

    /// Saves the settings to `path` from now on, for tests.
    #[cfg(test)]
    pub fn set_settings_file(&mut self, path: PathBuf) {
        self.settings_file = Some(path);
    }

    fn create(
        settings: Settings,
        writing: Writing,
        settings_file: Option<PathBuf>,
        cx: &mut App,
    ) -> Entity<AppState> {
        writing.set_suggestion_mode(settings.autocomplete);
        let state = cx.new(|cx: &mut Context<Self>| {
            // Learning is saved on the way out, at logoff or shutdown too, and
            // every few minutes in case jot doesn't get to quit. Quitting
            // waits for the settings' latest save too, which Quit has done
            // before it gets here.
            let save_on_quit = cx.on_app_quit(|this: &mut Self, _| {
                this.writing.save();
                this.settings_saved()
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
                settings,
                writing,
                settings_file,
                settings_save: None,
                saving_settings: false,
                settings_unsaved: false,
                file_writer: Arc::new(|path, data| {
                    crate::file_write::write_whole(path, data, &backup_folder())
                }),
                quitting: false,
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

    /// Ends once the settings file has the latest settings.
    pub fn settings_saved(&self) -> impl Future<Output = ()> + use<> {
        let save = self.saving_settings.then(|| self.settings_save.clone());
        async move {
            if let Some(Some(save)) = save {
                save.await;
            }
        }
    }

    /// Whether the settings are being written.
    pub fn is_saving_settings(&self) -> bool {
        self.saving_settings
    }

    /// Saves the vocabulary without holding up the windows.
    pub fn save_vocabulary_in_background(&self, cx: &App) {
        self.writing.save_in_background(cx).detach();
    }

    /// Saves the vocabulary as a window closes, in the background. When jot
    /// quits with it, the last window on Windows and Linux, quitting waits
    /// for that save, a second at most, with the window already gone.
    pub fn save_vocabulary_on_close(&self, cx: &App) {
        self.save_vocabulary_in_background(cx);
    }

    /// Saves the settings on a thread of their own and tells every window,
    /// which applies them to its documents. Changes made while a write runs
    /// are written together once it ends.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        if self.settings_file.is_some() {
            self.settings_unsaved = true;
            if !self.saving_settings {
                self.saving_settings = true;
                let save = cx.spawn(async move |this, cx| {
                    while let Ok(Some((settings, path, write))) =
                        this.update(cx, |this, _| this.take_unsaved_settings())
                    {
                        let saved =
                            on_own_thread(move || save_settings(&settings, &path, &write)).await;
                        if let Err(error) = saved {
                            log::error!("Couldn\u{2019}t save the settings. {error}");
                        }
                    }
                });
                self.settings_save = Some(save.shared());
            }
        }
        cx.notify();
    }

    /// The settings to write next, with their file and the writer, or
    /// `None`, which ends the writing, when the file has them.
    fn take_unsaved_settings(&mut self) -> Option<(Settings, PathBuf, FileWriter)> {
        let path = self.settings_file.clone()?;
        if !std::mem::take(&mut self.settings_unsaved) {
            self.saving_settings = false;
            return None;
        }
        Some((self.settings.clone(), path, self.file_writer.clone()))
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
    pub fn set_autocomplete(&mut self, mode: SuggestionMode, cx: &mut Context<Self>) {
        self.settings.autocomplete = mode;
        self.writing.set_suggestion_mode(mode);
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

    /// Switches every window to the theme named `name`, or without one to
    /// the light or dark theme the system calls for.
    pub fn set_theme(&mut self, name: Option<SharedString>, cx: &mut Context<Self>) {
        theme::apply(name.as_deref(), cx);
        self.settings.theme = name;
        self.settings_changed(cx);
    }
}

/// Runs `work` on a thread of its own, which may block for as long as it
/// needs, such as on a slow disk or for another program's save of the same
/// file, while the windows go on.
pub async fn on_own_thread<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let (sender, done) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("jot-save".into())
        .spawn(move || {
            let _ = sender.send(work());
        })?;
    done.await
        .unwrap_or_else(|_| Err(io::Error::other("The save stopped partway")))
}

/// Writes `settings` to `path` with `write`, making the folder first.
fn save_settings(settings: &Settings, path: &Path, write: &FileWriter) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    write(path, settings.to_json()?.as_bytes())
}

/// Where jot keeps a copy of a file while it writes the file in place, to
/// put back if the write fails. The folder is the user's alone, and a copy
/// is removed once its save is done.
pub fn backup_folder() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("jot")
        .join("backups")
}

/// The saved settings, and the file to save them to. A file that can't be
/// read leaves jot with the defaults, which it doesn't save over the file.
fn load_settings() -> (Settings, Option<PathBuf>) {
    let Some(path) = Settings::config_path() else {
        return (Settings::default(), None);
    };
    match Settings::load(&path) {
        Ok(settings) => (settings, Some(path)),
        Err(error) => {
            log::error!(
                "Couldn\u{2019}t read the settings in {}, so jot won\u{2019}t save them this time. {error}",
                path.display()
            );
            (Settings::default(), None)
        }
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

/// Where jot keeps its writing aids' files, as it always has: in
/// `<config>/jot`, with `vocabulary.json` and `user-dictionary.txt`, and with
/// `dictionary.txt` there read as a word list besides English.
fn writing_options() -> WritingOptions {
    let Some(folder) = dirs::config_dir().map(|config| config.join("jot")) else {
        return WritingOptions::new();
    };
    WritingOptions::new()
        .with_word_lists([
            WordList::English,
            WordList::File(folder.join("dictionary.txt")),
        ])
        .with_data_dir(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{TestAppContext, px};

    /// A change made in jot is in the settings file for its next start.
    #[gpui_kit::test]
    fn changed_settings_load_at_the_next_start(cx: &mut TestAppContext) {
        let folder =
            std::env::temp_dir().join(format!("jot-app-state-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let file = folder.join("jot").join("settings.json");
        let state = cx.update(|cx| {
            gpui_kit::init(cx);
            theme::init(None, cx);
            AppState::create(
                Settings::default(),
                Writing::in_memory(),
                Some(file.clone()),
                cx,
            )
        });
        state.update(cx, |state, cx| {
            state.set_theme(Some("Gruvbox Dark".into()), cx);
            state.set_editor_font(FontSettings::new("Inconsolata").with_size(px(16.)), cx);
            state.toggle_word_wrap(cx);
            state.toggle_line_numbers(cx);
            state.set_xml_auto_complete(true, cx);
            state.set_autocomplete(SuggestionMode::Eager, cx);
            state.set_spell_check(false, cx);
            state.zoom_in(cx);
            state.set_keymap(Keymap::Vim, cx);
            state.set_smooth_caret(true, cx);
        });
        cx.executor().allow_parking();
        let saved = cx.read(|cx| state.read(cx).settings_saved());
        cx.foreground_executor().block_test(saved);

        let settings = cx.read(|cx| state.read(cx).settings.clone());
        assert_eq!(settings.theme.as_deref(), Some("Gruvbox Dark"));
        assert_eq!(settings.zoom_level, 110);
        assert_eq!(Settings::load(&file).unwrap(), settings);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// Settings changed several times at once are written once, with the
    /// latest of them.
    #[gpui_kit::test]
    fn settings_changed_at_once_are_written_once(cx: &mut TestAppContext) {
        use std::sync::Mutex;
        let folder =
            std::env::temp_dir().join(format!("jot-app-state-test-at-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let file = folder.join("jot").join("settings.json");
        let state = cx.update(|cx| {
            gpui_kit::init(cx);
            theme::init(None, cx);
            AppState::create(
                Settings::default(),
                Writing::in_memory(),
                Some(file.clone()),
                cx,
            )
        });
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writer: FileWriter = {
            let writes = writes.clone();
            let backups = folder.join("backups");
            Arc::new(move |path, data| {
                writes
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(data).into_owned());
                crate::file_write::write_whole(path, data, &backups)
            })
        };
        state.update(cx, |state, _| state.file_writer = writer);
        cx.executor().allow_parking();

        state.update(cx, |state, cx| {
            state.toggle_word_wrap(cx);
            state.zoom_in(cx);
            state.set_keymap(Keymap::Vim, cx);
        });
        let saved = cx.read(|cx| state.read(cx).settings_saved());
        cx.foreground_executor().block_test(saved);

        let settings = cx.read(|cx| state.read(cx).settings.clone());
        assert_eq!(*writes.lock().unwrap(), [settings.to_json().unwrap()]);
        assert_eq!(Settings::load(&file).unwrap(), settings);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
