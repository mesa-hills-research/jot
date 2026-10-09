//! Status bar at the bottom of the window.

use crate::state::AppState;
use gpui_kit::{div, px, Entity, IntoElement, InteractiveElement, ParentElement, Render, Styled, Window};
use gpui_kit::component::{ActiveTheme, h_flex};

/// Status bar showing cursor position, zoom level, etc.
pub struct StatusBar {
    app_state: Entity<AppState>,
}

impl StatusBar {
    pub fn new(app_state: Entity<AppState>) -> Self {
        Self { app_state }
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        let state = self.app_state.read(cx);
        let settings = &state.settings;

        let cursor_info = if let Some(doc) = state.active_document() {
            let editor = doc.read(cx).editor_state.read(cx);
            let pos = editor.cursor_position();
            format!("Ln {}, Col {}", pos.line + 1, pos.character + 1)
        } else {
            "Ln 1, Col 1".to_string()
        };

        let wrap_status = if settings.word_wrap {
            "Wrap: On"
        } else {
            "Wrap: Off"
        };

        let zoom_status = format!("{}%", settings.zoom_level);

        h_flex()
            .w_full()
            .h(px(24.))
            .px_3()
            .bg(cx.theme().secondary)
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|s| s.text_color(cx.theme().foreground))
                            .child(cursor_info),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|s| s.text_color(cx.theme().foreground))
                            .child(wrap_status),
                    )
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|s| s.text_color(cx.theme().foreground))
                            .child(zoom_status),
                    )
                    .child("UTF-8"),
            )
    }
}