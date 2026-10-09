//! Title bar containing the menu and other window controls.

use crate::chrome;
use crate::components::MenuBar;
use crate::state::AppState;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, TitleBar, h_flex};
use gpui_kit::{
    Div, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

pub struct JotTitleBar {
    app_state: Entity<AppState>,
    menu_bar: Entity<MenuBar>,
}

impl JotTitleBar {
    pub fn new(app_state: Entity<AppState>, menu_bar: Entity<MenuBar>) -> Self {
        Self {
            app_state,
            menu_bar,
        }
    }
}

impl Render for JotTitleBar {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        let state = self.app_state.read(cx);

        let (title_text, is_dirty) = if let Some(doc) = state.active_document() {
            let doc = doc.read(cx);
            (doc.title.clone(), doc.dirty)
        } else {
            ("Jot".into(), false)
        };

        let display_title = if is_dirty {
            format!("{} • - Jot", title_text)
        } else {
            format!("{} - Jot", title_text)
        };

        TitleBar::new()
            // GPUI Kit's Linux close button calls `window.remove_window()`, which
            // skips the close check. This sends it through the check instead.
            // macOS and Windows close through the system.
            .on_close_window(|_, window, cx| chrome::close_window(window, cx))
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .px_2()
                    .gap_4()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(Icon::empty())
                                    .path("icons/logo/jot-logo-grand-hotel.svg")
                                    .with_size(px(5.5))
                                    .text_color(cx.theme().foreground),
                            )
                            .child(title_bar_item(self.menu_bar.clone())),
                    )
                    .child(
                        div().flex_1().flex().items_center().justify_center().child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(display_title),
                        ),
                    )
                    .child(div().w(px(100.))),
            )
    }
}

/// Holds clickable title bar content.
///
/// GPUI Kit's `TitleBar` makes the whole bar a drag area. Without `occlude()`
/// a press on a menu turns into a window drag on Windows, and on macOS and
/// Linux the window moves if the pointer moves during the click. Text and
/// empty space stay draggable.
fn title_bar_item(child: impl IntoElement) -> Div {
    h_flex().occlude().child(child)
}
