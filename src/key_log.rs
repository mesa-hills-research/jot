//! Recording how the documents are typed, with times, to tune when word
//! suggestions show. It runs only when jot starts with `JOT_KEY_LOG=1`.
//!
//! Each document's editing goes to a file of its own in the `keylog` folder
//! beside the settings (`%APPDATA%\jot\keylog` on Windows), one JSON Lines
//! file per document from when it opens until it closes. GPUI Kit's
//! `KeyLog` describes what a file holds. Its header also says how Quiet was
//! timed, `fixed` or `rhythm` (with `JOT_QUIET_RHYTHM=1`). Nothing leaves
//! this machine.

use gpui_kit::component::input::{KeyLog, TextareaState};
use gpui_kit::writing::{QuietTiming, Writing};
use gpui_kit::{App, Global};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// The folder the recordings go to, set while recording.
struct KeyLogFolder(PathBuf);

impl Global for KeyLogFolder {}

/// Starts recording when `JOT_KEY_LOG` is `1` and the folder can be made.
pub fn init(cx: &mut App) {
    if std::env::var_os("JOT_KEY_LOG").is_none_or(|value| value != "1") {
        return;
    }
    let Some(folder) = dirs::config_dir().map(|dir| dir.join("jot").join("keylog")) else {
        log::error!("JOT_KEY_LOG is set, but there's no settings folder to record keys in");
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&folder) {
        log::error!(
            "Couldn't make {} to record keys in: {error}",
            folder.display()
        );
        return;
    }
    cx.set_global(KeyLogFolder(folder));
}

/// Whether the documents' typing is being recorded.
pub fn is_recording(cx: &App) -> bool {
    cx.has_global::<KeyLogFolder>()
}

/// Records the editing of `state`, a document's editor, while recording.
pub fn record(state: TextareaState, writing: &Writing, cx: &App) -> TextareaState {
    static DOCUMENTS: AtomicU32 = AtomicU32::new(0);
    let Some(KeyLogFolder(folder)) = cx.try_global::<KeyLogFolder>() else {
        return state;
    };
    let name = format!(
        "{}-{}-{}.jsonl",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"),
        std::process::id(),
        DOCUMENTS.fetch_add(1, Ordering::Relaxed),
    );
    let timing = match writing.quiet_timing() {
        QuietTiming::FixedPause => "fixed",
        QuietTiming::Rhythm => "rhythm",
    };
    let writing = writing.clone();
    let mode = move || format!("{:?}", writing.suggestion_mode()).to_lowercase();
    state.key_log(
        KeyLog::new(folder.join(name))
            .mode(mode)
            .header("timing", timing),
    )
}

#[cfg(test)]
mod tests {
    use super::KeyLogFolder;
    use crate::launch::open_window;
    use crate::state::{AppState, Settings};
    use gpui_kit::writing::{QuietTiming, Writing};
    use gpui_kit::{TestAppContext, VisualTestContext};

    /// Types into a new window while recording, with Quiet timed by
    /// `timing`, and returns the recording's events, its header first.
    fn record_typing(cx: &mut TestAppContext, timing: QuietTiming) -> Vec<serde_json::Value> {
        let folder = std::env::temp_dir().join(format!(
            "jot-key-log-test-{}-{timing:?}",
            std::process::id()
        ));
        _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            let settings = Settings::default();
            let writing = Writing::in_memory();
            writing.set_quiet_timing(timing);
            AppState::init_for_test(settings.clone(), writing, cx);
            crate::actions::init(&settings, cx);
            cx.set_global(KeyLogFolder(folder.clone()));
        });
        let window = cx.update(|cx| open_window(Vec::new(), cx)).unwrap();
        let mut cx = VisualTestContext::from_window(window, cx);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("recording-keys").is_some());
        assert_eq!(
            cx.debug_bounds("quiet-rhythm").is_some(),
            timing == QuietTiming::Rhythm
        );

        cx.simulate_input("Hi there");
        let files: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        let log = std::fs::read_to_string(&files[0]).unwrap();
        _ = std::fs::remove_dir_all(&folder);
        log.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// While recording, the title bar says so, and a document's typing goes
    /// to a file of its own that starts with the text it had, the mode and
    /// how Quiet was timed.
    #[gpui_kit::test]
    fn a_document_records_its_typing(cx: &mut TestAppContext) {
        let events = record_typing(cx, QuietTiming::FixedPause);
        assert_eq!(events[0]["text"], "");
        assert_eq!(events[0]["mode"], "quiet");
        assert_eq!(events[0]["timing"], "fixed");
        let typed: String = events
            .iter()
            .filter(|event| event["k"] == "type")
            .map(|event| event["s"].as_str().unwrap())
            .collect();
        assert_eq!(typed, "Hi there");
    }

    /// With Quiet timed by the typist's rhythm, the title bar says Rhythm
    /// and the recording says so too.
    #[gpui_kit::test]
    fn a_recording_names_rhythm_timing(cx: &mut TestAppContext) {
        let events = record_typing(cx, QuietTiming::Rhythm);
        assert_eq!(events[0]["timing"], "rhythm");
    }
}
