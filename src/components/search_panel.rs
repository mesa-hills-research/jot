use crate::state::AppState;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState, Position},
    v_flex,
};
use gpui_kit::{
    AppContext, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};

pub struct SearchPanel {
    app_state: Entity<AppState>,
    search_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    focus_handle: FocusHandle,
    match_count: usize,
    current_match: usize,
    match_positions: Vec<usize>,
    _subscriptions: Vec<Subscription>,
}

impl SearchPanel {
    pub fn new(
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find..."));
        let replace_input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with..."));
        let focus_handle = cx.focus_handle();

        let search_sub = cx.subscribe(&search_input, {
            let app_state = app_state.clone();
            move |this, _, event, cx| {
                if let InputEvent::Change = event {
                    this.perform_search(&app_state, cx);
                }
            }
        });

        Self {
            app_state,
            search_input,
            replace_input,
            focus_handle,
            match_count: 0,
            current_match: 0,
            match_positions: Vec::new(),
            _subscriptions: vec![search_sub],
        }
    }

    fn perform_search(&mut self, app_state: &Entity<AppState>, cx: &mut gpui_kit::Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            self.match_count = 0;
            self.current_match = 0;
            self.match_positions.clear();
            cx.notify();
            return;
        }

        let state = app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let content = doc.read(cx).content(cx);
            self.match_positions = content.match_indices(&query).map(|(pos, _)| pos).collect();
            self.match_count = self.match_positions.len();
            self.current_match = if self.match_positions.is_empty() {
                0
            } else {
                1
            };
            cx.notify();
        }
    }

    fn find_next(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        if self.match_positions.is_empty() {
            return;
        }

        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }

        let state = self.app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let current_cursor = editor_state.read(cx).cursor();

            let next_pos = self
                .match_positions
                .iter()
                .find(|&&pos| pos > current_cursor)
                .or_else(|| self.match_positions.first());

            if let Some(&pos) = next_pos {
                let line_info = self.offset_to_line_col(&self.app_state, pos, cx);
                editor_state.update(cx, |state, cx| {
                    state.set_cursor_position(
                        Position {
                            line: line_info.0 as u32,
                            character: line_info.1 as u32,
                        },
                        window,
                        cx,
                    );
                });

                self.current_match = self
                    .match_positions
                    .iter()
                    .position(|&p| p == pos)
                    .unwrap_or(0)
                    + 1;
                cx.notify();
            }
        }
    }

    fn find_prev(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        if self.match_positions.is_empty() {
            return;
        }

        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }

        let state = self.app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let current_cursor = editor_state.read(cx).cursor();

            let prev_pos = self
                .match_positions
                .iter()
                .rev()
                .find(|&&pos| pos < current_cursor)
                .or_else(|| self.match_positions.last());

            if let Some(&pos) = prev_pos {
                let line_info = self.offset_to_line_col(&self.app_state, pos, cx);
                editor_state.update(cx, |state, cx| {
                    state.set_cursor_position(
                        Position {
                            line: line_info.0 as u32,
                            character: line_info.1 as u32,
                        },
                        window,
                        cx,
                    );
                });

                self.current_match = self
                    .match_positions
                    .iter()
                    .position(|&p| p == pos)
                    .unwrap_or(0)
                    + 1;
                cx.notify();
            }
        }
    }

    fn offset_to_line_col(
        &self,
        app_state: &Entity<AppState>,
        offset: usize,
        cx: &gpui_kit::App,
    ) -> (usize, usize) {
        let state = app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let content = doc.read(cx).content(cx);
            let mut line = 0;
            let mut col = 0;
            let mut current_offset = 0;

            for ch in content.chars() {
                if current_offset >= offset {
                    break;
                }
                if ch == '\n' {
                    line += 1;
                    col = 0;
                } else {
                    col += 1;
                }
                current_offset += ch.len_utf8();
            }
            return (line, col);
        }
        (0, 0)
    }

    fn replace_current(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let replacement = self.replace_input.read(cx).value().to_string();

        if query.is_empty() {
            return;
        }

        let state = self.app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let content = doc.read(cx).content(cx);
            let current_cursor = editor_state.read(cx).cursor();

            if let Some(&match_pos) = self
                .match_positions
                .iter()
                .find(|&&pos| pos <= current_cursor && current_cursor <= pos + query.len())
            {
                let before = &content[..match_pos];
                let after = &content[match_pos + query.len()..];
                let new_content = format!("{}{}{}", before, replacement, after);

                editor_state.update(cx, |state, cx| {
                    state.set_value(&new_content, window, cx);
                });

                self.perform_search(&self.app_state.clone(), cx);
                self.find_next(window, cx);
            }
        }
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let replacement = self.replace_input.read(cx).value().to_string();

        if query.is_empty() {
            return;
        }

        let state = self.app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let content = doc.read(cx).content(cx);
            let new_content = content.replace(&query, &replacement);

            editor_state.update(cx, |state, cx| {
                state.set_value(&new_content, window, cx);
            });

            self.match_count = 0;
            self.current_match = 0;
            self.match_positions.clear();
            cx.notify();
        }
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        let focus_handle = self.search_input.read(cx).focus_handle(cx);
        focus_handle.focus(window, cx);
    }
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut gpui_kit::Context<Self>,
    ) -> impl IntoElement {
        let state = self.app_state.read(cx);

        if !state.search_visible {
            return div().into_any_element();
        }

        let replace_mode = state.replace_mode;
        let match_info = if self.match_count > 0 {
            format!("{} of {}", self.current_match, self.match_count)
        } else {
            "No results".to_string()
        };

        v_flex()
            .w_full()
            .p_2()
            .gap_2()
            .bg(cx.theme().secondary)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .h(px(28.))
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(240.))
                            .h_full()
                            .child(Input::new(&self.search_input).small().w_full()),
                    )
                    .child(
                        Button::new("prev")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronUp)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_prev(window, cx);
                            })),
                    )
                    .child(
                        Button::new("next")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronDown)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_next(window, cx);
                            })),
                    )
                    .child(
                        Button::new("case")
                            .ghost()
                            .small()
                            .icon(IconName::CaseSensitive),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(match_info),
                    )
                    .child(
                        Button::new("close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |_, _, cx| {
                                    app_state.update(cx, |state, cx| {
                                        state.search_visible = false;
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
            )
            .when(replace_mode, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .h(px(28.))
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .w(px(240.))
                                .h_full()
                                .child(Input::new(&self.replace_input).small().w_full()),
                        )
                        .child(
                            Button::new("replace")
                                .small()
                                .outline()
                                .label("Replace")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.replace_current(window, cx);
                                })),
                        )
                        .child(
                            Button::new("replace-all")
                                .small()
                                .outline()
                                .label("Replace All")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.replace_all(window, cx);
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}
