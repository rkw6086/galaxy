use galaxyui::elements::{
    Align, CacheOption, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex,
    FormattedTextElement, Image, Padding, ParentElement, Text,
};
use galaxyui::fonts::Weight;
use galaxyui::text_layout::TextAlignment;
use galaxyui::{AppContext, Entity, SingletonEntity, View, ViewContext, ViewHandle};
use warpui::assets::asset_cache::AssetSource;

use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use super::SettingsSection;
use crate::appearance::Appearance;
use crate::settings::app_icon::AppIconSettings;

pub struct AboutPageView {
    page: PageType<Self>,
}

impl AboutPageView {
    pub fn new(_ctx: &mut ViewContext<AboutPageView>) -> Self {
        AboutPageView {
            page: PageType::new_monolith(AboutPageWidget, None, false),
        }
    }
}

impl Entity for AboutPageView {
    type Event = SettingsPageEvent;
}

impl View for AboutPageView {
    fn ui_name() -> &'static str {
        "AboutPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

struct AboutPageWidget;

impl SettingsWidget for AboutPageWidget {
    type View = AboutPageView;

    fn search_terms(&self) -> &str {
        "about galaxy ai version"
    }

    fn render(
        &self,
        _view: &AboutPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let icon_file =
            AppIconSettings::get_base_icon_file_name(*AppIconSettings::as_ref(app).app_icon);
        let image_path = match icon_file {
            "galaxy" => "bundled/png/galaxy.png",
            "galaxy_dotmatrix" => "bundled/png/galaxy_dotmatrix.png",
            "galaxy_explorer" => "bundled/png/galaxy_explorer.png",
            "galaxy_rainbow" => "bundled/png/galaxy_rainbow.png",
            "galaxy_wormhole" => "bundled/png/galaxy_wormhole.png",
            _ => "bundled/png/galaxy.png",
        };

        let icon = ConstrainedBox::new(
            Image::new(
                AssetSource::Bundled { path: image_path },
                CacheOption::BySize,
            )
            .finish(),
        )
        .with_max_height(144.)
        .with_max_width(144.)
        .finish();
        let title = FormattedTextElement::from_str("Galaxy", appearance.ui_font_family(), 28.)
            .with_weight(Weight::Bold)
            .with_color(appearance.theme().active_ui_text_color().into_solid())
            .with_alignment(TextAlignment::Center)
            .finish();
        let version = Text::new(
            format!("Version {}", env!("CARGO_PKG_VERSION")),
            appearance.ui_font_family(),
            12.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish();
        let description = FormattedTextElement::from_str(
            "Galaxy is a local-first terminal and AI workspace built for developers. Your settings, terminal data, conversations, and Galaxy Drive content stay on this machine in your Galaxy directory. Galaxy sends request data only to the AI providers you configure.",
            appearance.ui_font_family(),
            14.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into_solid())
        .with_alignment(TextAlignment::Center)
        .with_line_height_ratio(1.35)
        .finish();

        Align::new(
            ConstrainedBox::new(
                Container::new(
                    Flex::column()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(12.)
                        .with_child(icon)
                        .with_child(title)
                        .with_child(version)
                        .with_child(description)
                        .finish(),
                )
                .with_padding(Padding::uniform(24.))
                .finish(),
            )
            .with_max_width(560.)
            .finish(),
        )
        .finish()
    }
}

impl SettingsPageMeta for AboutPageView {
    fn section() -> SettingsSection {
        SettingsSection::About
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<AboutPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AboutPageView>) -> Self {
        SettingsPageViewHandle::About(view_handle)
    }
}
