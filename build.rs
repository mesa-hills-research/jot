fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        // The app icon has the lowest ID, so Explorer and the windows take
        // it as jot.exe's own. Files that jot opens show the document icon,
        // which Open with names by its ID (`jot.exe,-2`).
        res.set_icon("packaging/windows/jot-app.ico")
            .set_icon_with_id("packaging/windows/jot-document.ico", "2");
        res.compile().expect("failed to compile windows resources");
    }
}
