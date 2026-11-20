use galaxy_core::channel::ChannelState;
use galaxyui::elements::{
    Align, CacheOption, ChildView, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex,
    FormattedTextElement, Image, Padding, ParentElement, Text,
};
use galaxyui::fonts::Weight;
use galaxyui::text_layout::TextAlignment;
use galaxyui::{
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};
use warpui::assets::asset_cache::AssetSource;

use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use super::SettingsSection;
use crate::appearance::Appearance;
use crate::autoupdate::{self, AutoupdateStage, AutoupdateState};
use crate::settings::app_icon::AppIconSettings;
use crate::view_components::action_button::{ActionButton, SecondaryTheme};
use crate::workspace::WorkspaceAction;

#[derive(Clone, Debug, PartialEq)]
pub enum AboutPageAction {
    CheckForUpdates,
}

pub struct AboutPageView {
    page: PageType<Self>,
    check_button: ViewHandle<ActionButton>,
    restart_button: ViewHandle<ActionButton>,
    download_button: ViewHandle<ActionButton>,
}

impl AboutPageView {
    pub fn new(ctx: &mut ViewContext<AboutPageView>) -> Self {
        let check_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Check for updates", SecondaryTheme)
                .on_click(|ctx| ctx.dispatch_typed_action(AboutPageAction::CheckForUpdates))
        });
        let restart_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Install update and restart", SecondaryTheme)
                .on_click(|ctx| ctx.dispatch_typed_action(WorkspaceAction::ApplyUpdate))
        });
        let download_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Download update", SecondaryTheme)
                .on_click(|ctx| ctx.dispatch_typed_action(WorkspaceAction::DownloadNewVersion))
        });
        ctx.observe(&AutoupdateState::handle(ctx), |view, _, ctx| {
            view.update_check_button(ctx);
            ctx.notify();
        });
        let view = AboutPageView {
            page: PageType::new_monolith(AboutPageWidget, None, false),
            check_button,
            restart_button,
            download_button,
        };
        view.update_check_button(ctx);
        view
    }

    fn update_check_button(&self, ctx: &mut ViewContext<Self>) {
        let disabled = !autoupdate::is_available() || AutoupdateState::as_ref(ctx).is_busy();
        self.check_button
            .update(ctx, |button, ctx| button.set_disabled(disabled, ctx));
        let label = if matches!(
            autoupdate::get_update_state(ctx),
            AutoupdateStage::UpdatedPendingRestart { .. }
        ) {
            "Restart Galaxy"
        } else {
            "Install update and restart"
        };
        self.restart_button
            .update(ctx, |button, ctx| button.set_label(label, ctx));
    }
}

impl Entity for AboutPageView {
    type Event = SettingsPageEvent;
}
impl TypedActionView for AboutPageView {
    type Action = AboutPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AboutPageAction::CheckForUpdates => {
                AutoupdateState::handle(ctx)
                    .update(ctx, |state, ctx| state.manually_check_for_update(ctx));
            }
        }
    }
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
        view: &AboutPageView,
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
            crate::build_info::about_version_label(),
            appearance.ui_font_family(),
            12.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish();
        let build = Text::new(
            format!(
                "Build {} · {}",
                ChannelState::channel().local_control_channel_name(),
                crate::build_info::profile()
            ),
            appearance.ui_font_family(),
            12.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish();
        let build_details = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(version)
            .with_child(build)
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

        let mut content = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(12.)
            .with_child(icon)
            .with_child(title)
            .with_child(build_details)
            .with_child(description);
        if autoupdate::is_available() {
            let state = AutoupdateState::as_ref(app);
            content.add_child(
                Text::new(state.status_message(), appearance.ui_font_family(), 12.)
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .finish(),
            );
            content.add_child(ChildView::new(&view.check_button).finish());
            match autoupdate::get_update_state(app) {
                AutoupdateStage::UpdateReady { .. }
                | AutoupdateStage::UpdatedPendingRestart { .. } => {
                    content.add_child(ChildView::new(&view.restart_button).finish());
                }
                AutoupdateStage::UnableToUpdateToNewVersion { .. }
                | AutoupdateStage::UnableToLaunchNewVersion { .. } => {
                    content.add_child(ChildView::new(&view.download_button).finish());
                }
                _ => {}
            }
        }
        Align::new(
            ConstrainedBox::new(
                Container::new(content.finish())
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
