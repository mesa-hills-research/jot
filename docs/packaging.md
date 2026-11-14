# Installing jot as an app

`cargo build --release` makes a single `jot` binary. On Windows that is all jot needs. jot.exe
carries jot's icon and the icon of the files it opens, and Settings > Add Jot to Open with lists
it in File Explorer's Open with menu. On macOS and Linux, the files in `packaging/` turn the binary
into an app with jot's icon that the system offers for text files.

## macOS

`packaging/macos` holds the bundle's `Info.plist`, the app icon `jot.icns` and the document icon
`jot-document.icns`. These commands build `Jot.app` from them.

```sh
cargo build --release
mkdir -p Jot.app/Contents/MacOS Jot.app/Contents/Resources
cp target/release/jot Jot.app/Contents/MacOS/
cp packaging/macos/Info.plist Jot.app/Contents/
cp packaging/macos/jot.icns packaging/macos/jot-document.icns Jot.app/Contents/Resources/
codesign --force --sign - Jot.app
```

`codesign --sign -` signs the bundle for this Mac only. Move `Jot.app` to Applications. The Finder
then lists jot in Open With for plain text and Markdown, and a file whose default app is jot shows
the document icon. Making jot the default is up to you, in a file's Get Info.

The bundle's version in `Info.plist` follows `Cargo.toml`, and `cargo test` fails when they differ.

## Linux

`packaging/linux` holds a desktop entry and jot's app icon in the hicolor theme's sizes. These
commands install them for your user, with jot in `~/.local/bin`.

```sh
cargo build --release
install -Dm755 target/release/jot ~/.local/bin/jot
install -Dm644 packaging/linux/MesaHillsResearch.Jot.desktop -t ~/.local/share/applications
mkdir -p ~/.local/share/icons
cp -r packaging/linux/icons/hicolor ~/.local/share/icons/
update-desktop-database ~/.local/share/applications
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor
```

A package puts the same files under `/usr/share` and jot in `/usr/bin`.

The desktop entry is named after the app ID jot's windows report, so docks and window switchers
show them under jot's name and icon. File managers draw a text file's icon from the icon theme,
whichever app opens it, so jot has no document icon on Linux.
