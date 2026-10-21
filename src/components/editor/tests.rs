//! The editor's right-click menu, in a jot window on GPUI's test platform.

// Named one by one, since `gpui_kit::*` would bring in GPUI's `test`
// attribute in place of the built-in one.
use crate::launch::{jot_app, open_window};
use crate::state::{AppState, Settings};
use gpui_kit::component::input::TextareaState;
use gpui_kit::writing::Writing;
use gpui_kit::{
    ClipboardItem, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext,
    VisualTestContext, point, px,
};

const TEXT: &str = "Hello world\nSecond line";

/// The platform's primary modifier, as the editor's shortcuts use it.
const MODIFIER: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

/// A jot window, with `TEXT` in its editor.
fn open(cx: &mut TestAppContext) -> (VisualTestContext, Entity<TextareaState>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let settings = Settings::default();
        AppState::init_for_test(settings.clone(), Writing::in_memory(), cx);
        crate::actions::init(&settings, cx);
    });
    let window = cx.update(|cx| open_window(Vec::new(), cx)).unwrap();
    let mut cx = VisualTestContext::from_window(window, cx);
    cx.run_until_parked();
    let editor = cx.update(|window, cx| {
        let window_state = jot_app(window, cx).unwrap().read(cx).window_state().clone();
        let document = window_state.read(cx).active_document().unwrap().clone();
        let editor = document.read(cx).editor_state.clone();
        editor.update(cx, |editor, cx| editor.set_value(TEXT, window, cx));
        editor
    });
    draw(&mut cx);
    (cx, editor)
}

/// Runs what is pending and draws the window.
fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

fn right_click(position: Point<Pixels>, cx: &mut VisualTestContext) {
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    draw(cx);
}

/// Presses the item of the open menu that shows `key` as its shortcut.
fn press_menu_item(key: &str, cx: &mut VisualTestContext) {
    let selector: &'static str = format!("kbd:{MODIFIER}-{key}").leak();
    let item = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("the menu shows {selector}"))
        .center();
    cx.simulate_mouse_move(item, None, Modifiers::default());
    cx.simulate_click(item, Modifiers::default());
    draw(cx);
}

fn selected(editor: &Entity<TextareaState>, cx: &VisualTestContext) -> std::ops::Range<usize> {
    editor.read_with(cx, |editor, _| editor.selected_range())
}

/// After Select All, from the keyboard or from the editor's menu, a
/// right-click past the end of the text or in the empty space below it keeps
/// the selection. The menu's Copy then copies all of the text, and its Paste
/// replaces it.
#[gpui_kit::test]
fn a_right_click_past_the_text_keeps_select_all(cx: &mut TestAppContext) {
    let (mut cx, editor) = open(cx);
    let end = editor.read_with(&cx, |editor, _| {
        editor.range_to_bounds(&(TEXT.len()..TEXT.len())).unwrap()
    });
    let past_the_end = end.center() + point(px(60.), px(0.));
    let bottom = editor.read_with(&cx, |editor, _| editor.input_bounds().bottom());
    let below = point(end.left() + px(4.), bottom - px(4.));
    assert!(
        below.y > end.bottom() + px(20.),
        "the click is below the text"
    );

    cx.simulate_keystrokes(&format!("{MODIFIER}-a"));
    draw(&mut cx);
    assert_eq!(selected(&editor, &cx), 0..TEXT.len());
    right_click(past_the_end, &mut cx);
    assert_eq!(selected(&editor, &cx), 0..TEXT.len());
    press_menu_item("c", &mut cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(TEXT.to_string())
    );
    assert_eq!(selected(&editor, &cx), 0..TEXT.len());

    // A click in "world", then Select All from the menu opened there, a
    // right-click below the text and Paste.
    let world = editor.read_with(&cx, |editor, _| editor.range_to_bounds(&(7..8)).unwrap());
    cx.simulate_click(world.left_center(), Modifiers::default());
    draw(&mut cx);
    assert_eq!(selected(&editor, &cx), 7..7);
    right_click(world.left_center(), &mut cx);
    press_menu_item("a", &mut cx);
    assert_eq!(selected(&editor, &cx), 0..TEXT.len());
    right_click(below, &mut cx);
    assert_eq!(selected(&editor, &cx), 0..TEXT.len());
    cx.write_to_clipboard(ClipboardItem::new_string("Pasted".into()));
    press_menu_item("v", &mut cx);
    assert_eq!(editor.read_with(&cx, |editor, _| editor.value()), "Pasted");
}
