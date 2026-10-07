use crate::state::{AppEvent, AppState};
use gpui::{div, px, Entity, IntoElement, Render, Styled, Subscription, Window, prelude::*};
use gpui_component::{
    input::{Input, InputEvent, InputState, Position},
    ActiveTheme,
};

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input",
    "link", "meta", "param", "source", "track", "wbr",
];

pub struct Editor {
    app_state: Entity<AppState>,
    active_doc_subscription: Option<Subscription>,
    _app_subscriptions: Vec<Subscription>,
    last_text_len: usize,
}

impl Editor {
    pub fn new(
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let mut editor = Self {
            app_state: app_state.clone(),
            active_doc_subscription: None,
            _app_subscriptions: Vec::new(),
            last_text_len: 0,
        };

        editor.subscribe_to_app(app_state, window, cx);
        editor.update_active_document_subscription(window, cx);
        editor
    }

    fn subscribe_to_app(
        &mut self,
        app_state: Entity<AppState>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let sub = cx.subscribe_in(&app_state, window, |this, _, event, window, cx| {
            match event {
                AppEvent::ActiveTabChanged(_) | AppEvent::TabAdded(_) => {
                    this.update_active_document_subscription(window, cx);
                }
                _ => {}
            }
        });
        self._app_subscriptions.push(sub);
    }

    fn update_active_document_subscription(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.active_doc_subscription = None;

        let state = self.app_state.read(cx);
        if let Some(doc) = state.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let app_state = self.app_state.clone();

            self.last_text_len = editor_state.read(cx).text().len();

            self.active_doc_subscription = Some(cx.subscribe_in(
                &editor_state,
                window,
                move |this, editor_state, event, window, cx| {
                    if let InputEvent::Change = event {
                        this.handle_text_change(&editor_state, &app_state, window, cx);
                    }
                },
            ));
        }
    }

    fn handle_text_change(
        &mut self,
        editor_state: &Entity<InputState>,
        app_state: &Entity<AppState>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let xml_enabled = app_state.read(cx).settings.xml_auto_complete;

        let (current_text, cursor, current_len, cursor_pos) = {
            let state = editor_state.read(cx);
            (
                state.value().to_string(),
                state.cursor(),
                state.text().len(),
                state.cursor_position(),
            )
        };

        let prev_len = self.last_text_len;
        self.last_text_len = current_len;

        {
            let app_state = app_state.clone();
            app_state.update(cx, |state, cx| {
                if let Some(doc) = state.active_document().cloned() {
                    doc.update(cx, |doc, cx| {
                        doc.check_dirty(cx);
                    });
                }
                cx.notify();
            });
        }

        if !xml_enabled {
            return;
        }

        if current_len != prev_len + 1 {
            return;
        }

        if let Some(closing_tag) = Self::detect_xml_tag_to_close(&current_text, cursor) {
            let remainder = &current_text[cursor..];
            let next_non_whitespace = remainder.trim_start();
            if next_non_whitespace.starts_with(&closing_tag) {
                return;
            }

            editor_state.update(cx, |state, cx| {
                let insertion = format!("\n\n{}", closing_tag);

                state.insert(&insertion, window, cx);
                let target_line = cursor_pos.line + 1;
                let target_char = 0;

                state.set_cursor_position(
                    Position {
                        line: target_line,
                        character: target_char,
                    },
                    window,
                    cx,
                );
            });
        }
    }

    /// Check whether a string is a valid XML tag name.
    ///
    /// Per the XML specification, a valid name must begin with a letter
    /// (a-z, A-Z) or an underscore, followed by zero or more letters,
    /// digits, hyphens, underscores, periods, or colons.
    fn is_valid_xml_tag_name(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }

        let mut chars = name.chars();

        let first = match chars.next() {
            Some(c) => c,
            None => return false,
        };

        if !first.is_alphabetic() && first != '_' {
            return false;
        }

        chars.all(|c| {
            c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ':'
        })
    }

    fn detect_xml_tag_to_close(content: &str, cursor: usize) -> Option<String> {
        if cursor == 0 || cursor > content.len() {
            return None;
        }

        let before_cursor = content.get(..cursor)?;

        if !before_cursor.ends_with('>') {
            return None;
        }

        let last_open = before_cursor.rfind('<')?;
        let tag_content = before_cursor.get(last_open + 1..cursor - 1)?;

        if tag_content.starts_with('/')
            || tag_content.starts_with('!')
            || tag_content.starts_with('?')
            || tag_content.ends_with('/')
            || tag_content.is_empty()
        {
            return None;
        }

        let tag_name = tag_content
            .split(|c: char| c.is_whitespace())
            .next()?
            .trim();

        if tag_name.is_empty() {
            return None;
        }

        if !Self::is_valid_xml_tag_name(tag_name) {
            return None;
        }

        let tag_lower = tag_name.to_lowercase();
        if VOID_ELEMENTS.contains(&tag_lower.as_str()) {
            return None;
        }

        Some(format!("</{tag_name}>"))
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let (font_size, font_family, editor_state, _doc_id) = {
            let state = self.app_state.read(cx);
            let settings = &state.settings;

            let (editor_state, doc_id) = if let Some(doc) = state.active_document() {
                let doc = doc.read(cx);
                (Some(doc.editor_state.clone()), Some(doc.id))
            } else {
                (None, None)
            };

            (
                settings.effective_font_size(),
                settings.font_family.clone(),
                editor_state,
                doc_id,
            )
        };

        if let Some(editor_state) = editor_state {
            div()
                .flex_1()
                .size_full()
                .bg(cx.theme().background)
                .font_family(font_family)
                .text_size(px(font_size))
                .overflow_hidden()
                .child(
                    Input::new(&editor_state)
                        .appearance(false)
                        .h_full()
                        .w_full(),
                )
        } else {
            div()
                .flex_1()
                .size_full()
                .bg(cx.theme().background)
                .items_center()
                .justify_center()
                .child("No document open")
        }
    }
}