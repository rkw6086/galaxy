use futures::stream::AbortHandle;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::appearance::Appearance;
use galaxy_core::ui::theme::color::internal_colors;
use galaxyui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Flex,
    MainAxisSize, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Text,
};
use galaxyui::{AppContext, Element, SingletonEntity, ViewContext};
use lsp::{ParameterLabel, SignatureHelpResult, SignatureInfo};
use pathfinder_geometry::vector::Vector2F;
use string_offset::CharOffset;

use super::local_code_editor::LocalCodeEditorView;

const SIGNATURE_HELP_MAX_WIDTH: f32 = 500.;

/// State for signature help display.
pub(super) enum SignatureHelpState {
    None,
    Loading(Option<AbortHandle>),
    Showing {
        result: SignatureHelpResult,
        anchor_offset: CharOffset,
    },
}

impl Default for SignatureHelpState {
    fn default() -> Self {
        Self::None
    }
}

impl SignatureHelpState {
    pub fn clear(&mut self) -> bool {
        if matches!(self, Self::None) {
            return false;
        }
        if let Self::Loading(Some(handle)) = self {
            handle.abort();
        }
        *self = Self::None;
        true
    }
}

impl LocalCodeEditorView {
    pub(super) fn is_signature_help_enabled() -> bool {
        FeatureFlag::LspSignatureHelp.is_enabled()
    }

    /// Check if a trigger character was typed and request signature help.
    pub(super) fn on_content_changed_for_signature_help(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if !Self::is_signature_help_enabled() {
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

        match char_before {
            Some('(') | Some(',') => {
                self.request_signature_help(cursor_offset, ctx);
            }
            Some(')') => {
                if self.signature_help_state.clear() {
                    ctx.notify();
                }
            }
            _ => {}
        }
    }

    fn request_signature_help(
        &mut self,
        trigger_offset: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) {
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
            .signature_help(file_path.to_path_buf(), lsp_position)
        {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to call lsp.signature_help: {e}");
                return;
            }
        };

        let abort_handle = ctx
            .spawn(future, move |me, result, ctx| {
                match result {
                    Ok(Some(sig_help)) if !sig_help.signatures.is_empty() => {
                        me.signature_help_state = SignatureHelpState::Showing {
                            result: sig_help,
                            anchor_offset: trigger_offset,
                        };
                    }
                    _ => {
                        me.signature_help_state = SignatureHelpState::None;
                    }
                }
                ctx.notify();
            })
            .abort_handle();

        self.signature_help_state = SignatureHelpState::Loading(Some(abort_handle));
    }

    /// Render the signature help tooltip.
    pub(super) fn render_signature_help(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        let SignatureHelpState::Showing { result, .. } = &self.signature_help_state else {
            return None;
        };

        let active_sig_idx = result.active_signature.unwrap_or(0);
        let sig = result.signatures.get(active_sig_idx)?;

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let active_param = result.active_parameter.unwrap_or(0);

        // Render the signature label with the active parameter highlighted
        let label_element = render_signature_label(sig, active_param, appearance);

        // Wrap in container with docs if available
        let mut content = Flex::column();
        content.add_child(label_element);

        if let Some(doc) = &sig.documentation {
            content.add_child(
                Container::new(
                    Text::new(
                        doc.clone(),
                        appearance.ui_font_family(),
                        appearance.ui_font_size() * 0.9,
                    )
                    .with_color(theme.disabled_ui_text_color().into())
                    .finish(),
                )
                .with_padding_top(4.)
                .finish(),
            );
        }

        let constrained = ConstrainedBox::new(content.finish())
            .with_max_width(SIGNATURE_HELP_MAX_WIDTH)
            .finish();

        let tooltip = Container::new(constrained)
            .with_horizontal_padding(8.)
            .with_vertical_padding(6.)
            .with_background(theme.background())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_border(Border::all(1.).with_border_fill(internal_colors::neutral_4(theme)))
            .finish();

        Some(tooltip)
    }

    /// Position the signature help tooltip above the cursor.
    pub(super) fn signature_help_positioning(
        &self,
        app: &AppContext,
    ) -> Option<OffsetPositioning> {
        let anchor_offset = match &self.signature_help_state {
            SignatureHelpState::Showing { anchor_offset, .. } => *anchor_offset,
            _ => return None,
        };

        let bounds = self
            .editor()
            .as_ref(app)
            .character_bounds_in_viewport(anchor_offset, app)?;

        // Position above the cursor
        Some(OffsetPositioning::offset_from_parent(
            Vector2F::new(bounds.origin_x(), bounds.origin_y()),
            ParentOffsetBounds::ParentByPosition,
            ParentAnchor::TopLeft,
            ChildAnchor::BottomLeft,
        ))
    }
}

fn render_signature_label(
    sig: &SignatureInfo,
    active_param: usize,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();

    // If we have parameter info, try to highlight the active parameter in the label
    if let Some(param) = sig.parameters.get(active_param) {
        let (before, highlight, after) = match &param.label {
            ParameterLabel::Offsets(start, end) => {
                let start = *start as usize;
                let end = *end as usize;
                if end <= sig.label.len() {
                    (
                        sig.label[..start].to_string(),
                        sig.label[start..end].to_string(),
                        sig.label[end..].to_string(),
                    )
                } else {
                    (sig.label.clone(), String::new(), String::new())
                }
            }
            ParameterLabel::Simple(name) => {
                if let Some(idx) = sig.label.find(name.as_str()) {
                    (
                        sig.label[..idx].to_string(),
                        name.clone(),
                        sig.label[idx + name.len()..].to_string(),
                    )
                } else {
                    (sig.label.clone(), String::new(), String::new())
                }
            }
        };

        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Min);

        if !before.is_empty() {
            row.add_child(
                Text::new(
                    before,
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(theme.active_ui_text_color().into())
                .finish(),
            );
        }

        if !highlight.is_empty() {
            row.add_child(
                Text::new(
                    highlight,
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(theme.ui_warning_color())
                .finish(),
            );
        }

        if !after.is_empty() {
            row.add_child(
                Text::new(
                    after,
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(theme.active_ui_text_color().into())
                .finish(),
            );
        }

        row.finish()
    } else {
        // No parameter info — just show the full label
        Text::new(
            sig.label.clone(),
            appearance.monospace_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(theme.active_ui_text_color().into())
        .finish()
    }
}
