use std::time::Duration;

use futures::stream::AbortHandle;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::appearance::Appearance;
use galaxy_core::ui::theme::color::internal_colors;
use galaxyui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Flex,
    MainAxisSize, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
    Shrinkable, Text,
};
use galaxyui::{AppContext, Element, SingletonEntity, ViewContext};
use lsp::CodeActionData;
use pathfinder_geometry::vector::Vector2F;
use string_offset::CharOffset;
use vec1::Vec1;

use super::local_code_editor::LocalCodeEditorView;

pub const CODE_ACTIONS_DEBOUNCE_PERIOD: Duration = Duration::from_millis(200);
const CODE_ACTIONS_MENU_WIDTH: f32 = 380.;
const CODE_ACTIONS_MENU_MAX_HEIGHT: f32 = 250.;
const MAX_VISIBLE_ACTIONS: usize = 12;

/// State machine for code actions.
pub(super) enum CodeActionsState {
    /// No code actions active.
    Idle,
    /// Fetching code actions from the LSP.
    Requesting {
        abort_handle: AbortHandle,
        anchor_offset: CharOffset,
    },
    /// Code actions are available (shown via lightbulb or menu).
    Available {
        actions: Vec<CodeActionData>,
        anchor_offset: CharOffset,
        menu_open: bool,
        selected_index: usize,
    },
}

impl Default for CodeActionsState {
    fn default() -> Self {
        Self::Idle
    }
}

impl CodeActionsState {
    pub fn dismiss(&mut self) -> bool {
        if matches!(self, Self::Idle) {
            return false;
        }
        if let Self::Requesting { abort_handle, .. } = self {
            abort_handle.abort();
        }
        *self = Self::Idle;
        true
    }

    pub fn has_actions(&self) -> bool {
        matches!(self, Self::Available { actions, .. } if !actions.is_empty())
    }

    pub fn is_menu_open(&self) -> bool {
        matches!(self, Self::Available { menu_open: true, .. })
    }

    pub fn open_menu(&mut self) {
        if let Self::Available { menu_open, .. } = self {
            *menu_open = true;
        }
    }

    pub fn close_menu(&mut self) {
        if let Self::Available { menu_open, .. } = self {
            *menu_open = false;
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        if let Self::Available {
            actions,
            selected_index,
            ..
        } = self
        {
            if actions.is_empty() {
                return;
            }
            let len = actions.len() as i32;
            let new_idx = (*selected_index as i32 + delta).rem_euclid(len);
            *selected_index = new_idx as usize;
        }
    }
}

impl LocalCodeEditorView {
    pub(super) fn is_code_actions_enabled() -> bool {
        FeatureFlag::LspCodeActions.is_enabled()
    }

    /// Trigger code actions fetch after selection/cursor change (debounced).
    pub(super) fn on_selection_changed_for_code_actions(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::is_code_actions_enabled() {
            return;
        }

        if self.lsp_server.is_none() {
            return;
        }

        let cursor_offset = self.editor().as_ref(ctx).cursor_head_offset(ctx);
        let _ = self.code_actions_debounce_tx.try_send(cursor_offset);
    }

    /// Handle the debounced code actions request.
    pub(super) fn request_code_actions_debounced(
        &mut self,
        offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::is_code_actions_enabled() {
            return;
        }

        let Some(file_path) = self.file_path() else {
            return;
        };

        let Some(lsp_server) = &self.lsp_server else {
            return;
        };

        let editor = self.editor().as_ref(ctx);
        let lsp_position = editor.offset_to_lsp_position(offset, ctx);

        // Build a zero-width range at cursor for code action request
        let lsp_range = lsp_types::Range {
            start: lsp_types::Position {
                line: lsp_position.line as u32,
                character: lsp_position.column as u32,
            },
            end: lsp_types::Position {
                line: lsp_position.line as u32,
                character: lsp_position.column as u32,
            },
        };

        // Include diagnostics at cursor position for context
        let diagnostics_at_cursor: Vec<lsp_types::Diagnostic> = self
            .processed_diagnostics
            .iter()
            .filter(|d| d.start <= offset && offset <= d.end)
            .filter_map(|d| {
                // Convert back to LSP diagnostic (we only need message + severity)
                Some(lsp_types::Diagnostic {
                    range: lsp_range,
                    severity: Some(d.severity),
                    message: d.message.clone(),
                    ..Default::default()
                })
            })
            .collect();

        let future = match lsp_server
            .as_ref(ctx)
            .code_actions(file_path.to_path_buf(), lsp_range, diagnostics_at_cursor)
        {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to call lsp.code_actions: {e}");
                return;
            }
        };

        self.code_actions_state.dismiss();

        let abort_handle = ctx
            .spawn(future, move |me, result, ctx| {
                me.handle_code_actions_response(result, offset, ctx);
            })
            .abort_handle();

        self.code_actions_state = CodeActionsState::Requesting {
            abort_handle,
            anchor_offset: offset,
        };
    }

    fn handle_code_actions_response(
        &mut self,
        result: anyhow::Result<Vec<CodeActionData>>,
        anchor_offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
        let actions = match result {
            Ok(actions) if !actions.is_empty() => actions,
            _ => {
                self.code_actions_state = CodeActionsState::Idle;
                ctx.notify();
                return;
            }
        };

        self.code_actions_state = CodeActionsState::Available {
            actions,
            anchor_offset,
            menu_open: false,
            selected_index: 0,
        };
        ctx.notify();
    }

    /// Open the code actions menu (triggered by Cmd+. or clicking the lightbulb).
    pub(super) fn open_code_actions_menu(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::is_code_actions_enabled() {
            return;
        }

        match &self.code_actions_state {
            CodeActionsState::Available { actions, .. } if !actions.is_empty() => {
                self.code_actions_state.open_menu();
                ctx.notify();
            }
            _ => {
                // No actions available yet — force a fetch and open when ready
                let cursor_offset = self.editor().as_ref(ctx).cursor_head_offset(ctx);
                self.request_code_actions_debounced(cursor_offset, ctx);
            }
        }
    }

    /// Apply the selected code action.
    pub(super) fn confirm_code_action(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        let (raw_action, anchor_offset) = match &self.code_actions_state {
            CodeActionsState::Available {
                actions,
                selected_index,
                anchor_offset,
                ..
            } => {
                let Some(action) = actions.get(*selected_index) else {
                    return false;
                };
                (action.raw.clone(), *anchor_offset)
            }
            _ => return false,
        };

        self.code_actions_state = CodeActionsState::Idle;

        // Apply the code action based on its type
        match raw_action {
            lsp_types::CodeActionOrCommand::CodeAction(action) => {
                if let Some(edit) = action.edit {
                    self.apply_workspace_edit(edit, ctx);
                }
                // TODO: handle action.command (execute command on server)
            }
            lsp_types::CodeActionOrCommand::Command(_cmd) => {
                // TODO: execute command on LSP server
                log::info!("Code action command execution not yet implemented");
            }
        }

        ctx.notify();
        true
    }

    /// Apply a workspace edit (shared between code actions and rename).
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: lsp_types::WorkspaceEdit,
        ctx: &mut ViewContext<Self>,
    ) {
        let mut edits_for_current_file: Vec<(String, std::ops::Range<CharOffset>)> = Vec::new();

        if let Some(changes) = edit.changes {
            for (_uri, text_edits) in changes {
                // For now, apply all edits assuming they're for the current file.
                // TODO: support multi-file edits by checking URI against open files.
                for text_edit in text_edits {
                    let start = self.editor().as_ref(ctx).lsp_location_to_offset(
                        &lsp::types::Location {
                            line: text_edit.range.start.line as usize,
                            column: text_edit.range.start.character as usize,
                        },
                        ctx,
                    );
                    let end = self.editor().as_ref(ctx).lsp_location_to_offset(
                        &lsp::types::Location {
                            line: text_edit.range.end.line as usize,
                            column: text_edit.range.end.character as usize,
                        },
                        ctx,
                    );
                    edits_for_current_file.push((text_edit.new_text, start..end));
                }
            }
        }

        if edits_for_current_file.is_empty() {
            return;
        }

        // Sort in reverse order to avoid offset shifting
        edits_for_current_file.sort_by(|a, b| b.1.start.cmp(&a.1.start));

        if let Ok(edits) = Vec1::try_from_vec(edits_for_current_file) {
            self.editor.update(ctx, |editor, ctx| {
                editor.apply_edits(edits, ctx);
            });
        }
    }

    /// Render the code actions menu overlay.
    pub(super) fn render_code_actions_menu(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        let CodeActionsState::Available {
            actions,
            menu_open: true,
            selected_index,
            ..
        } = &self.code_actions_state
        else {
            return None;
        };

        if actions.is_empty() {
            return None;
        }

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let visible_count = actions.len().min(MAX_VISIBLE_ACTIONS);

        let mut content_column = Flex::column();

        for (idx, action) in actions.iter().enumerate().take(visible_count) {
            let is_selected = idx == *selected_index;
            content_column.add_child(render_code_action_item(action, is_selected, appearance));
        }

        let constrained_content = ConstrainedBox::new(content_column.finish())
            .with_width(CODE_ACTIONS_MENU_WIDTH)
            .with_max_height(CODE_ACTIONS_MENU_MAX_HEIGHT)
            .finish();

        let menu = Container::new(constrained_content)
            .with_background(theme.background())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_border(Border::all(1.).with_border_fill(internal_colors::neutral_4(theme)))
            .finish();

        Some(menu)
    }

    /// Compute positioning for the code actions menu.
    pub(super) fn code_actions_menu_positioning(
        &self,
        app: &AppContext,
    ) -> Option<OffsetPositioning> {
        let anchor_offset = match &self.code_actions_state {
            CodeActionsState::Available {
                anchor_offset,
                menu_open: true,
                ..
            } => *anchor_offset,
            _ => return None,
        };

        let bounds = self
            .editor()
            .as_ref(app)
            .character_bounds_in_viewport(anchor_offset, app)?;

        Some(OffsetPositioning::offset_from_parent(
            Vector2F::new(bounds.origin_x(), bounds.max_y()),
            ParentOffsetBounds::ParentByPosition,
            ParentAnchor::TopLeft,
            ChildAnchor::TopLeft,
        ))
    }
}

fn render_code_action_item(
    action: &CodeActionData,
    is_selected: bool,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let title = action.title.clone();

    let kind_label: &'static str = match action.kind.as_deref() {
        Some("quickfix") => "fix",
        Some("refactor") => "ref",
        Some("refactor.extract") => "ext",
        Some("refactor.inline") => "inl",
        Some("source.organizeImports") => "imp",
        _ => " ",
    };

    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min);

    row.add_child(
        Container::new(
            Text::new_inline(
                kind_label,
                appearance.monospace_font_family(),
                appearance.monospace_font_size() * 0.85,
            )
            .with_color(theme.disabled_ui_text_color().into())
            .finish(),
        )
        .with_padding_left(4.)
        .with_padding_right(6.)
        .finish(),
    );

    row.add_child(
        Shrinkable::new(
            1.,
            Text::new(
                title,
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_color(theme.active_ui_text_color().into())
            .finish(),
        )
        .finish(),
    );

    if action.is_preferred {
        row.add_child(
            Container::new(
                Text::new_inline(
                    "*",
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(theme.ui_warning_color())
                .finish(),
            )
            .with_padding_left(4.)
            .finish(),
        );
    }

    let mut container = Container::new(row.finish())
        .with_vertical_padding(4.)
        .with_horizontal_padding(6.);

    if is_selected {
        container = container.with_background(internal_colors::neutral_2(theme));
    } else {
        container = container.with_background(theme.background());
    }

    container.finish()
}
