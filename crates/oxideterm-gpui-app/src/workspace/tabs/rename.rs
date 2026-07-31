use super::*;
use oxideterm_gpui_ui::text_input::{TextInputView, text_input};
use oxideterm_gpui_ui::modal::{dismissible_dialog_backdrop, modal_container, modal_header, modal_body, modal_footer};

impl WorkspaceApp {
    /// Handle keyboard input for the tab rename dialog.
    pub(in crate::workspace) fn handle_tab_rename_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((_, draft)) = &mut self.tab_rename_dialog else {
            return false;
        };
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || modifiers.control {
            return true;
        }
        match key {
            "enter" => {
                self.confirm_tab_rename(cx);
                true
            }
            "escape" => {
                self.cancel_tab_rename(cx);
                true
            }
            "backspace" => {
                draft.pop();
                cx.notify();
                true
            }
            "tab" | "arrowleft" | "arrowright" | "arrowup" | "arrowdown"
            | "home" | "end" | "delete" => true,
            _ => {
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        draft.push_str(text);
                        cx.notify();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Open the tab rename dialog.
    pub(in crate::workspace) fn begin_tab_rename(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_by_id(tab_id) else {
            return;
        };
        let draft = self.tab_display_title(tab);
        self.tab_rename_dialog = Some((tab_id, draft));
        cx.notify();
    }

    pub(in crate::workspace) fn cancel_tab_rename(&mut self, cx: &mut Context<Self>) {
        if self.tab_rename_dialog.take().is_some() {
            cx.notify();
        }
    }

    pub(in crate::workspace) fn confirm_tab_rename(&mut self, cx: &mut Context<Self>) {
        let Some((tab_id, draft)) = self.tab_rename_dialog.take() else {
            return;
        };
        let trimmed = draft.trim();
        let new_title = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        if let Some(tab) = self.tab_mut_by_id(tab_id) {
            tab.set_custom_title(new_title);
        }
        cx.notify();
    }

    pub(in crate::workspace) fn reset_tab_title(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_mut_by_id(tab_id) {
            tab.set_custom_title(None);
            cx.notify();
        }
    }

    /// Render the tab rename dialog as a modal overlay.
    pub(in crate::workspace) fn render_tab_rename_dialog(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (tab_id, draft) = self.tab_rename_dialog.as_ref()?;
        let theme = self.tokens.ui;
        let can_confirm = !draft.trim().is_empty();
        let tab_id = *tab_id;

        let dialog = modal_container(&self.tokens)
            .child(modal_header(
                &self.tokens,
                self.i18n.t("tabbar.rename_tab"),
                String::new(),
            ))
            .child(
                modal_body(&self.tokens)
                    .py(px(12.0))
                    .child(
                        text_input(
                            &self.tokens,
                            TextInputView {
                                value: draft,
                                placeholder: self.i18n.t("tabbar.rename_placeholder"),
                                focused: true,
                                caret_visible: self.new_connection_caret_visible,
                                secret: false,
                                selected_all: false,
                                selected_range: None,
                                marked_text: None,
                            },
                        ),
                    ),
            )
            .child(
                modal_footer(&self.tokens)
                    .child(
                        oxideterm_gpui_ui::button::button(
                            &self.tokens,
                            self.i18n.t("common.actions.cancel"),
                            oxideterm_gpui_ui::button::ButtonTone::Secondary,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, _window, cx| {
                                this.cancel_tab_rename(cx);
                                cx.stop_propagation();
                            }),
                        ),
                    )
                    .child(
                        oxideterm_gpui_ui::button::button(
                            &self.tokens,
                            self.i18n.t("common.actions.confirm"),
                            oxideterm_gpui_ui::button::ButtonTone::Primary,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, _window, cx| {
                                this.confirm_tab_rename(cx);
                                cx.stop_propagation();
                            }),
                        ),
                    ),
            );

        let backdrop = dismissible_dialog_backdrop().on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event, _window, cx| {
                this.cancel_tab_rename(cx);
                cx.stop_propagation();
            }),
        );

        Some(
            backdrop
                .child(dialog)
                .into_any_element(),
        )
    }
}
