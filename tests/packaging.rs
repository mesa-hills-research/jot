//! The files that install jot as an app on macOS and Linux name jot's
//! version and binary, and icons that are there.

use std::path::{Path, PathBuf};

fn packaging(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("packaging")
        .join(path)
}

/// The string after `<key>{key}</key>` in an Info.plist.
fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let (_, after) = plist.split_once(&format!("<key>{key}</key>"))?;
    let value = after.trim_start().strip_prefix("<string>")?;
    Some(&value[..value.find("</string>")?])
}

#[test]
fn the_mac_bundle_names_jots_version_binary_and_icons() {
    let plist = std::fs::read_to_string(packaging("macos/Info.plist")).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        plist_string(&plist, "CFBundleShortVersionString"),
        Some(version)
    );
    assert_eq!(plist_string(&plist, "CFBundleVersion"), Some(version));
    assert_eq!(
        plist_string(&plist, "CFBundleExecutable"),
        Some(env!("CARGO_PKG_NAME"))
    );
    // The app icon and the icon of the files jot opens.
    for key in ["CFBundleIconFile", "CFBundleTypeIconFile"] {
        let icon = plist_string(&plist, key).unwrap();
        let icns = std::fs::read(packaging(&format!("macos/{icon}.icns"))).unwrap();
        assert_eq!(&icns[..4], b"icns", "{icon}");
    }
}

#[test]
fn the_linux_desktop_entry_has_its_icon_in_every_size() {
    let entry = std::fs::read_to_string(packaging("linux/MesaHillsResearch.Jot.desktop")).unwrap();
    let icon = entry
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap();
    for size in [16, 24, 32, 48, 128, 256] {
        let png = packaging(&format!(
            "linux/icons/hicolor/{size}x{size}/apps/{icon}.png"
        ));
        assert!(png.is_file(), "{}", png.display());
    }
    let svg = packaging(&format!("linux/icons/hicolor/scalable/apps/{icon}.svg"));
    assert!(svg.is_file(), "{}", svg.display());
}
