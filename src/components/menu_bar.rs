use crate::actions::*;
use crate::state::AppState;
use gpui_kit::component::{
    Selectable, Sizable,
    button::{Button, ButtonVariants},
    menu::PopupMenu,
};
use gpui_kit::{
    App, AppContext, ClickEvent, Context, DismissEvent, Entity, Focusable, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};

pub struct MenuBar {
    app_state: Entity<AppState>,
    active_menu: Option<usize>,
    popup_menus: [Option<Entity<PopupMenu>>; 4],
    _subscriptions: Vec<Subscription>,
}

impl MenuBar {
    pub fn new(_app_state: Entity<AppState>) -> Entity<Self> {
        unimplemented!("Must be created with MenuBar::build")
    }

    pub fn build(app_state: Entity<AppState>, cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            app_state,
            active_menu: None,
            popup_menus: [None, None, None, None],
            _subscriptions: Vec::new(),
        })
    }

    fn set_active_menu(
        &mut self,
        index: Option<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_menu == index {
            return;
        }

        if let Some(old_idx) = self.active_menu {
            self.popup_menus[old_idx] = None;
        }

        self.active_menu = index;
        self._subscriptions.clear();
        cx.notify();
    }

    fn handle_menu_click(
        &mut self,
        index: usize,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_index = if self.active_menu == Some(index) {
            None
        } else {
            Some(index)
        };
        self.set_active_menu(new_index, window, cx);
    }

    fn handle_menu_hover(
        &mut self,
        index: usize,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !hovered {
            return;
        }

        if self.active_menu.is_some() && self.active_menu != Some(index) {
            self.set_active_menu(Some(index), window, cx);
        }
    }

    fn handle_dismiss(
        &mut self,
        _: &Entity<PopupMenu>,
        _: &DismissEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_active_menu(None, window, cx);
    }

    fn build_file_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            menu.menu("New Tab", Box::new(NewTab))
                .menu("New Window", Box::new(NewWindow))
                .menu("New from Template...", Box::new(NewFromTemplate))
                .menu("Open...", Box::new(OpenFile))
                .separator()
                .menu("Save", Box::new(Save))
                .menu("Save As...", Box::new(SaveAs))
                .separator()
                .menu("Close Tab", Box::new(CloseTab))
                .menu("Close Window", Box::new(CloseWindow))
        });

        self._subscriptions
            .push(cx.subscribe_in(&menu, window, Self::handle_dismiss));
        menu.read(cx).focus_handle(cx).focus(window, cx);

        menu
    }

    fn build_edit_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            menu.menu("Undo", Box::new(Undo))
                .menu("Redo", Box::new(Redo))
                .separator()
                .menu("Cut", Box::new(Cut))
                .menu("Copy", Box::new(Copy))
                .menu("Paste", Box::new(Paste))
                .separator()
                .menu("Select All", Box::new(SelectAll))
                .separator()
                .menu("Find...", Box::new(Find))
                .menu("Replace...", Box::new(Replace))
                .menu("Go to Line...", Box::new(GoToLine))
        });

        self._subscriptions
            .push(cx.subscribe_in(&menu, window, Self::handle_dismiss));
        menu.read(cx).focus_handle(cx).focus(window, cx);

        menu
    }

    fn build_view_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        let state = self.app_state.read(cx);
        let word_wrap = state.settings.word_wrap;
        let line_numbers = state.settings.line_numbers;

        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.menu("Zoom In", Box::new(ZoomIn))
                .menu("Zoom Out", Box::new(ZoomOut))
                .menu("Reset Zoom", Box::new(ResetZoom))
                .separator()
                .menu_with_check("Word Wrap", word_wrap, Box::new(ToggleWordWrap))
                .menu_with_check("Line Numbers", line_numbers, Box::new(ToggleLineNumbers))
                .separator()
                .menu("Settings", Box::new(OpenSettings))
        });

        self._subscriptions
            .push(cx.subscribe_in(&menu, window, Self::handle_dismiss));
        menu.read(cx).focus_handle(cx).focus(window, cx);

        menu
    }

    fn build_help_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            menu.menu("Keyboard Shortcuts", Box::new(OpenSettings))
                .separator()
                .menu("About Jot", Box::new(OpenSettings))
        });

        self._subscriptions
            .push(cx.subscribe_in(&menu, window, Self::handle_dismiss));
        menu.read(cx).focus_handle(cx).focus(window, cx);

        menu
    }

    fn get_or_build_menu(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PopupMenu> {
        if let Some(menu) = &self.popup_menus[index] {
            return menu.clone();
        }

        let menu = match index {
            0 => self.build_file_menu(window, cx),
            1 => self.build_edit_menu(window, cx),
            2 => self.build_view_menu(window, cx),
            3 => self.build_help_menu(window, cx),
            _ => unreachable!(),
        };

        self.popup_menus[index] = Some(menu.clone());
        menu
    }

    fn render_menu_button(
        &mut self,
        index: usize,
        label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.active_menu == Some(index);
        let button_id: SharedString = format!("menu-{}", index).into();

        div()
            .id(index)
            .relative()
            .child(
                Button::new(button_id)
                    .ghost()
                    .small()
                    .label(label)
                    .selected(is_active)
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, ev, window, cx| {
                        this.handle_menu_click(index, ev, window, cx);
                    })),
            )
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                this.handle_menu_hover(index, *hovered, window, cx);
            }))
            .when(is_active, |this| {
                let menu = self.get_or_build_menu(index, window, cx);
                this.child(deferred(
                    anchored()
                        .anchor(gpui_kit::Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(div().occlude().top_1().child(menu)),
                ))
            })
    }
}

impl Render for MenuBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_0()
            .child(self.render_menu_button(0, "File", window, cx))
            .child(self.render_menu_button(1, "Edit", window, cx))
            .child(self.render_menu_button(2, "View", window, cx))
            .child(self.render_menu_button(3, "Help", window, cx))
    }
}
