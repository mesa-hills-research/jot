//! Tab bar containing document tabs.

use super::JotTab;
use crate::components::close_tab_with_prompt;
use crate::state::AppState;
use gpui_kit::component::{ActiveTheme, Selectable, h_flex};
use gpui_kit::{
    Entity, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};

/// Tab bar displaying all open documents.
pub struct JotTabBar {
    app_state: Entity<AppState>,
}

impl JotTabBar {
    pub fn new(app_state: Entity<AppState>) -> Self {
        Self { app_state }
    }
}

impl Render for JotTabBar {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        let state = self.app_state.read(cx);
        let active_index = state.active_index;

        div()
            .w_full()
            .h(px(32.))
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .id("tab-bar-scroll")
                    .h_full()
                    .overflow_x_scroll()
                    .children(state.documents.iter().enumerate().map(|(index, doc)| {
                        let doc_read = doc.read(cx);
                        let title = doc_read.title.clone();
                        let dirty = doc_read.dirty;
                        let selected = index == active_index;
                        let app_state = self.app_state.clone();

                        JotTab::new(SharedString::from(format!("tab-{}", index)), title)
                            .dirty(dirty)
                            .selected(selected)
                            .on_click({
                                let app_state = app_state.clone();
                                move |_, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.switch_to_tab(index, cx);
                                    });
                                }
                            })
                            .on_close({
                                let app_state = app_state.clone();
                                move |_, window, cx| {
                                    close_tab_with_prompt(app_state.clone(), index, window, cx);
                                }
                            })
                            .on_middle_click({
                                let app_state = app_state.clone();
                                move |_, window, cx| {
                                    close_tab_with_prompt(app_state.clone(), index, window, cx);
                                }
                            })
                    })),
            )
    }
}
