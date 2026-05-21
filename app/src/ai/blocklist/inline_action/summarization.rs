use galaxy_core::ui::appearance::Appearance;
use galaxyui::elements::shimmering_text::{ShimmerConfig, ShimmeringTextElement, ShimmeringTextStateHandle};
use galaxyui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Flex,
    MainAxisAlignment, ParentElement, Radius, Shrinkable, Text,
};
use galaxyui::r#async::{SpawnedFutureHandle, Timer};
use galaxyui::{AppContext, Entity, SingletonEntity, View, ViewContext};
use instant::Instant;
use std::time::Duration;

use super::inline_action_header::{
    INLINE_ACTION_HEADER_VERTICAL_PADDING, INLINE_ACTION_HORIZONTAL_PADDING,
};
use super::inline_action_icons::icon_size;
use crate::ai::blocklist::block::view_impl::WithContentItemSpacing;
use crate::ui_components::icons::Icon;

pub enum SummarizationViewEvent {}

pub struct SummarizationView {
    pub is_finished: bool,
    shimmering_text_handle: ShimmeringTextStateHandle,
    start_time: Instant,
    timer_handle: Option<SpawnedFutureHandle>,
}

impl SummarizationView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let mut view = Self {
            is_finished: false,
            shimmering_text_handle: ShimmeringTextStateHandle::default(),
            start_time: Instant::now(),
            timer_handle: None,
        };
        view.start_timer(ctx);
        view
    }

    pub fn mark_finished(&mut self) {
        self.is_finished = true;
        if let Some(handle) = self.timer_handle.take() {
            handle.abort();
        }
    }

    fn start_timer(&mut self, ctx: &mut ViewContext<Self>) {
        if self.timer_handle.is_some() {
            return;
        }
        let handle = ctx.spawn(
            async move {
                Timer::after(Duration::from_secs(1)).await;
            },
            |me, _unit, ctx| {
                me.timer_handle = None;
                if !me.is_finished {
                    ctx.notify();
                    me.start_timer(ctx);
                }
            },
        );
        self.timer_handle = Some(handle);
    }

    fn render_in_progress(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let header_background = theme.surface_2();

        let mut header_row = Flex::row()
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);

        // Clock loader icon (magenta, matches InProgress convention)
        let icon_element = galaxyui::elements::Icon::new(
            Icon::ClockLoader.into(),
            theme.ansi_fg_magenta(),
        )
        .finish();
        let icon_box = ConstrainedBox::new(icon_element)
            .with_width(icon_size(app))
            .with_height(icon_size(app))
            .finish();
        header_row.add_child(
            Container::new(icon_box)
                .with_margin_right(8.)
                .finish(),
        );

        // Shimmering "Summarizing conversation..." text
        let base_color = theme.disabled_text_color(header_background).into_solid();
        let shimmer_color = theme.main_text_color(header_background).into_solid();
        let shimmer_element = ShimmeringTextElement::new(
            "Summarizing conversation...".to_string(),
            appearance.ui_font_family(),
            appearance.monospace_font_size(),
            base_color,
            shimmer_color,
            ShimmerConfig::default(),
            self.shimmering_text_handle.clone(),
        )
        .finish();
        header_row.add_child(Shrinkable::new(1.0, shimmer_element).finish());

        // Elapsed time suffix
        let elapsed = self.start_time.elapsed();
        let elapsed_text = format_elapsed(elapsed);
        let suffix = Text::new_inline(
            format!(" \u{2022} {elapsed_text}"),
            appearance.ui_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(theme.disabled_text_color(header_background).into())
        .finish();
        header_row.add_child(suffix);

        Container::new(header_row.finish())
            .with_horizontal_padding(INLINE_ACTION_HORIZONTAL_PADDING)
            .with_vertical_padding(INLINE_ACTION_HEADER_VERTICAL_PADDING)
            .with_background(header_background)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .finish()
    }

    fn render_finished(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let header_background = theme.surface_2();

        let mut header_row = Flex::row()
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);

        // Checkmark-style icon for completed
        let icon_element = galaxyui::elements::Icon::new(
            Icon::Check.into(),
            theme.ansi_fg_green(),
        )
        .finish();
        let icon_box = ConstrainedBox::new(icon_element)
            .with_width(icon_size(app))
            .with_height(icon_size(app))
            .finish();
        header_row.add_child(
            Container::new(icon_box)
                .with_margin_right(8.)
                .finish(),
        );

        let elapsed = self.start_time.elapsed();
        let elapsed_text = format_elapsed(elapsed);
        let title = Text::new_inline(
            format!("Conversation summarized \u{2022} {elapsed_text}"),
            appearance.ui_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(theme.main_text_color(header_background).into())
        .finish();
        header_row.add_child(Shrinkable::new(1.0, title).finish());

        Container::new(header_row.finish())
            .with_horizontal_padding(INLINE_ACTION_HORIZONTAL_PADDING)
            .with_vertical_padding(INLINE_ACTION_HEADER_VERTICAL_PADDING)
            .with_background(header_background)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .finish()
    }
}

impl Entity for SummarizationView {
    type Event = SummarizationViewEvent;
}

impl View for SummarizationView {
    fn ui_name() -> &'static str {
        "SummarizationView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let element = if self.is_finished {
            self.render_finished(app)
        } else {
            self.render_in_progress(app)
        };
        element.with_agent_output_item_spacing(app).finish()
    }
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}
