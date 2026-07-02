use futures::stream::AbortHandle;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::appearance::Appearance;
use galaxyui::{SingletonEntity, ViewContext, ViewHandle};
use lsp::types::Location;
use string_offset::CharOffset;
use vec1::Vec1;

use super::local_code_editor::LocalCodeEditorView;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};

#[derive(Default)]
pub(super) enum RenameState {
    #[default]
    Idle,
    Preparing {
        abort_handle: AbortHandle,
    },
    InputActive {
        editor: ViewHandle<EditorView>,
        anchor_offset: CharOffset,
    },
    Applying {
        abort_handle: AbortHandle,
    },
}

impl RenameState {
    pub fn dismiss(&mut self) -> bool {
        match self {
            Self::Idle => false,
            Self::Preparing { abort_handle } | Self::Applying { abort_handle } => {
                abort_handle.abort();
                *self = Self::Idle;
                true
            }
            Self::InputActive { .. } => {
                *self = Self::Idle;
                true
            }
        }
    }
}

impl LocalCodeEditorView {
    pub(super) fn is_rename_enabled() -> bool {
        FeatureFlag::LspRename.is_enabled()
    }

    pub(super) fn start_rename(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::is_rename_enabled() {
            return;
        }

        let Some(file_path) = self.file_path() else {
            return;
        };

        let Some(lsp_server) = &self.lsp_server else {
            return;
        };

        let cursor_offset = self.editor().as_ref(ctx).cursor_head_offset(ctx);
        let lsp_position = self
            .editor()
            .as_ref(ctx)
            .offset_to_lsp_position(cursor_offset, ctx);

        let future = match lsp_server
            .as_ref(ctx)
            .prepare_rename(file_path.to_path_buf(), lsp_position)
        {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to call lsp.prepare_rename: {e}");
                return;
            }
        };

        self.rename_state.dismiss();

        let abort_handle = ctx
            .spawn(future, move |me, result, ctx| {
                me.handle_prepare_rename_response(result, cursor_offset, ctx);
            })
            .abort_handle();

        self.rename_state = RenameState::Preparing { abort_handle };
    }

    fn handle_prepare_rename_response(
        &mut self,
        result: anyhow::Result<Option<lsp::PrepareRenameResult>>,
        cursor_offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
        let prepare_result = match result {
            Ok(Some(r)) => r,
            _ => {
                log::info!("Symbol at cursor cannot be renamed");
                self.rename_state = RenameState::Idle;
                return;
            }
        };

        let editor_ref = self.editor().as_ref(ctx);
        let range_start = editor_ref.lsp_location_to_offset(&prepare_result.range.start, ctx);
        let range_end = editor_ref.lsp_location_to_offset(&prepare_result.range.end, ctx);

        let current_name = editor_ref
            .buffer_text_in_range(range_start..range_end, ctx)
            .unwrap_or_default();

        let placeholder = prepare_result.placeholder.unwrap_or(current_name);

        let rename_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(None, appearance),
                    select_all_on_focus: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_buffer_text(&placeholder, ctx);
            editor
        });

        ctx.subscribe_to_view(&rename_editor, |me, _, event, ctx| {
            me.handle_rename_editor_event(event, ctx);
        });

        ctx.focus(&rename_editor);

        self.rename_state = RenameState::InputActive {
            editor: rename_editor,
            anchor_offset: cursor_offset,
        };
        ctx.notify();
    }

    fn handle_rename_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Enter => {
                let new_name = match &self.rename_state {
                    RenameState::InputActive { editor, .. } => editor.as_ref(ctx).buffer_text(ctx),
                    _ => return,
                };
                self.confirm_rename(new_name, ctx);
            }
            EditorEvent::Escape => {
                self.rename_state = RenameState::Idle;
                ctx.focus(self.editor());
                ctx.notify();
            }
            _ => {}
        }
    }

    fn confirm_rename(&mut self, new_name: String, ctx: &mut ViewContext<Self>) {
        if new_name.is_empty() {
            self.rename_state = RenameState::Idle;
            ctx.focus(self.editor());
            ctx.notify();
            return;
        }

        let anchor_offset = match &self.rename_state {
            RenameState::InputActive { anchor_offset, .. } => *anchor_offset,
            _ => return,
        };

        let Some(file_path) = self.file_path() else {
            return;
        };

        let Some(lsp_server) = &self.lsp_server else {
            return;
        };

        let lsp_position = self
            .editor()
            .as_ref(ctx)
            .offset_to_lsp_position(anchor_offset, ctx);

        let future =
            match lsp_server
                .as_ref(ctx)
                .rename(file_path.to_path_buf(), lsp_position, new_name)
            {
                Ok(future) => future,
                Err(e) => {
                    log::warn!("Failed to call lsp.rename: {e}");
                    self.rename_state = RenameState::Idle;
                    ctx.focus(self.editor());
                    ctx.notify();
                    return;
                }
            };

        let abort_handle = ctx
            .spawn(future, move |me, result, ctx| {
                me.handle_rename_response(result, ctx);
            })
            .abort_handle();

        self.rename_state = RenameState::Applying { abort_handle };
    }

    fn handle_rename_response(
        &mut self,
        result: anyhow::Result<Option<lsp::RenameResult>>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.rename_state = RenameState::Idle;
        ctx.focus(self.editor());

        let rename_result = match result {
            Ok(Some(r)) if !r.edits.is_empty() => r,
            Ok(_) => {
                log::info!("Rename returned no edits");
                ctx.notify();
                return;
            }
            Err(e) => {
                log::warn!("Rename failed: {e}");
                ctx.notify();
                return;
            }
        };

        let mut edits_for_current_file: Vec<(String, std::ops::Range<CharOffset>)> = Vec::new();

        for file_edit in &rename_result.edits {
            for text_edit in &file_edit.edits {
                let start = self.editor().as_ref(ctx).lsp_location_to_offset(
                    &Location {
                        line: text_edit.range.start.line,
                        column: text_edit.range.start.column,
                    },
                    ctx,
                );
                let end = self.editor().as_ref(ctx).lsp_location_to_offset(
                    &Location {
                        line: text_edit.range.end.line,
                        column: text_edit.range.end.column,
                    },
                    ctx,
                );
                edits_for_current_file.push((text_edit.text.clone(), start..end));
            }
        }

        if edits_for_current_file.is_empty() {
            ctx.notify();
            return;
        }

        edits_for_current_file.sort_by_key(|b| std::cmp::Reverse(b.1.start));

        if let Ok(edits) = Vec1::try_from_vec(edits_for_current_file) {
            self.editor.update(ctx, |editor, ctx| {
                editor.apply_edits(edits, ctx);
            });
        }

        ctx.notify();
    }
}
