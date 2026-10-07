//! Theme utilities for Jot.

use std::path::PathBuf;

/// Locates the themes directory relative to the current working directory or executable.
pub fn find_themes_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;

    let local = cwd.join("themes");
    if local.exists() {
        return Some(local);
    }

    let up_two = cwd.join("../../themes");
    if up_two.exists() {
        return Some(up_two);
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let exe_themes = exe_dir.join("themes");
            if exe_themes.exists() {
                return Some(exe_themes);
            }
        }
    }

    None
}