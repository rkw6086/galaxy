//! Compact links to existing child conversations in launch and follow-up output.

use galaxyui::elements::{
    ChildAnchor, ConstrainedBox, Container, CrossAxisAlignment, Empty, Flex, Hoverable,
    MainAxisSize, MouseStateHandle, ParentAnchor, ParentElement, Text,
};
use galaxyui::platform::Cursor;
use galaxyui::{AppContext, Element, EntityId, SingletonEntity};
use pathfinder_geometry::vector::vec2f;

use super::orchestration_conversation_links::{
    dispatch_focus_or_open_child_agent_pane, is_conversation_open_in_other_visible_view,
};
use super::orchestration_pill_bar::render_static_agent_pill;
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus, StatusColorStyle};
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;

#[derive(Debug, Clone)]
pub struct SubagentLinkState {
    pub conversation_id: AIConversationId,
    mouse_state: MouseStateHandle,
}

impl SubagentLinkState {
    pub fn new(conversation_id: AIConversationId) -> Self {
        Self {
            conversation_id,
            mouse_state: MouseStateHandle::default(),
        }
    }
}

pub fn render_subagent_link(
    state: &SubagentLinkState,
    self_terminal_view_id: EntityId,
    app: &AppContext,
) -> Box<dyn Element> {
    let Some(conversation) =
        BlocklistAIHistoryModel::as_ref(app).conversation(&state.conversation_id)
    else {
        return Empty::new().finish();
    };
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let name = conversation
        .agent_name()
        .filter(|name| !name.is_empty())
        .unwrap_or("Agent");
    let status = conversation.status();
    let status_text = match status {
        ConversationStatus::InProgress => "Working…",
        ConversationStatus::Success => "Complete",
        ConversationStatus::Error => "Error",
        ConversationStatus::TransientError => "Retrying…",
        ConversationStatus::Cancelled => "Cancelled",
        ConversationStatus::Blocked { .. } => "Blocked",
        ConversationStatus::WaitingForEvents => "Waiting…",
    };
    let (icon, color) = status.status_icon_and_color(theme, StatusColorStyle::Standard);
    let row = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.)
        .with_child(render_static_agent_pill(name, app))
        .with_child(
            ConstrainedBox::new(icon.to_warpui_icon(color.into()).finish())
                .with_width(12.)
                .with_height(12.)
                .finish(),
        )
        .with_child(
            Text::new(status_text.to_string(), appearance.ui_font_family(), 12.)
                .with_color(blended_colors::text_disabled(theme, theme.surface_2()))
                .finish(),
        )
        .finish();
    let conversation_id = state.conversation_id;
    let tooltip = if is_conversation_open_in_other_visible_view(
        conversation_id,
        self_terminal_view_id,
        app,
    ) {
        format!("Focus {name}'s pane")
    } else {
        format!("Open {name} in a new pane")
    };
    let link = Hoverable::new(state.mouse_state.clone(), move |_| row)
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, app, _| {
            dispatch_focus_or_open_child_agent_pane(
                conversation_id,
                self_terminal_view_id,
                ctx,
                app,
            );
        })
        .finish();
    Container::new(appearance.ui_builder().overlay_tool_tip_on_element(
        tooltip,
        state.mouse_state.clone(),
        link,
        ParentAnchor::TopLeft,
        ChildAnchor::BottomLeft,
        vec2f(0., -4.),
    ))
    .with_vertical_padding(4.)
    .finish()
}
