//! Title bar containing the menu and other window controls.

use crate::components::MenuBar;
use crate::state::AppState;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, TitleBar, h_flex};
use gpui_kit::{Entity, IntoElement, ParentElement, Render, Styled, Window, div, px};

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

        TitleBar::new().child(
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
                        .child(self.menu_bar.clone()),
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
