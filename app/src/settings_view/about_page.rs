use galaxyui::ui_components::components::UiComponent;
use galaxyui::{AppContext, Entity, SingletonEntity, View, ViewContext, ViewHandle};
use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{
    Align, CacheOption, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, Image,
    MainAxisAlignment, MouseStateHandle, ParentElement, Wrap,
};

use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use super::SettingsSection;
use crate::appearance::Appearance;
use crate::channel::ChannelState;
use crate::settings::app_icon::AppIconSettings;
use crate::themes::theme::ColorScheme;
use crate::workspace::WorkspaceAction;

pub struct AboutPageView {
    page: PageType<Self>,
}

impl AboutPageView {
    pub fn new(_ctx: &mut ViewContext<AboutPageView>) -> Self {
        AboutPageView {
            page: PageType::new_monolith(AboutPageWidget::default(), None, false),
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

#[derive(Default)]
struct AboutPageWidget {
    copy_version_button_mouse_state: MouseStateHandle,
}

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
        let ui_builder = appearance.ui_builder();

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

        let version =
            ChannelState::app_version().unwrap_or(concat!("v", env!("CARGO_PKG_VERSION")));

        let version_text = ui_builder
            .span(version.to_string())
            .with_soft_wrap()
            .build()
            .with_margin_top(16.)
            .finish();

        let copy_version_icon = appearance
            .ui_builder()
            .copy_button(16., self.copy_version_button_mouse_state.clone())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(version));
            })
            .finish();

        let version_row = Wrap::row()
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_children([
                version_text,
                Container::new(copy_version_icon)
                    .with_margin_top(16.)
                    .with_padding_left(6.)
                    .finish(),
            ]);

        Align::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Image::new(
                            AssetSource::Bundled { path: image_path },
                            CacheOption::BySize,
                        )
                        .finish(),
                    )
                    .with_max_height(100.)
                    .with_max_width(350.)
                    .finish(),
                )
                .with_child(version_row.finish())
                .with_child(
                    ui_builder
                        .span("Galaxy is a local-first, open-source developer terminal built around your workflow, your data, and the model providers you choose.")
                        .build()
                        .with_margin_top(16.)
                        .finish(),
                )
                .with_child(
                    ui_builder
                        .span("Conversations, settings, and Galaxy Drive content are stored locally by default. Network access is limited to providers and tools that you explicitly configure.")
                        .with_soft_wrap()
                        .build()
                        .with_margin_top(8.)
                        .finish(),
                )
                .with_child(
                    ui_builder
                        .span("The application and most workspace crates are licensed under AGPL-3.0-only. GalaxyUI crates are licensed under the MIT License. See LICENSE-AGPL and LICENSE-MIT in the source repository for the full terms.")
                        .with_soft_wrap()
                        .build()
                        .with_margin_top(8.)
                        .finish(),
                )
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
