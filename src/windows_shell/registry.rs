//! Applies the Open with changes to the registry, reads what an earlier Add
//! left there, and runs the Settings page's button.

use super::open_with::{self, Installed, Op, Value};
use gpui_kit::component::{WindowExt, notification::Notification};
use gpui_kit::{App, Global, Window};
use std::path::PathBuf;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::Error;
use windows_registry::{CURRENT_USER, Type};

type Result<T> = windows::core::Result<T>;

/// Whether jot is in Open with, as the Settings page shows it.
#[derive(Clone, Copy, Default)]
pub struct OpenWith {
    pub added: bool,
    /// A change, or the check at startup, is under way.
    pub busy: bool,
}

impl Global for OpenWith {}

impl OpenWith {
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    fn set(self, cx: &mut App) {
        cx.set_global(self);
        cx.refresh_windows();
    }
}

/// Reads whether jot is in Open with. When it is, but for a jot.exe
/// somewhere else or for other types, it brings the registration up to
/// date for this jot: the user asked for jot to be there.
pub fn check_open_with(cx: &mut App) {
    OpenWith {
        added: false,
        busy: true,
    }
    .set(cx);
    let checked = cx.background_executor().spawn(async move {
        let installed = installed()?;
        if let Some(installed) = &installed
            && let Some(ops) = open_with::update(installed, &current_exe()?)
        {
            log::info!("Bringing Open with up to date for this jot");
            apply(&ops)?;
        }
        anyhow::Ok(installed.is_some())
    });
    cx.spawn(async move |cx| {
        let added = checked.await.unwrap_or_else(|error| {
            log::error!("Couldn\u{2019}t check jot\u{2019}s place in Open with: {error:#}");
            false
        });
        cx.update(|cx| OpenWith { added, busy: false }.set(cx));
    })
    .detach();
}

/// Adds jot to Open with, or removes it when it is there.
pub fn toggle_open_with(window: &mut Window, cx: &mut App) {
    let state = OpenWith::get(cx);
    if state.busy {
        return;
    }
    let add = !state.added;
    OpenWith {
        busy: true,
        ..state
    }
    .set(cx);
    let window = window.window_handle();
    let changed = cx.background_executor().spawn(async move {
        let changed = change_open_with(add);
        // What is there now, also after a change that stopped partway.
        let added = installed().map(|installed| installed.is_some());
        (changed, added.unwrap_or(!add))
    });
    cx.spawn(async move |cx| {
        let (changed, added) = changed.await;
        cx.update(|cx| {
            if let Err(error) = changed {
                let message = if add {
                    format!("Couldn\u{2019}t add Jot to Open with: {error:#}")
                } else {
                    format!("Couldn\u{2019}t remove Jot from Open with: {error:#}")
                };
                log::error!("{message}");
                window
                    .update(cx, |_, window, cx| {
                        window.push_notification(Notification::error(message), cx)
                    })
                    .ok();
            }
            OpenWith { added, busy: false }.set(cx);
        });
    })
    .detach();
}

fn change_open_with(add: bool) -> anyhow::Result<()> {
    let exe = current_exe()?;
    if add {
        apply(&open_with::add(&exe))?;
    } else if let Some(installed) = installed()? {
        apply(&open_with::remove(&installed, &exe))?;
    }
    Ok(())
}

/// The running jot.exe, whose path goes in the registry as text.
fn current_exe() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    anyhow::ensure!(
        exe.to_str().is_some(),
        "the path of jot.exe isn\u{2019}t valid Unicode"
    );
    Ok(exe)
}

/// What an earlier Add left in the registry, or `None` when jot isn't in
/// Open with.
fn installed() -> Result<Option<Installed>> {
    let command = CURRENT_USER
        .open(open_with::command_key())
        .and_then(|key| key.get_string(""));
    let command = match command {
        Ok(command) => command,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let extensions = match CURRENT_USER.open(open_with::file_associations_key()) {
        Ok(key) => key.values()?.map(|(name, _)| name).collect(),
        Err(error) if is_missing(&error) => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(Some(Installed {
        exe: open_with::exe_in_command(&command),
        extensions,
    }))
}

/// Makes the changes in `ops`, in order, then tells File Explorer that the
/// types' apps changed.
fn apply(ops: &[Op]) -> Result<()> {
    let result = ops.iter().try_for_each(apply_one);
    // Also after a change that stopped partway, which changed some.
    // SAFETY: SHCNE_ASSOCCHANGED takes no items.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    result
}

fn apply_one(op: &Op) -> Result<()> {
    match op {
        Op::Set { key, name, value } => {
            let key = CURRENT_USER.create(key)?;
            match value {
                Value::Text(text) => key.set_string(name, text),
                Value::Empty => key.set_bytes(name, Type::Other(0), &[]),
            }
        }
        Op::DeleteValue { key, name } => {
            let key = CURRENT_USER.options().read().write().open(key);
            missing_is_fine(key.and_then(|key| key.remove_value(name)))
        }
        Op::DeleteTree { key } => missing_is_fine(CURRENT_USER.remove_tree(key)),
        Op::DeleteIfEmpty { key } => {
            let empty = match CURRENT_USER.open(key) {
                Ok(opened) => opened.keys()?.next().is_none() && opened.values()?.next().is_none(),
                Err(error) => return missing_is_fine(Err(error)),
            };
            if empty {
                missing_is_fine(CURRENT_USER.remove_tree(key))
            } else {
                Ok(())
            }
        }
    }
}

fn is_missing(error: &Error) -> bool {
    error.code() == ERROR_FILE_NOT_FOUND.to_hresult()
}

fn missing_is_fine(result: Result<()>) -> Result<()> {
    match result {
        Err(error) if is_missing(&error) => Ok(()),
        result => result,
    }
}
