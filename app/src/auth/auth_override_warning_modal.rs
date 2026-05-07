use galaxyui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};

use super::auth_view_modal::AuthRedirectPayload;

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
