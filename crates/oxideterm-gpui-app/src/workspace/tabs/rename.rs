use super::*;
use oxideterm_gpui_ui::text_input::{TextInputView, text_input};

/// Inline tab-rename state machine.
///
/// Tabs are renamed in place: a right-click menu item or a double-click on the
/// tab strip enters edit mode, the draft is pre-filled with the current display
/// title, and Enter/blur commit while Escape cancels. The custom title is
/// ephemeral for the running session; tabs are not persisted across restart.
impl WorkspaceApp {
    /// Handle a key event while inline rename is active.
    ///
    /// Rename mode owns keyboard focus (per the focus-routing invariants), so
    /// this must run before the terminal/command-bar dispatch chain. Returns
    /// `true` when the key was consumed so the caller stops propagation.
    pub(in crate::workspace) fn handle_tab_rename_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.renaming_tab_id.is_none() {
            return false;
        }
       let key = event.keystroke.key.as_str();
       let modifiers = &event.keystroke.modifiers;
        // While rename is active, consume all keys so nothing leaks to the
        // terminal behind the input. Platform/Ctrl shortcuts (including
        // Cmd/Ctrl+V paste) are swallowed — the inline draft has no paste
        // support, so letting paste reach the shell would be a footgun.
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
                self.rename_input_draft.pop();
                cx.notify();
                true
            }
            // Navigation keys have no effect on the draft but must be
            // consumed so they don't reach the terminal.
            "tab" | "arrowleft" | "arrowright" | "arrowup" | "arrowdown"
            | "home" | "end" | "delete" => true,
            _ => {
                // Use the platform text payload (`key_char`), not the key name,
                // so shifted symbols and non-US layouts insert the bytes the
                // user actually typed (security-critical invariant for inputs).
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        self.rename_input_draft.push_str(text);
                        cx.notify();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Enter inline-rename mode for a tab, pre-filling the draft with the
    /// current display title so the user edits the name they already see.
    pub(in crate::workspace) fn begin_tab_rename(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_by_id(tab_id) else {
            return;
        };
        // Edit the visible title. For a tab that already has a custom title this
        // keeps the custom text; otherwise it seeds the draft with the derived
        // default so the user can tweak it without retyping.
        let draft = self.tab_display_title(tab);
        self.renaming_tab_id = Some(tab_id);
        self.rename_input_draft = draft;
        cx.notify();
    }

    /// Abort inline rename without applying the draft.
    pub(in crate::workspace) fn cancel_tab_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming_tab_id.take().is_some() {
            self.rename_input_draft.clear();
            cx.notify();
        }
    }

    /// Apply the current draft as the tab's custom title and exit rename mode.
    ///
    /// A blank/whitespace-only draft clears the custom title instead of storing
    /// an empty string, so the tab falls back to its derived default name.
   pub(in crate::workspace) fn confirm_tab_rename(&mut self, cx: &mut Context<Self>) {
       let Some(tab_id) = self.renaming_tab_id.take() else {
           return;
       };
        // Pass the raw draft; set_custom_title normalizes (trim, empty→None,
        // equals-title→None) in one place.
        let draft = std::mem::take(&mut self.rename_input_draft);
        let new_title = if draft.trim().is_empty() {
            None
        } else {
            Some(draft)
        };
       if let Some(tab) = self.tab_mut_by_id(tab_id) {
           tab.set_custom_title(new_title);
       }
       cx.notify();
   }

    /// Clear a tab's custom title, restoring the derived default name.
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

    /// Whether the given tab is currently in inline-rename mode.
    pub(in crate::workspace) fn is_renaming_tab(&self, tab_id: TabId) -> bool {
        self.renaming_tab_id == Some(tab_id)
    }

    /// Render the inline rename input for a tab, replacing the static title.
    ///
    /// Uses the shared text-input primitive so focus border, caret, and
    /// typography match every other OxideTerm form field. The draft lives on
    /// `WorkspaceApp` and is mutated by `handle_tab_rename_key`; this view is a
    /// pure projection of that draft.
   pub(in crate::workspace) fn render_tab_rename_input(&self) -> AnyElement {
       let placeholder = self.i18n.t("tabbar.rename_placeholder");
       // Wrap the text_input in a container that stops mouse-down
       // propagation so clicks inside the input don't bubble to the
       // workspace root and trigger a premature commit.
       div()
           .flex_1()
           .min_w(px(0.0))
           .text_size(px(self.tokens.metrics.tab_font_size))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
           .child(text_input(
                &self.tokens,
                TextInputView {
                    value: &self.rename_input_draft,
                    placeholder,
                    focused: true,
                    // Reuse the shared blink state. Rename is the only active
                    // editable field while open, so the shared flag is accurate.
                    caret_visible: self.new_connection_caret_visible,
                    secret: false,
                    selected_all: false,
                    selected_range: None,
                    marked_text: None,
                },
            ))
            .into_any_element()
    }
}
