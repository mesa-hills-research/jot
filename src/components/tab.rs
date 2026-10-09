use gpui_kit::{
    div, px, App, ClickEvent, ElementId, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, prelude::FluentBuilder, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable, h_flex};
use std::rc::Rc;

#[derive(IntoElement)]
pub struct JotTab {
    id: ElementId,
    title: SharedString,
    dirty: bool,
    selected: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    on_close: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    on_middle_click: Option<Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>>,
}

impl JotTab {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            dirty: false,
            selected: false,
            on_click: None,
            on_close: None,
            on_middle_click: None,
        }
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    pub fn on_close(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }

    pub fn on_middle_click(
        mut self,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_middle_click = Some(Rc::new(f));
        self
    }
}

impl Selectable for JotTab {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for JotTab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let bg = if self.selected {
            cx.theme().tab_active
        } else {
            cx.theme().tab
        };

        let fg = if self.selected {
            cx.theme().tab_active_foreground
        } else {
            cx.theme().tab_foreground
        };

        let border_color = if self.selected {
            cx.theme().border
        } else {
            cx.theme().border.opacity(0.6)
        };

        let close_button_visible = self.selected;
        let tab_id = self.id.clone();
        
        let side_element_width = px(20.);

        div()
            .id(self.id)
            .group("tab")
            .flex()
            .items_center()
            .h(px(32.))
            .min_w(px(80.))
            .max_w(px(200.))
            .px_2()
            .bg(bg)
            .text_color(fg)
            .text_sm()
            .cursor_pointer()
            .rounded_t_md()
            .border_1()
            .border_b_0()
            .border_color(border_color)
            .when(!self.selected, |this| {
                this.hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            })
            .when_some(self.on_click.clone(), |this, on_click| {
                this.on_click(move |ev, window, cx| on_click(ev, window, cx))
            })
            .when_some(self.on_middle_click.clone(), |this, on_middle| {
                this.on_mouse_down(MouseButton::Middle, move |ev, window, cx| {
                    on_middle(ev, window, cx)
                })
            })
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .gap_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .w(side_element_width)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(self.dirty, |this| {
                                this.child(
                                    div()
                                        .size(px(8.))
                                        .rounded_full()
                                        .bg(cx.theme().foreground),
                                )
                            }),
                    )
                    .child(
                        /* Center: title */
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_center()
                            .child(self.title),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("{}-close", tab_id).into()))
                            .w(side_element_width)
                            .h(px(20.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .when(!close_button_visible, |this| {
                                this.invisible().group_hover("tab", |s| s.visible())
                            })
                            .hover(|s| s.bg(cx.theme().muted))
                            .when_some(self.on_close.clone(), |this, on_close| {
                                this.on_click(move |ev, window, cx| {
                                    cx.stop_propagation();
                                    on_close(ev, window, cx);
                                })
                            })
                            .child(Icon::new(IconName::Close).size_3()),
                    ),
            )
    }
}