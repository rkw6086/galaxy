//! Inline subagent panel rendered within the parent agent's chat flow.
//!
//! Shows a collapsible panel with the subagent's status, a mini-transcript of
//! recent messages, and controls to expand inline or open the full child view.

use galaxyui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Empty, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius, Shrinkable, Text,
};
use galaxyui::platform::Cursor;
use galaxyui::ui_components::components::UiComponent;
use galaxyui::{AppContext, EntityId, SingletonEntity};
use pathfinder_color::ColorU;
use warp_multi_agent_api as api;

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus, StatusColorStyle};
use crate::ai::agent::AIAgentActionId;
use crate::ai::blocklist::agent_view::orchestration_conversation_links::dispatch_focus_or_open_child_agent_pane;
use crate::ai::blocklist::block::AIBlockAction;
use crate::ai::blocklist::inline_action::inline_action_header::{
    ICON_MARGIN, INLINE_ACTION_HEADER_VERTICAL_PADDING, INLINE_ACTION_HORIZONTAL_PADDING,
};
use crate::ai::blocklist::inline_action::inline_action_icons::icon_size;
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button;
use crate::ui_components::icons::Icon;

const MINI_TRANSCRIPT_MAX_LINES: usize = 8;
const MINI_TRANSCRIPT_MAX_CHARS: usize = 120;
const COMPLETION_SUMMARY_MAX_CHARS: usize = 300;
const PANEL_MAX_HEIGHT: f32 = 200.;
const PANEL_CORNER_RADIUS: f32 = 8.;

/// State for a single subagent inline panel instance.
#[derive(Debug, Clone)]
pub struct SubagentPanelState {
    pub conversation_id: AIConversationId,
    pub is_expanded: bool,
    pub header_mouse_state: MouseStateHandle,
    pub open_mouse_state: MouseStateHandle,
}

impl SubagentPanelState {
    pub fn new(conversation_id: AIConversationId) -> Self {
        Self {
            conversation_id,
            // The panel exists to expose the child agent's live conversation.
            // Start expanded so its responses are visible without another click.
            is_expanded: true,
            header_mouse_state: MouseStateHandle::default(),
            open_mouse_state: MouseStateHandle::default(),
        }
    }
}

/// Renders the inline subagent panel for a child conversation.
pub fn render_subagent_inline_panel(
    state: &SubagentPanelState,
    action_id: &AIAgentActionId,
    self_terminal_view_id: EntityId,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    let history_model = BlocklistAIHistoryModel::as_ref(app);
    let Some(conversation) = history_model.conversation(&state.conversation_id) else {
        return Empty::new().finish();
    };

    let status = conversation.status().clone();
    let agent_name = conversation.agent_name().unwrap_or("Subagent").to_string();

    let panel_bg = blended_colors::neutral_2(theme);

    let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

    // Header — always visible, click to toggle expand/collapse
    let header_mouse_state = state.header_mouse_state.clone();
    let open_mouse_state = state.open_mouse_state.clone();
    let ui_builder = appearance.ui_builder().clone();
    let toggle_action_id = action_id.clone();
    let header_status = status.clone();
    let header_expanded = state.is_expanded;
    let toggle = Hoverable::new(header_mouse_state, move |_mouse_state| {
        render_panel_header(&agent_name, &header_status, header_expanded, panel_bg, app)
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(AIBlockAction::ToggleSubagentPanel {
            action_id: toggle_action_id.clone(),
        });
    })
    .finish();
    let child_conversation_id = state.conversation_id;
    let open = icon_button(appearance, Icon::LinkExternal, false, open_mouse_state)
        .with_tooltip(move || {
            ui_builder
                .tool_tip("Open child conversation".to_string())
                .build()
                .finish()
        })
        .build()
        .on_click(move |ctx, app, _| {
            dispatch_focus_or_open_child_agent_pane(
                child_conversation_id,
                self_terminal_view_id,
                ctx,
                app,
            );
        })
        .finish();
    column.add_child(
        Flex::row()
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Shrinkable::new(1., toggle).finish())
            .with_child(
                Container::new(open)
                    .with_padding_left(4.)
                    .with_padding_right(INLINE_ACTION_HORIZONTAL_PADDING)
                    .finish(),
            )
            .finish(),
    );

    // Body (mini-transcript) — only when expanded
    if state.is_expanded {
        let transcript_lines = collect_mini_transcript(&state.conversation_id, app);
        if !transcript_lines.is_empty() {
            column.add_child(render_mini_transcript(&transcript_lines, panel_bg, app));
        }
    }

    // Footer — show summary when complete
    if status.is_done() {
        if let Some(summary) = get_completion_summary(&state.conversation_id, app) {
            column.add_child(render_summary_footer(&summary, panel_bg, app));
        }
    }

    Container::new(column.finish())
        .with_background_color(panel_bg)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PANEL_CORNER_RADIUS)))
        .with_margin_top(4.)
        .with_margin_bottom(4.)
        .finish()
}

fn render_panel_header(
    agent_name: &str,
    status: &ConversationStatus,
    is_expanded: bool,
    _background: ColorU,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let font_family = appearance.ui_font_family();
    let font_size = appearance.monospace_font_size();
    let surface = theme.surface_2();

    let mut header_row = Flex::row()
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center);

    // Left: status icon + agent name + status text
    let mut left_side = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);

    let (icon, icon_color) = status.status_icon_and_color(theme, StatusColorStyle::Standard);
    let status_icon_element =
        ConstrainedBox::new(galaxyui::elements::Icon::new(icon.into(), icon_color).finish())
            .with_width(icon_size(app))
            .with_height(icon_size(app))
            .finish();

    left_side.add_child(
        Container::new(status_icon_element)
            .with_margin_right(ICON_MARGIN)
            .finish(),
    );

    let name_color: ColorU = theme.main_text_color(surface).into();
    left_side.add_child(
        Text::new_inline(agent_name.to_string(), font_family, font_size)
            .with_color(name_color)
            .finish(),
    );

    let status_text = match status {
        ConversationStatus::InProgress => "Working...",
        ConversationStatus::Success => "Complete",
        ConversationStatus::Error => "Error",
        ConversationStatus::TransientError => "Retrying...",
        ConversationStatus::Cancelled => "Cancelled",
        ConversationStatus::Blocked { .. } => "Blocked",
        ConversationStatus::WaitingForEvents => "Waiting...",
    };
    let status_text_color = blended_colors::text_disabled(theme, surface);
    left_side.add_child(
        Container::new(
            Text::new_inline(status_text.to_string(), font_family, font_size)
                .with_color(status_text_color)
                .finish(),
        )
        .with_margin_left(8.)
        .finish(),
    );

    header_row.add_child(Shrinkable::new(1.0, left_side.finish()).finish());

    // Right: collapse/expand chevron
    let mut right_side = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);

    let chevron_icon = if is_expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let chevron_color = blended_colors::text_disabled(theme, surface);
    let chevron = ConstrainedBox::new(
        galaxyui::elements::Icon::new(chevron_icon.into(), chevron_color).finish(),
    )
    .with_width(icon_size(app))
    .with_height(icon_size(app))
    .finish();
    right_side.add_child(Container::new(chevron).with_margin_right(4.).finish());

    header_row.add_child(right_side.finish());

    Container::new(header_row.finish())
        .with_padding_left(INLINE_ACTION_HORIZONTAL_PADDING)
        .with_padding_right(INLINE_ACTION_HORIZONTAL_PADDING)
        .with_padding_top(INLINE_ACTION_HEADER_VERTICAL_PADDING)
        .with_padding_bottom(INLINE_ACTION_HEADER_VERTICAL_PADDING)
        .finish()
}

fn collect_mini_transcript(conversation_id: &AIConversationId, app: &AppContext) -> Vec<String> {
    let history_model = BlocklistAIHistoryModel::as_ref(app);
    let Some(conversation) = history_model.conversation(conversation_id) else {
        return vec![];
    };

    let messages = conversation.all_linearized_messages();
    collect_visible_transcript(&messages, MINI_TRANSCRIPT_MAX_LINES)
}

fn collect_visible_transcript(messages: &[&api::Message], max_lines: usize) -> Vec<String> {
    let mut lines = messages
        .iter()
        .rev()
        // Filter first, then apply the visible-line limit. A tool-heavy turn can
        // contain many internal messages between user/agent chat messages.
        .filter_map(|message| extract_message_text(message))
        .take(max_lines)
        .map(|text| truncate_with_ellipsis(&text, MINI_TRANSCRIPT_MAX_CHARS))
        .collect::<Vec<_>>();
    lines.reverse();
    lines
}

fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_none() {
        return prefix;
    }

    let visible_prefix_chars = max_chars.saturating_sub(3);
    let mut truncated = prefix
        .chars()
        .take(visible_prefix_chars)
        .collect::<String>();
    truncated.push_str(&".".repeat(max_chars.min(3)));
    truncated
}

fn extract_message_text(msg: &api::Message) -> Option<String> {
    let message_content = msg.message.as_ref()?;
    match message_content {
        api::message::Message::AgentOutput(_) => extract_agent_output_text(msg),
        api::message::Message::UserQuery(query) => {
            if query.query.is_empty() {
                None
            } else {
                Some(query.query.clone())
            }
        }
        _ => None,
    }
}

fn extract_agent_output_text(msg: &api::Message) -> Option<String> {
    let api::message::Message::AgentOutput(output) = msg.message.as_ref()? else {
        return None;
    };
    (!output.text.is_empty()).then(|| output.text.clone())
}

fn render_mini_transcript(
    lines: &[String],
    background: ColorU,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let text_color = blended_colors::text_disabled(theme, background);
    let font_family = appearance.ui_font_family();
    let font_size = appearance.monospace_font_size() - 1.;

    let mut column = Flex::column();
    for line in lines {
        let prefixed = format!("> {line}");
        column.add_child(
            Text::new_inline(prefixed, font_family, font_size)
                .with_color(text_color)
                .finish(),
        );
    }

    ConstrainedBox::new(
        Container::new(column.finish())
            .with_padding_left(INLINE_ACTION_HORIZONTAL_PADDING)
            .with_padding_right(INLINE_ACTION_HORIZONTAL_PADDING)
            .with_padding_top(4.)
            .with_padding_bottom(4.)
            .finish(),
    )
    .with_max_height(PANEL_MAX_HEIGHT)
    .finish()
}

fn get_completion_summary(conversation_id: &AIConversationId, app: &AppContext) -> Option<String> {
    let history_model = BlocklistAIHistoryModel::as_ref(app);
    let conversation = history_model.conversation(conversation_id)?;

    if !conversation.status().is_done() {
        return None;
    }

    completion_summary_from_messages(&conversation.all_linearized_messages())
}

fn completion_summary_from_messages(messages: &[&api::Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        extract_agent_output_text(message)
            .map(|text| truncate_with_ellipsis(&text, COMPLETION_SUMMARY_MAX_CHARS))
    })
}

fn render_summary_footer(summary: &str, _background: ColorU, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let surface = theme.surface_2();
    let text_color: ColorU = theme.main_text_color(surface).into();
    let label_color = blended_colors::text_disabled(theme, surface);
    let font_family = appearance.ui_font_family();
    let font_size = appearance.monospace_font_size();

    let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Start);
    row.add_child(
        Text::new_inline("Summary: ".to_string(), font_family, font_size)
            .with_color(label_color)
            .finish(),
    );
    row.add_child(
        Shrinkable::new(
            1.0,
            Text::new_inline(summary.to_string(), font_family, font_size)
                .with_color(text_color)
                .finish(),
        )
        .finish(),
    );

    Container::new(row.finish())
        .with_padding_left(INLINE_ACTION_HORIZONTAL_PADDING)
        .with_padding_right(INLINE_ACTION_HORIZONTAL_PADDING)
        .with_padding_top(6.)
        .with_padding_bottom(6.)
        .finish()
}

#[cfg(test)]
#[path = "subagent_inline_panel_tests.rs"]
mod tests;
