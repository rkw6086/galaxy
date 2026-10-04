//! Renders the Galaxy logo with shimmering text for loading states.

use galaxy_core::ui::appearance::Appearance;
use galaxyui::elements::shimmering_text::{
    ShimmerConfig, ShimmeringTextElement, ShimmeringTextStateHandle,
};
use galaxyui::elements::Element;
use galaxyui::{AppContext, SingletonEntity};

pub const LOADING_LOGO_GAP: f32 = 8.;

pub fn loading_logo_size(font_size: f32) -> f32 {
    (font_size * 1.5).max(24.)
}

/// Creates a shimmering text element beside the Galaxy logo.
pub fn shimmering_warp_loading_text(
    text: impl Into<String>,
    font_size: f32,
    shimmer_handle: ShimmeringTextStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    // Use same colors as common.rs for consistency
    let base_color = theme.disabled_text_color(theme.surface_1()).into_solid();
    let shimmer_color = theme.main_text_color(theme.surface_1()).into_solid();

    // Hardcoded shimmer config for consistent animation
    let config = ShimmerConfig::default();

    ShimmeringTextElement::new(
        text.into(),
        appearance.ui_font_family(),
        font_size,
        base_color,
        shimmer_color,
        config,
        shimmer_handle,
    )
    .with_leading_icon(
        "bundled/svg/galaxy-mark.svg",
        loading_logo_size(font_size),
        LOADING_LOGO_GAP,
    )
    .finish()
}
