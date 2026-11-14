//! jot.exe carries two icons. The first is jot's own, and the one with
//! resource ID 2 is the icon of the files jot opens, which Open with names
//! as `jot.exe,-2`.
#![cfg(windows)]

use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
use windows::core::HSTRING;

#[test]
fn jot_exe_has_its_own_icon_and_the_document_icon() {
    let exe = HSTRING::from(env!("CARGO_BIN_EXE_jot"));
    // An index of -1 with no icons asked for counts them.
    // SAFETY: no handles are written.
    let count = unsafe { ExtractIconExW(&exe, -1, None, None, 0) };
    assert_eq!(count, 2);
    // Explorer reads `jot.exe,0` and `jot.exe,-2` the same way.
    for index in [0, -2] {
        let mut icon = HICON::default();
        // SAFETY: one icon is asked for, and `icon` holds one.
        let extracted = unsafe { ExtractIconExW(&exe, index, Some(&mut icon), None, 1) };
        assert_eq!(extracted, 1, "jot.exe,{index}");
        // SAFETY: ExtractIconExW made it, and nothing else holds it.
        unsafe { DestroyIcon(icon) }.unwrap();
    }
}
