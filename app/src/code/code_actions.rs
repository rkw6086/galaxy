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

use super::local_code_editor::LocalCodeEditorView;

pub const CODE_ACTIONS_DEBOUNCE_PERIOD: Duration = Duration::from_millis(200);
const CODE_ACTIONS_MENU_WIDTH: f32 = 380.;
const CODE_ACTIONS_MENU_MAX_HEIGHT: f32 = 250.;
const MAX_VISIBLE_ACTIONS: usize = 12;

pub(super) enum CodeActionsState {
    Idle,
    Requesting { abort_handle: AbortHandle },
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

    pub fn open_menu(&mut self) {
        if let Self::Available { menu_open, .. } = self {
            *menu_open = true;
        }
    }
}

impl LocalCodeEditorView {
    pub(super) fn is_code_actions_enabled() -> bool {
        FeatureFlag::LspCodeActions.is_enabled()
    }

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

        let diagnostics_at_cursor: Vec<lsp_types::Diagnostic> = self
            .processed_diagnostics
            .iter()
            .filter(|d| d.start <= offset && offset <= d.end)
            .map(|d| lsp_types::Diagnostic {
                range: lsp_range,
                severity: Some(d.severity),
                message: d.message.clone(),
                ..Default::default()
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

        self.code_actions_state = CodeActionsState::Requesting { abort_handle };
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
                let cursor_offset = self.editor().as_ref(ctx).cursor_head_offset(ctx);
                self.request_code_actions_debounced(cursor_offset, ctx);
            }
        }
    }

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
