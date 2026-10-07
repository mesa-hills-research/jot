//! Keyboard shortcut display component with keycap styling.

use gpui::{
    div, App, IntoElement, Keystroke, ParentElement, RenderOnce,
    SharedString, Styled, Window,
};
use gpui_component::ActiveTheme;

/// A keyboard shortcut indicator with keycap styling.
#[derive(IntoElement)]
pub struct Kbd {
    keystroke: Keystroke,
}

impl Kbd {
    pub fn new(keystroke: Keystroke) -> Self {
        Self { keystroke }
    }

    /// Format a keystroke for display.
    pub fn format(keystroke: &Keystroke) -> SharedString {
        let mut parts = Vec::new();

        #[cfg(target_os = "macos")]
        {
            if keystroke.modifiers.control {
                parts.push("⌃");
            }
            if keystroke.modifiers.alt {
                parts.push("⌥");
            }
            if keystroke.modifiers.shift {
                parts.push("⇧");
            }
            if keystroke.modifiers.platform {
                parts.push("⌘");
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            if keystroke.modifiers.control {
                parts.push("Ctrl");
            }
            if keystroke.modifiers.alt {
                parts.push("Alt");
            }
            if keystroke.modifiers.shift {
                parts.push("Shift");
            }
            if keystroke.modifiers.platform {
                parts.push("Win");
            }
        }

        let key = match keystroke.key.as_str() {
            "escape" => "Esc",
            "enter" => "⏎",
            "backspace" => "⌫",
            "tab" => "Tab",
            "space" => "Space",
            "up" => "↑",
            "down" => "↓",
            "left" => "←",
            "right" => "→",
            key => key,
        };

        parts.push(key);

        #[cfg(target_os = "macos")]
        {
            parts.join("").into()
        }

        #[cfg(not(target_os = "macos"))]
        {
            parts.join("+").into()
        }
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let formatted = Self::format(&self.keystroke);

        div()
            .text_xs()
            .font_family("JetBrains Mono")
            .px_1()
            .py_0p5()
            .rounded_sm()
            .flex_shrink_0()
            .bg(cx.theme().muted.opacity(0.5))
            .text_color(cx.theme().muted_foreground)
            .border_1()
            .border_color(cx.theme().border.opacity(0.5))
            .child(formatted)
    }
}