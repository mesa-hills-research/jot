//! UI components for Jot.

mod dialogs;
mod editor;
// Not used yet.
#[allow(dead_code)]
mod kbd;
mod settings_panel;
mod status_bar;
mod tab;
mod tab_bar;
mod title_bar;

pub use dialogs::*;
pub use editor::*;
pub use settings_panel::*;
pub use status_bar::*;
pub use tab::*;
pub use tab_bar::*;
pub use title_bar::*;

/// View modes for the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Editor,
    Settings,
}
