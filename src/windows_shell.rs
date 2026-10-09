//! Windows shell integration: jot in File Explorer's Open with menu for text
//! files, and a taskbar jump list with New Window, New Document and the
//! files used recently.
//!
//! The registry changes for Open with and the list of recent files are
//! plain data, worked out apart from the Win32 calls that apply them.

mod open_with;
mod recent;

#[cfg(target_os = "windows")]
mod jump_list;
#[cfg(target_os = "windows")]
mod registry;

#[cfg(target_os = "windows")]
pub use jump_list::file_used;
#[cfg(target_os = "windows")]
pub use registry::{OpenWith, toggle_open_with};

use crate::instance::Kind;
use std::path::Path;

/// The AppUserModelID that Windows groups jot's windows by, under one
/// taskbar button with one jump list.
pub const APP_ID: &str = "MesaHillsResearch.Jot";

/// jot's name wherever Windows shows it.
pub const APP_NAME: &str = "Jot";

/// The tasks of the jump list, in order. GPUI starts jot with
/// `--dock-action <index>` for one, and the jot that starts passes what it
/// asks for to the running jot. New Window is a launch without files, which
/// opens a window.
pub const TASKS: [(&str, Kind); 2] = [
    ("New Window", Kind::Open),
    ("New Document", Kind::NewDocument),
];

/// What the jump list's task number `index` asks for. A task this jot
/// doesn't have asks for a window.
pub fn task(index: &str) -> Kind {
    match index
        .parse::<usize>()
        .ok()
        .and_then(|index| TASKS.get(index))
    {
        Some((_, kind)) => *kind,
        None => {
            log::warn!("jot has no jump list task {index}");
            Kind::Open
        }
    }
}

/// Whether `a` and `b` name the same file as Windows compares names, which
/// is without regard to case.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b || a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Gives jot its AppUserModelID. Call it before the first window opens.
#[cfg(target_os = "windows")]
pub fn set_app_identity(cx: &mut gpui_kit::App) {
    cx.set_app_identity(APP_ID, APP_NAME);
}

/// Brings Open with up to date when jot.exe has moved, and fills the jump
/// list.
#[cfg(target_os = "windows")]
pub fn init(cx: &mut gpui_kit::App) {
    registry::check_open_with(cx);
    jump_list::refresh(cx);
}
