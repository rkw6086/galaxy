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
use lsp::{CompletionItemData, CompletionKind, CompletionResult, CompletionTrigger};
use pathfinder_geometry::vector::Vector2F;
use string_offset::CharOffset;
use vec1::Vec1;

use super::local_code_editor::LocalCodeEditorView;

pub const COMPLETION_DEBOUNCE_PERIOD: Duration = Duration::from_millis(50);
const COMPLETION_MENU_MAX_HEIGHT: f32 = 220.;
const COMPLETION_MENU_WIDTH: f32 = 340.;
const MAX_VISIBLE_ITEMS: usize = 10;

pub(super) enum CompletionState {
    Idle,
    Requesting { abort_handle: AbortHandle },
    Showing {
        items: Vec<CompletionItemData>,
        filtered_indices: Vec<usize>,
        selected_index: usize,
        trigger_offset: CharOffset,
    },
}

impl Default for CompletionState {
    fn default() -> Self {
        Self::Idle
    }
}

impl CompletionState {
    pub fn is_showing(&self) -> bool {
        matches!(self, Self::Showing { .. })
    }

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

    pub fn move_selection(&mut self, delta: i32) {
        if let Self::Showing {
            filtered_indices,
            selected_index,
            ..
        } = self
        {
            if filtered_indices.is_empty() {
                return;
            }
            let len = filtered_indices.len() as i32;
            let new_idx = (*selected_index as i32 + delta).rem_euclid(len);
            *selected_index = new_idx as usize;
        }
    }

    pub fn filter(&mut self, query: &str) {
        if let Self::Showing {
            items,
            filtered_indices,
            selected_index,
            ..
        } = self
        {
            if query.is_empty() {
                *filtered_indices = (0..items.len()).collect();
            } else {
                let query_lower = query.to_lowercase();
                *filtered_indices = items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        let filter_text = item.effective_filter_text().to_lowercase();
                        fuzzy_match(&filter_text, &query_lower)
                    })
                    .map(|(idx, _)| idx)
                    .collect();
            }
            if *selected_index >= filtered_indices.len() {
                *selected_index = 0;
            }
        }
    }
}

fn fuzzy_match(target: &str, query: &str) -> bool {
    let mut target_chars = target.chars();
    for query_char in query.chars() {
        loop {
            match target_chars.next() {
                Some(tc) if tc == query_char => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

impl LocalCodeEditorView {
    pub(super) fn is_completion_enabled() -> bool {
        FeatureFlag::LspCompletion.is_enabled()
    }

    pub(super) fn sync_completion_intercept(&self, ctx: &mut ViewContext<Self>) {
        let should_intercept = self.completion_state.is_showing();
        self.editor.update(ctx, |editor, _ctx| {
            editor.completion_intercept_keys = should_intercept;
        });
    }

    pub(super) fn on_content_changed_for_completion(&mut self, ctx: &mut ViewContext<Self>) {
        if !Self::is_completion_enabled() {
            return;
        }

        if self.lsp_server.is_none() {
            return;
        }

        let cursor_offset = self.editor().as_ref(ctx).cursor_head_offset(ctx);
        if cursor_offset == CharOffset::from(0) {
            return;
        }

        let char_before = self
            .editor()
            .as_ref(ctx)
            .char_at(cursor_offset - CharOffset::from(1), ctx);

        let trigger = match char_before {
            Some('.') => Some(CompletionTrigger::TriggerCharacter(".".into())),
            Some(':') => {
                if cursor_offset >= CharOffset::from(2) {
                    let prev_char = self
                        .editor()
                        .as_ref(ctx)
                        .char_at(cursor_offset - CharOffset::from(2), ctx);
                    if prev_char == Some(':') {
                        Some(CompletionTrigger::TriggerCharacter("::".into()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Some(c) if c.is_alphanumeric() || c == '_' => {
                let _ = self.completion_debounce_tx.try_send(cursor_offset);
                return;
            }
            _ => {
                if self.completion_state.dismiss() {
                    self.sync_completion_intercept(ctx);
                    ctx.notify();
                }
                return;
            }
        };

        if let Some(trigger) = trigger {
            self.request_completion(cursor_offset, trigger, ctx);
        }
    }

    pub(super) fn request_completion(
        &mut self,
        trigger_offset: CharOffset,
        trigger: CompletionTrigger,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::is_completion_enabled() {
            return;
        }

        let Some(file_path) = self.file_path() else {
            return;
        };

        let Some(lsp_server) = &self.lsp_server else {
            return;
        };

        let lsp_position = self
            .editor()
            .as_ref(ctx)
            .offset_to_lsp_position(trigger_offset, ctx);

        let future = match lsp_server
            .as_ref(ctx)
            .completion(file_path.to_path_buf(), lsp_position, trigger)
        {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to call lsp.completion: {e}");
                return;
            }
        };

        self.completion_state.dismiss();

        let abort_handle = ctx
            .spawn(future, move |me, result, ctx| {
                me.handle_completion_response(result, trigger_offset, ctx);
            })
            .abort_handle();

        self.completion_state = CompletionState::Requesting { abort_handle };
    }

    pub(super) fn request_completion_debounced(
        &mut self,
        offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
        if let CompletionState::Showing { trigger_offset, .. } = &self.completion_state {
            let query = self.get_completion_filter_query(*trigger_offset, ctx);
            self.completion_state.filter(&query);
            ctx.notify();
            return;
        }

        self.request_completion(offset, CompletionTrigger::Invoked, ctx);
    }

    fn handle_completion_response(
        &mut self,
        result: anyhow::Result<Option<CompletionResult>>,
        trigger_offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
        let completion_result = match result {
            Ok(Some(result)) if !result.items.is_empty() => result,
            _ => {
                self.completion_state = CompletionState::Idle;
                self.sync_completion_intercept(ctx);
                ctx.notify();
                return;
            }
        };

        let filtered_indices: Vec<usize> = (0..completion_result.items.len()).collect();

        self.completion_state = CompletionState::Showing {
            items: completion_result.items,
            filtered_indices,
            selected_index: 0,
            trigger_offset,
        };

        let query = self.get_completion_filter_query(trigger_offset, ctx);
        self.completion_state.filter(&query);

        self.sync_completion_intercept(ctx);
        ctx.notify();
    }

    fn get_completion_filter_query(
        &self,
        trigger_offset: CharOffset,
        ctx: &ViewContext<Self>,
    ) -> String {
        let editor = self.editor().as_ref(ctx);
        let cursor_offset = editor.cursor_head_offset(ctx);
        if cursor_offset <= trigger_offset {
            return String::new();
        }
        editor
            .buffer_text_in_range(trigger_offset..cursor_offset, ctx)
            .unwrap_or_default()
    }

    pub(super) fn confirm_completion(&mut self, ctx: &mut ViewContext<Self>) -> bool {
        let (insert_text, text_edit_range, trigger_offset) = match &self.completion_state {
            CompletionState::Showing {
                items,
                filtered_indices,
                selected_index,
                trigger_offset,
                ..
            } => {
                let Some(&idx) = filtered_indices.get(*selected_index) else {
                    return false;
                };
                let item = &items[idx];
                (
                    item.insert_text.clone(),
                    item.text_edit_range.clone(),
                    *trigger_offset,
                )
            }
            _ => return false,
        };

        self.completion_state = CompletionState::Idle;
        self.sync_completion_intercept(ctx);

        self.editor.update(ctx, |editor, ctx| {
            let edit_range = if let Some(range) = text_edit_range {
                let start = editor.lsp_location_to_offset(&range.start, ctx);
                let end = editor.lsp_location_to_offset(&range.end, ctx);
                start..end
            } else {
                let cursor = editor.cursor_head_offset(ctx);
                trigger_offset..cursor
            };

            if let Ok(edits) = Vec1::try_from_vec(vec![(insert_text, edit_range)]) {
                editor.apply_edits(edits, ctx);
            }
        });

        ctx.notify();
        true
    }

    pub(super) fn render_completion_menu(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        let CompletionState::Showing {
            items,
            filtered_indices,
            selected_index,
            ..
        } = &self.completion_state
        else {
            return None;
        };

        if filtered_indices.is_empty() {
            return None;
        }

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let visible_count = filtered_indices.len().min(MAX_VISIBLE_ITEMS);

        let mut content_column = Flex::column();

        for (display_idx, &item_idx) in
            filtered_indices.iter().enumerate().take(visible_count)
        {
            let item = &items[item_idx];
            let is_selected = display_idx == *selected_index;
            content_column.add_child(render_completion_item(item, is_selected, appearance));
        }

        let constrained_content = ConstrainedBox::new(content_column.finish())
            .with_width(COMPLETION_MENU_WIDTH)
            .with_max_height(COMPLETION_MENU_MAX_HEIGHT)
            .finish();

        let menu = Container::new(constrained_content)
            .with_background(theme.background())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_border(Border::all(1.).with_border_fill(internal_colors::neutral_4(theme)))
            .finish();

        Some(menu)
    }

    pub(super) fn completion_menu_positioning(
        &self,
        app: &AppContext,
    ) -> Option<OffsetPositioning> {
        let trigger_offset = match &self.completion_state {
            CompletionState::Showing { trigger_offset, .. } => *trigger_offset,
            _ => return None,
        };

        let bounds = self
            .editor()
            .as_ref(app)
            .character_bounds_in_viewport(trigger_offset, app)?;

        Some(OffsetPositioning::offset_from_parent(
            Vector2F::new(bounds.origin_x(), bounds.max_y()),
            ParentOffsetBounds::ParentByPosition,
            ParentAnchor::TopLeft,
            ChildAnchor::TopLeft,
        ))
    }
}

fn render_completion_item(
    item: &CompletionItemData,
    is_selected: bool,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();

    let icon_text: &'static str = match item.kind {
        Some(CompletionKind::Function) | Some(CompletionKind::Method) => "fn",
        Some(CompletionKind::Variable) => "var",
        Some(CompletionKind::Field) | Some(CompletionKind::Property) => "fld",
        Some(CompletionKind::Class) | Some(CompletionKind::Struct) => "str",
        Some(CompletionKind::Interface) => "ifc",
        Some(CompletionKind::Module) => "mod",
        Some(CompletionKind::Enum) => "enm",
        Some(CompletionKind::EnumMember) => "emb",
        Some(CompletionKind::Constant) => "cst",
        Some(CompletionKind::Keyword) => "kw",
        Some(CompletionKind::Snippet) => "snp",
        Some(CompletionKind::TypeParameter) => "typ",
        _ => "  ",
    };

    let label = item.label.clone();
    let detail = item.detail.clone();

    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min);

    row.add_child(
        Container::new(
            Text::new_inline(
                icon_text,
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
                label,
                appearance.monospace_font_family(),
                appearance.monospace_font_size(),
            )
            .with_color(theme.active_ui_text_color().into())
            .finish(),
        )
        .finish(),
    );

    if let Some(detail) = detail {
        row.add_child(
            Container::new(
                Shrinkable::new(
                    2.,
                    Text::new(
                        detail,
                        appearance.monospace_font_family(),
                        appearance.monospace_font_size() * 0.85,
                    )
                    .with_color(theme.disabled_ui_text_color().into())
                    .finish(),
                )
                .finish(),
            )
            .with_padding_left(8.)
            .finish(),
        );
    }

    let mut container = Container::new(row.finish())
        .with_vertical_padding(3.)
        .with_horizontal_padding(4.);

    if is_selected {
        container = container.with_background(internal_colors::neutral_2(theme));
    } else {
        container = container.with_background(theme.background());
    }

    container.finish()
}
