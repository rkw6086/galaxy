use galaxyui::{AppContext, Entity, SingletonEntity, View, ViewContext, ViewHandle};
use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{Align, CacheOption, ConstrainedBox, Element, Image};

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
        _appearance: &Appearance,
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

        Align::new(
            ConstrainedBox::new(
                Image::new(
                    AssetSource::Bundled { path: image_path },
                    CacheOption::BySize,
                )
                .finish(),
            )
            .with_max_height(144.)
            .with_max_width(144.)
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
