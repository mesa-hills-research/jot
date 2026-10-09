# jot

jot is a fast, native text editor written in Rust on [GPUI](https://www.gpui.rs), for Windows,
macOS and Linux. It fills the space between a simple editor like Notepad and a word processor:
your files stay plain text, and jot helps you write them.

- Spell checking as you type, with suggestions and Add to Dictionary
- Word completion that learns from your own writing, stored on your computer, and by default
  waits until you pause
- Tabs, find and replace, Go to Line, word wrap, line numbers and zoom
- Several windows in one jot, and files opened with `jot notes.txt` or Open With join the
  running jot as tabs
- On Windows, a place in File Explorer's Open with menu for text files, added from Settings, and
  a taskbar jump list with New Window, New Document and your recent files
- An optional smooth caret that glides as you type and move around
- Light and dark color themes
- New from Template, which starts a new document from any file

Words you add to the dictionary go to `user-dictionary.txt` in jot's settings folder:
`%APPDATA%\jot` on Windows, `~/Library/Application Support/jot` on macOS and `~/.config/jot` on
Linux.

## Status

jot is moving from gpui-component 0.5.0 to [GPUI Kit](https://github.com/mesa-hills-research/gpui_kit).
That work happens on the `gpui-kit` branch, which is the one to build and run.

## Building

The `gpui-kit` branch builds from its GitHub dependencies. `main` expects a local copy of
gpui-component 0.5.0 at the path in its `Cargo.toml`.

You need [Rust](https://rustup.rs/) and the tools GPUI builds with:

- **Windows**: Visual Studio Build Tools with the C++ workload and the Windows SDK
- **macOS**: Xcode
- **Linux**: the X11, Wayland and Vulkan development packages, as listed in Zed's
  [Linux build guide](https://zed.dev/docs/development/linux)

Then:

```sh
git clone https://github.com/mesa-hills-research/jot
cd jot
git checkout gpui-kit
cargo run --release
```

jot loads its color themes from a `themes` folder in the working directory or next to the
executable, so run it from the repository or copy `themes` beside the binary.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

The bundled fonts, icons, color themes and spelling word list keep their own licenses, listed in
[NOTICE](NOTICE).
