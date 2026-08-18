use galaxy_core::ui::appearance::Appearance;
use warpui::elements::{Border, Container, CornerRadius, ParentElement, Radius};
use warpui::{AppContext, Element, SingletonEntity};

use super::inline_action_icons::icon_size;
use crate::ai::blocklist::block::view_impl::{
    CONTENT_HORIZONTAL_PADDING, CONTENT_ITEM_VERTICAL_MARGIN,
};

/// Renders the shared outer shell used by native and runtime-owned tool panes.
///
/// Callers own execution and body content. This function owns the pane geometry
/// and theme treatment so display-only runtimes cannot drift from native tools.
pub(crate) fn render_tool_pane_shell(
    content: Box<dyn Element>,
    has_highlighted_border: bool,
    spans_conversation_width: bool,
    should_remove_bottom_margin: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = Appearance::as_ref(app).theme();
    let border_color = if has_highlighted_border {
        theme.accent()
    } else {
        theme.surface_2()
    };

    Container::new(content)
        .with_margin_left(if has_highlighted_border || spans_conversation_width {
            CONTENT_HORIZONTAL_PADDING
        } else {
            CONTENT_HORIZONTAL_PADDING + icon_size(app) + 16.
        })
        .with_margin_right(CONTENT_HORIZONTAL_PADDING)
        .with_margin_bottom(if should_remove_bottom_margin {
            0.
        } else {
            CONTENT_ITEM_VERTICAL_MARGIN
        })
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .with_border(Border::all(1.).with_border_fill(border_color))
        .finish()
}
