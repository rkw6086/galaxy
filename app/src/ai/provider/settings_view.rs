use galaxyui::elements::{
    Container, CornerRadius, CrossAxisAlignment, Flex, ParentElement, Radius, Text,
};
use galaxyui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::ai::provider::convert::CachingConfig;
use crate::ai::provider::external_config::ExternalBedrockConfig;
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;

pub struct SettingsView {
    external_config: ExternalBedrockConfig,
    caching_config: CachingConfig,
}

impl SettingsView {
    pub fn new() -> Self {
        let external_config = ExternalBedrockConfig::load();
        let caching_config = CachingConfig::from_external_config(&external_config);
        Self {
            external_config,
            caching_config,
        }
    }
}

impl View for SettingsView {
    fn ui_name() -> &'static str {
        "SettingsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_size = appearance.ui_font_size();
        let text_color = blended_colors::text_main(theme, theme.surface_2());
        let label_color = blended_colors::text_sub(theme, theme.surface_2());

        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(8.0);

        // Header
        column = column.with_child(
            Text::new(
                "Galaxy Bedrock Settings",
                appearance.ui_font_family(),
                font_size + 2.0,
            )
            .with_color(text_color)
            .finish(),
        );

        // Separator
        column = column.with_child(
            Text::new(
                "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
                appearance.ui_font_family(),
                font_size,
            )
            .with_color(label_color)
            .finish(),
        );

        // AWS Configuration
        if let Some(profile) = &self.external_config.profile {
            column = column.with_child(
                Text::new(
                    format!("AWS Profile: {}", profile),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(text_color)
                .finish(),
            );
        }

        if let Some(region) = &self.external_config.region {
            column = column.with_child(
                Text::new(
                    format!("AWS Region: {}", region),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(text_color)
                .finish(),
            );
        }

        // Model Configuration
        column = column.with_child(
            Text::new(
                "\n━━━ Model Selection ━━━",
                appearance.ui_font_family(),
                font_size - 1.0,
            )
            .with_color(label_color)
            .finish(),
        );

        if let Some(model) = &self.external_config.anthropic_model {
            column = column.with_child(
                Text::new(
                    format!("Default Model (ANTHROPIC_MODEL): {}", model),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(text_color)
                .soft_wrap(true)
                .finish(),
            );
            column = column.with_child(
                Text::new(
                    "Sets the initial model in dropdown. You can change it.",
                    appearance.ui_font_family(),
                    font_size - 1.0,
                )
                .with_color(label_color)
                .soft_wrap(true)
                .finish(),
            );
        } else {
            column = column.with_child(
                Text::new(
                    "Default Model: Not set",
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(label_color)
                .finish(),
            );
            column = column.with_child(
                Text::new(
                    "Set ANTHROPIC_MODEL in settings.json to set initial model",
                    appearance.ui_font_family(),
                    font_size - 1.0,
                )
                .with_color(label_color)
                .finish(),
            );
        }

        if let Some(model) = &self.external_config.anthropic_small_fast_model {
            column = column.with_child(
                Text::new(
                    format!("Small/Fast Model: {}", model),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(text_color)
                .soft_wrap(true)
                .finish(),
            );
        }

        // Caching Configuration
        let caching_status = if self.caching_config.enabled {
            "✓ Enabled"
        } else {
            "✗ Disabled"
        };
        column = column.with_child(
            Text::new(
                format!("Prompt Caching: {}", caching_status),
                appearance.ui_font_family(),
                font_size,
            )
            .with_color(text_color)
            .finish(),
        );

        if self.caching_config.extended_ttl_requested {
            column = column.with_child(
                Text::new(
                    "Extended 1h Cache TTL: ✓ Active",
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(text_color)
                .finish(),
            );
            column = column.with_child(
                Text::new(
                    "All cache points use 1-hour TTL. Model list filtered to compatible models only.",
                    appearance.ui_font_family(),
                    font_size - 1.0,
                )
                .with_color(label_color)
                .soft_wrap(true)
                .finish(),
            );
            column = column.with_child(
                Text::new(
                    "Compatible: Claude Opus 4.5, Sonnet 4.5, Haiku 4.5",
                    appearance.ui_font_family(),
                    font_size - 1.0,
                )
                .with_color(label_color)
                .soft_wrap(true)
                .finish(),
            );
        }

        // Additional Models
        if !self.external_config.models.is_empty() {
            column = column.with_child(
                Text::new(
                    format!("\nAdditional models: {}", self.external_config.models.len()),
                    appearance.ui_font_family(),
                    font_size,
                )
                .with_color(label_color)
                .finish(),
            );
        }

        // Auth Refresh Command
        if let Some(cmd) = &self.external_config.auth_refresh_command {
            column = column.with_child(
                Text::new(
                    format!("\nAuth Refresh: {}", cmd),
                    appearance.ui_font_family(),
                    font_size - 1.0,
                )
                .with_color(label_color)
                .soft_wrap(true)
                .finish(),
            );
        }

        // Help text
        column = column.with_child(
            Text::new(
                "\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
                appearance.ui_font_family(),
                font_size,
            )
            .with_color(label_color)
            .finish(),
        );
        column = column.with_child(
            Text::new(
                "Settings loaded from ~/.claude/settings.json",
                appearance.ui_font_family(),
                font_size - 1.0,
            )
            .with_color(label_color)
            .finish(),
        );

        Container::new(column.finish())
            .with_uniform_padding(12.0)
            .with_background(theme.surface_2())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.0)))
            .finish()
    }
}

impl Entity for SettingsView {
    type Event = ();
}

impl TypedActionView for SettingsView {
    type Action = ();

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
