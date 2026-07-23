use galaxyui::{AppContext, ModelHandle, ViewContext, ViewHandle};

use super::view::PaneView;
use super::{
    DetachType, PaneConfiguration, PaneContent, PaneGroup, PaneId, ShareableLink,
    ShareableLinkError,
};
use crate::app_state::{EnvironmentManagementPaneSnapshot, LeafContents};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::settings_view::{SettingsView, SettingsViewEvent};

pub struct EnvironmentManagementPane {
    view: ViewHandle<PaneView<SettingsView>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
}

impl EnvironmentManagementPane {
    pub fn new(ctx: &mut ViewContext<PaneGroup>) -> Self {
        let settings_view = ctx.add_typed_action_view(|ctx| SettingsView::new(None, ctx));
        let pane_configuration = settings_view.as_ref(ctx).pane_configuration();

        let view = ctx.add_typed_action_view(|ctx| {
            let pane_id = PaneId::from_environment_management_pane_ctx(ctx);
            PaneView::new(pane_id, settings_view, (), pane_configuration.clone(), ctx)
        });

        Self {
            view,
            pane_configuration,
        }
    }
}

impl PaneContent for EnvironmentManagementPane {
    fn id(&self) -> PaneId {
        PaneId::from_environment_management_pane_view(&self.view)
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        focus_handle: PaneFocusHandle,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        self.view
            .update(ctx, |view, ctx| view.set_focus_handle(focus_handle, ctx));
        let pane_id = self.id();
        ctx.subscribe_to_view(&self.view, move |group, _, event, ctx| {
            group.handle_pane_view_event(pane_id, event, ctx);
        });
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        _detach_type: DetachType,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        ctx.unsubscribe_to_view(&self.view);
    }

    fn snapshot(&self, _ctx: &AppContext) -> LeafContents {
        LeafContents::EnvironmentManagement(EnvironmentManagementPaneSnapshot)
    }

    fn has_application_focus(&self, ctx: &mut ViewContext<PaneGroup>) -> bool {
        self.view.is_self_or_child_focused(ctx)
    }

    fn focus(&self, ctx: &mut ViewContext<PaneGroup>) {
        ctx.focus(&self.view);
    }

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<ShareableLink, ShareableLinkError> {
        Ok(ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, ctx: &AppContext) -> bool {
        self.view.as_ref(ctx).is_being_dragged()
    }
}
