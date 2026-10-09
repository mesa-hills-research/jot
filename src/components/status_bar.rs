//! Status bar at the bottom of the window.

use crate::state::{AppEvent, AppState};
use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div, px,
};

/// Status bar showing cursor position, zoom level, etc.
pub struct StatusBar {
    app_state: Entity<AppState>,
    /// Redraws when the active document's caret or keybinding mode changes.
    active_editor: Option<Subscription>,
    _app_subscription: Subscription,
}

impl StatusBar {
    pub fn new(app_state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let app_subscription = cx.subscribe(&app_state, |this, _, event, cx| {
            if matches!(
                event,
                AppEvent::ActiveTabChanged(_) | AppEvent::TabAdded(_) | AppEvent::TabRemoved(_)
            ) {
                this.observe_active_editor(cx);
            }
        });
        let mut status_bar = Self {
            app_state,
            active_editor: None,
            _app_subscription: app_subscription,
        };
        status_bar.observe_active_editor(cx);
        status_bar
    }

    fn observe_active_editor(&mut self, cx: &mut Context<Self>) {
        let editor_state = self
            .app_state
            .read(cx)
            .active_document()
            .map(|doc| doc.read(cx).editor_state.clone());
        self.active_editor =
            editor_state.map(|editor_state| cx.observe(&editor_state, |_, _, cx| cx.notify()));
        cx.notify();
    }
}

impl Render for StatusBar {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        let state = self.app_state.read(cx);
        let settings = &state.settings;

        let (cursor_info, mode) = if let Some(doc) = state.active_document() {
            let editor = doc.read(cx).editor_state.read(cx);
            let pos = editor.cursor_position();
            (
                format!("Ln {}, Col {}", pos.line + 1, pos.character + 1),
                editor.keymap_mode_label(),
            )
        } else {
            ("Ln 1, Col 1".to_string(), None)
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
                    // The keybinding scheme's mode, such as Vim's NORMAL.
                    .when_some(mode, |this, mode| {
                        this.child(div().text_color(cx.theme().foreground).child(mode))
                    })
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
