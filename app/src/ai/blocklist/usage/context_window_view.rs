use galaxyui::elements::{
    Container, CornerRadius, CrossAxisAlignment, Flex, ParentElement, Radius, Text,
};
use galaxyui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::ai::bedrock::convert::{ContentPart, ConversationMessage, MessageContent, MessageRole};
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;

pub struct ContextWindowView {
    messages: Vec<ConversationMessage>,
}

impl ContextWindowView {
    pub fn new(messages: Vec<ConversationMessage>) -> Self {
        Self { messages }
    }
}

impl View for ContextWindowView {
    fn ui_name() -> &'static str {
        "ContextWindowView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_size = appearance.ui_font_size();
        let text_color = blended_colors::text_main(theme, theme.surface_2());
        let label_color = blended_colors::text_sub(theme, theme.surface_2());

        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(6.0);

        // Header
        let total_chars: usize = self
            .messages
            .iter()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.len(),
                MessageContent::ToolUse { input, .. } => input.to_string().len(),
                MessageContent::ToolResult { content, .. } => content.len(),
                MessageContent::MultiPart(parts) => parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text(t) => t.len(),
                        ContentPart::Image { .. } => 6_400,
                        ContentPart::ToolUse { input, .. } => input.to_string().len(),
                        ContentPart::ToolResult { content, .. } => content.len(),
                    })
                    .sum(),
            })
            .sum();
        let estimated_tokens = total_chars / 4;

        let header_text = format!(
            "Context Window: {} messages, ~{} tokens est.",
            self.messages.len(),
            format_tokens(estimated_tokens as u32),
        );
        column = column.with_child(
            Text::new(header_text, appearance.ui_font_family(), font_size + 1.0)
                .with_color(label_color)
                .finish(),
        );

        // Messages — show full content, no truncation.
        // The parent blocklist handles scrolling.
        for (i, msg) in self.messages.iter().enumerate() {
            let role_str = match msg.role {
                MessageRole::User => "USER",
                MessageRole::Assistant => "ASST",
            };

            // Role header
            let header_line = format!("--- [{}] Message #{} ---", role_str, i);
            column = column.with_child(
                Text::new(header_line, appearance.ui_font_family(), font_size)
                    .with_color(label_color)
                    .soft_wrap(true)
                    .finish(),
            );

            // Full content
            let content_text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::ToolUse {
                    name,
                    tool_use_id,
                    input,
                } => {
                    format!(
                        "[ToolUse] name={}, id={}\ninput={}",
                        name, tool_use_id, input
                    )
                }
                MessageContent::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    format!(
                        "[ToolResult] id={}, error={}\n{}",
                        tool_use_id, is_error, content
                    )
                }
                MessageContent::MultiPart(parts) => {
                    let mut out = String::new();
                    for (pi, p) in parts.iter().enumerate() {
                        match p {
                            ContentPart::Text(t) => {
                                out.push_str(&format!("[Part {} Text] {}\n", pi, t));
                            }
                            ContentPart::Image { data, mime_type } => {
                                out.push_str(&format!(
                                    "[Part {} Image] mime_type={}, bytes={}\n",
                                    pi,
                                    mime_type,
                                    data.len()
                                ));
                            }
                            ContentPart::ToolUse {
                                name,
                                tool_use_id,
                                input,
                            } => {
                                out.push_str(&format!(
                                    "[Part {} ToolUse] name={}, id={}, input={}\n",
                                    pi, name, tool_use_id, input
                                ));
                            }
                            ContentPart::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                out.push_str(&format!(
                                    "[Part {} ToolResult] id={}, error={}\n{}\n",
                                    pi, tool_use_id, is_error, content
                                ));
                            }
                        }
                    }
                    out
                }
            };

            column = column.with_child(
                Text::new(content_text, appearance.ui_font_family(), font_size)
                    .with_color(text_color)
                    .soft_wrap(true)
                    .finish(),
            );
        }

        if self.messages.is_empty() {
            column = column.with_child(
                Text::new(
                    "(empty - no messages in bedrock history)".to_string(),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(label_color)
                .finish(),
            );
        }

        Container::new(column.finish())
            .with_uniform_padding(12.0)
            .with_background(theme.surface_2())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.0)))
            .finish()
    }
}

impl Entity for ContextWindowView {
    type Event = ();
}

impl TypedActionView for ContextWindowView {
    type Action = ();

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}

fn format_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}
