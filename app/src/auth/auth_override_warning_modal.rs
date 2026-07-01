use pathfinder_color::ColorU;
use galaxy_core::ui::appearance::Appearance;
use galaxyui::elements::{ChildView, Container, Fill};
use galaxyui::ui_components::components::{Coords, UiComponentStyles};
use galaxyui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::auth_manager::{AuthManager, AuthManagerEvent};
use super::auth_override_warning_body::AuthOverrideWarningBodyEvent;
use crate::auth::auth_override_warning_body::AuthOverrideWarningBody;
use crate::auth::auth_view_modal::AuthRedirectPayload;
use crate::modal::Modal;
use crate::root_view::unthemed_window_border;

pub struct AuthOverrideWarningModal {
    auth_override_warning_modal: ViewHandle<Modal<AuthOverrideWarningBody>>,
    interrupted_auth_payload: Option<AuthRedirectPayload>,
    variant: AuthOverrideWarningModalVariant,
}

#[derive(Clone, Debug)]
pub enum AuthOverrideWarningModalVariant {
    OnboardingView,
    WorkspaceModal,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum AuthOverrideWarningModalEvent {
    Close,
    BulkExport,
}

pub struct AuthOverrideWarningModal;

impl AuthOverrideWarningModal {
    pub fn new(_ctx: &mut ViewContext<Self>, _variant: AuthOverrideWarningModalVariant) -> Self {
        Self
    }

    pub fn set_interrupted_auth_payload(&mut self, _payload: AuthRedirectPayload) {}
}

impl Entity for AuthOverrideWarningModal {
    type Event = AuthOverrideWarningModalEvent;
}

impl View for AuthOverrideWarningModal {
    fn ui_name() -> &'static str {
        "AuthOverrideWarningModal"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}

impl TypedActionView for AuthOverrideWarningModal {
    type Action = ();
    fn handle_action(&mut self, _action: &(), _ctx: &mut ViewContext<Self>) {}
}
