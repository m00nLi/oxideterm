use super::*;
use gpui::{ClipboardItem, Font, TextRun};
use oxideterm_editor_core::utf16::{
    byte_index_for_utf16, next_utf16_boundary, previous_utf16_boundary, replace_utf16,
    utf16_offset_for_byte_index, utf16_slice, word_range_for_utf16_offset,
};
use oxideterm_gpui_ui::modal::{
    dialog_backdrop, dialog_description, dialog_header, dialog_title, modal_body, modal_container,
    modal_footer,
};
use oxideterm_gpui_ui::text_input::{
    TextInputAnchorId, TextInputView, text_input, text_input_anchor_probe,
};
use oxideterm_gpui_ui::typography::tauri_ui_font_family;
use std::ops::Range;

/// Stable anchor id for the tab-rename text input.  Chosen well above the
/// highest WorkspaceImeTarget id (3_000 + n) to avoid collisions.
const TAB_RENAME_ANCHOR_ID: u64 = 4_000;

/// Return the active selection range (sorted start..end), or None if the
/// anchor and cursor are equal (pure caret, no selection).
/// Character classification for word-boundary navigation (WindTerm-style).
/// Alphanumeric and `_` form one class so that `user_name` is a single word.
fn char_word_type(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

/// Move left across one group of same-type characters.
fn previous_word_boundary_local(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let byte_pos = byte_index_for_utf16(text, cursor);
    let prefix = &text[..byte_pos];
    let mut stop_byte = 0;
    let mut start_type: Option<u8> = None;
    for (byte_idx, ch) in prefix.char_indices().rev() {
        let t = char_word_type(ch);
        match start_type {
            None => start_type = Some(t),
            Some(st) if t != st => {
                stop_byte = byte_idx + ch.len_utf8();
                break;
            }
            _ => {}
        }
    }
    utf16_offset_for_byte_index(text, stop_byte)
}

/// Move right across one group of same-type characters.
fn next_word_boundary_local(text: &str, cursor: usize) -> usize {
    let text_len = text.encode_utf16().count();
    if cursor >= text_len {
        return text_len;
    }
    let byte_pos = byte_index_for_utf16(text, cursor);
    let suffix = &text[byte_pos..];
    let mut stop_byte = text.len();
    let mut start_type: Option<u8> = None;
    for (rel_byte, ch) in suffix.char_indices() {
        let t = char_word_type(ch);
        match start_type {
            None => start_type = Some(t),
            Some(st) if t != st => {
                stop_byte = byte_pos + rel_byte;
                break;
            }
            _ => {}
        }
    }
    utf16_offset_for_byte_index(text, stop_byte)
}

fn selection_range(anchor: usize, cursor: usize) -> Option<Range<usize>> {
    if anchor == cursor {
        None
    } else if anchor < cursor {
        Some(anchor..cursor)
    } else {
        Some(cursor..anchor)
    }
}

impl WorkspaceApp {
    /// Handle keyboard input for the tab rename dialog.
    pub(in crate::workspace) fn handle_tab_rename_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((_, draft, anchor, cursor)) = &mut self.tab_rename_dialog else {
            return false;
        };
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        let text_len = draft.encode_utf16().count();
        let sel = selection_range(*anchor, *cursor);

        // --- Clipboard shortcuts ---
        if modifiers.control || modifiers.platform {
            match key {
                "a" => {
                    *anchor = 0;
                    *cursor = text_len;
                    cx.notify();
                    return true;
                }
                "c" => {
                    if let Some(range) = sel {
                        cx.write_to_clipboard(ClipboardItem::new_string(utf16_slice(draft, range)));
                    }
                    return true;
                }
                "x" => {
                    if let Some(range) = sel {
                        let start = range.start;
                        cx.write_to_clipboard(ClipboardItem::new_string(utf16_slice(
                            draft,
                            range.clone(),
                        )));
                        replace_utf16(draft, Some(range), "");
                        *anchor = start;
                        *cursor = start;
                        cx.notify();
                    }
                    return true;
                }
                "v" => {
                   if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                       let text = text.replace(['\n', '\r'], " ");
                       let insert_len = text.encode_utf16().count();
                        let insert_pos = sel.as_ref().map_or(*cursor, |r| r.start);
                        let range = sel.unwrap_or(*cursor..*cursor);
                        replace_utf16(draft, Some(range), &text);
                       *anchor = insert_pos + insert_len;
                       *cursor = *anchor;
                        cx.notify();
                    }
                   return true;
               }
                "backspace" => {
                    let prev = previous_word_boundary_local(draft, *cursor);
                    if prev < *cursor {
                        replace_utf16(draft, Some(prev..*cursor), "");
                        *anchor = prev;
                        *cursor = prev;
                        cx.notify();
                    }
                    return true;
                }
                "delete" => {
                    let next = next_word_boundary_local(draft, *cursor);
                    if next > *cursor {
                        replace_utf16(draft, Some(*cursor..next), "");
                        cx.notify();
                    }
                    return true;
                }
                "left" | "arrowleft" => {
                    *cursor = previous_word_boundary_local(draft, *cursor);
                    *anchor = *cursor;
                    cx.notify();
                    return true;
                }
                "right" | "arrowright" => {
                    *cursor = next_word_boundary_local(draft, *cursor);
                    *anchor = *cursor;
                    cx.notify();
                    return true;
                }
               _ => return true,
            }
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
                if let Some(range) = sel {
                    let start = range.start;
                    replace_utf16(draft, Some(range), "");
                    *anchor = start;
                    *cursor = start;
                } else {
                    let prev = previous_utf16_boundary(draft, *cursor);
                    if prev < *cursor {
                        replace_utf16(draft, Some(prev..*cursor), "");
                        *anchor = prev;
                        *cursor = prev;
                    }
                }
                cx.notify();
                true
            }
            "delete" => {
                if let Some(range) = sel {
                    let start = range.start;
                    replace_utf16(draft, Some(range), "");
                    *anchor = start;
                    *cursor = start;
                } else {
                    let next = next_utf16_boundary(draft, *cursor);
                    if next > *cursor {
                        replace_utf16(draft, Some(*cursor..next), "");
                    }
                }
                cx.notify();
                true
            }
            "left" | "arrowleft" if modifiers.shift => {
                *cursor = previous_utf16_boundary(draft, *cursor);
                cx.notify();
                true
            }
            "right" | "arrowright" if modifiers.shift => {
                *cursor = next_utf16_boundary(draft, *cursor);
                cx.notify();
                true
            }
            "left" | "arrowleft" => {
                // Collapse selection or move left.
                let dest = if let Some(range) = sel {
                    range.start
                } else {
                    previous_utf16_boundary(draft, *cursor)
                };
                *anchor = dest;
                *cursor = dest;
                cx.notify();
                true
            }
            "right" | "arrowright" => {
                let dest = if let Some(range) = sel {
                    range.end
                } else {
                    next_utf16_boundary(draft, *cursor)
                };
                *anchor = dest;
                *cursor = dest;
                cx.notify();
                true
            }
            "up" | "arrowup" | "home" => {
                if modifiers.shift {
                    *cursor = 0;
                } else {
                    *anchor = 0;
                    *cursor = 0;
                }
                cx.notify();
                true
            }
            "down" | "arrowdown" | "end" => {
                if modifiers.shift {
                    *cursor = text_len;
                } else {
                    *anchor = text_len;
                    *cursor = text_len;
                }
                cx.notify();
                true
            }
            "tab" => true,
            _ => {
                if let Some(text) = event.keystroke.key_char.as_deref() {
                   if !text.is_empty() && !text.chars().any(char::is_control) {
                       let insert_pos = sel.as_ref().map_or(*cursor, |r| r.start);
                       let insert_len = text.encode_utf16().count();
                        let range = sel.unwrap_or(*cursor..*cursor);
                        replace_utf16(draft, Some(range), text);
                       *anchor = insert_pos + insert_len;
                       *cursor = *anchor;
                        cx.notify();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Compute the UTF-16 cursor offset for a mouse click inside the rename
    /// text input.  Shapes the draft text with the same UI font/size the
    /// `text_input` component renders so hit-testing matches the painted glyphs.
    fn tab_rename_cursor_for_x(&self, draft: &str, click_x: Pixels, window: &mut Window) -> usize {
        let text_len_utf16 = draft.encode_utf16().count();
        if text_len_utf16 == 0 {
            return 0;
        }
        let bounds = match self.tab_rename_input_bounds {
            Some(b) => b,
            None => return text_len_utf16,
        };
        let padding = self.tokens.metrics.ui_control_padding_x;
        let left = bounds.left() + px(padding);
        let right = bounds.right() - px(padding);
        if click_x <= left {
            return 0;
        }
        if click_x >= right {
            return text_len_utf16;
        }
        let relative_x = click_x - left;

        let family =
            tauri_ui_font_family(&self.settings_store.settings().appearance.ui_font_family);
        let font = Font {
            family,
            ..Default::default()
        };
        let shared = SharedString::from(draft.to_string());
        let run = TextRun {
            len: shared.len(),
            font,
            color: rgb(self.tokens.ui.text).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window.text_system().shape_line(
            shared,
            px(self.tokens.metrics.ui_text_sm),
            &[run],
            None,
        );
        let byte_index = shaped.closest_index_for_x(relative_x);
        utf16_offset_for_byte_index(draft, byte_index)
    }

    /// Open the tab rename dialog.
    pub(in crate::workspace) fn begin_tab_rename(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_by_id(tab_id, cx) else {
            return;
        };
        // Use the bare title (without the gId prefix) so the user only edits
        // the tab name, not the global id portion.
        let draft = tab.display_title().to_string();
        let text_len = draft.encode_utf16().count();
        // Select all text on open (WindTerm behavior): anchor at 0, cursor at
        // end so the user can immediately type to replace or press arrow to
        // edit.
        self.tab_rename_dialog = Some((tab_id, draft, 0, text_len));
        self.tab_rename_dialog_offset = Point::new(px(0.0), px(0.0));
        self.tab_rename_dialog_drag = None;
        self.tab_rename_text_drag = None;
        self.tab_rename_input_bounds = None;
        cx.notify();
    }

    pub(in crate::workspace) fn cancel_tab_rename(&mut self, cx: &mut Context<Self>) {
        if self.tab_rename_dialog.take().is_some() {
            self.tab_rename_dialog_offset = Point::new(px(0.0), px(0.0));
            self.tab_rename_dialog_drag = None;
            self.tab_rename_text_drag = None;
            self.tab_rename_input_bounds = None;
            cx.notify();
        }
    }

    pub(in crate::workspace) fn confirm_tab_rename(&mut self, cx: &mut Context<Self>) {
        let Some((tab_id, draft, _, _)) = self.tab_rename_dialog.take() else {
            return;
        };
        self.tab_rename_dialog_offset = Point::new(px(0.0), px(0.0));
        self.tab_rename_dialog_drag = None;
        self.tab_rename_text_drag = None;
        self.tab_rename_input_bounds = None;
        let trimmed = draft.trim();
        let new_title = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self.tab_host.update(cx, |host, _| {
            if let Some(tab) = host.tab_mut_by_id(tab_id) {
                tab.set_custom_title(new_title);
            }
        });
        cx.notify();
    }

    pub(in crate::workspace) fn reset_tab_title(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        self.tab_host.update(cx, |host, _| {
            if let Some(tab) = host.tab_mut_by_id(tab_id) {
                tab.set_custom_title(None);
            }
        });
        cx.notify();
    }

    /// Render the tab rename dialog as a modal overlay.
    pub(in crate::workspace) fn render_tab_rename_dialog(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_tab_id, draft, anchor, cursor) = self.tab_rename_dialog.as_ref()?;
        let anchor = *anchor;
        let cursor = *cursor;
        let sel = selection_range(anchor, cursor);

        // The text input is wrapped in an anchor probe so mouse-down hit
        // testing can convert pixel positions to character offsets.  The
        // probe runs during layout and stores the input bounds on the app.
        let input = text_input(
            &self.tokens,
            TextInputView {
                value: draft,
                placeholder: self.i18n.t("tabbar.rename_placeholder"),
                focused: true,
                caret_visible: self.input_caret.visible(),
                secret: false,
                selected_all: false,
                selected_range: sel.or(Some(cursor..cursor)),
                marked_text: None,
            },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                let text = this
                    .tab_rename_dialog
                    .as_ref()
                    .map(|(_, draft, _, _)| draft.clone())
                    .unwrap_or_default();
                let idx = this.tab_rename_cursor_for_x(&text, event.position.x, window);

                // click_count: 1 = place caret, 2 = select word, 3+ = select all.
                match event.click_count {
                    2 => {
                        let range = word_range_for_utf16_offset(&text, idx);
                        if let Some((_, _, anchor, cursor)) = this.tab_rename_dialog.as_mut() {
                            *anchor = range.start;
                            *cursor = range.end;
                        }
                    }
                    count if count >= 3 => {
                        let len = text.encode_utf16().count();
                        if let Some((_, _, anchor, cursor)) = this.tab_rename_dialog.as_mut() {
                            *anchor = 0;
                            *cursor = len;
                        }
                    }
                    _ => {
                        // Single click: place caret (or extend if shift held).
                        let new_anchor = if event.modifiers.shift {
                            this.tab_rename_dialog
                                .as_ref()
                                .map(|(_, _, a, _)| *a)
                                .unwrap_or(idx)
                        } else {
                            idx
                        };
                        if let Some((_, _, a, c)) = this.tab_rename_dialog.as_mut() {
                            *a = new_anchor;
                            *c = idx;
                        }
                        this.tab_rename_text_drag = Some(true);
                    }
                }
                cx.notify();
                cx.stop_propagation();
            }),
        );

       let probed_input =
           text_input_anchor_probe(TextInputAnchorId(TAB_RENAME_ANCHOR_ID), input, {
               let entity = cx.entity();
               move |anchor, _window, cx: &mut gpui::App| {
                    let _ = entity.update(cx, |this, cx| {
                       this.tab_rename_input_bounds = Some(anchor.bounds);
                        cx.notify();
                   });
               }
           });

        let dialog = modal_container(&self.tokens)
            .child(
                dialog_header(&self.tokens)
                    .child(dialog_title(&self.tokens, self.i18n.t("tabbar.rename_tab")))
                    .child(dialog_description(&self.tokens, String::new()))
                    .cursor(CursorStyle::OpenHand)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.tab_rename_dialog_drag =
                                Some((event.position, this.tab_rename_dialog_offset));
                            this.tab_rename_text_drag = None;
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(modal_body(&self.tokens).py(px(12.0)).child(probed_input))
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

        // Protected backdrop: outside clicks are swallowed so the dialog only
        // closes via the confirm/cancel buttons.  Mouse-move/up still drive
        // the header drag and text selection drag while the button is held.
        let backdrop = dialog_backdrop()
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                    // --- Header drag ---
                    if let Some((mouse_start, offset_start)) = this.tab_rename_dialog_drag {
                        this.tab_rename_dialog_offset = Point::new(
                            offset_start.x + (event.position.x - mouse_start.x),
                            offset_start.y + (event.position.y - mouse_start.y),
                        );
                        cx.notify();
                        return;
                    }
                    // --- Text selection drag ---
                    if this.tab_rename_text_drag.is_some() && event.dragging() {
                        let text = this
                            .tab_rename_dialog
                            .as_ref()
                            .map(|(_, draft, _, _)| draft.clone())
                            .unwrap_or_default();
                        let idx = this.tab_rename_cursor_for_x(&text, event.position.x, window);
                        if let Some((_, _, _, cursor)) = this.tab_rename_dialog.as_mut() {
                            *cursor = idx;
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    this.tab_rename_dialog_drag = None;
                    this.tab_rename_text_drag = None;
                    cx.stop_propagation();
                }),
            );

        // The drag offset shifts a full-size wrapper that centers the dialog.
        let offset = self.tab_rename_dialog_offset;
        let dialog_wrapper = div()
            .absolute()
            .top(px(f32::from(offset.y)))
            .left(px(f32::from(offset.x)))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(dialog);

        Some(backdrop.child(dialog_wrapper).into_any_element())
    }
}
