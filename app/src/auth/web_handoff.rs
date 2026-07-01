use anyhow::anyhow;
use galaxyui::ui_components::components::UiComponent as _;
use galaxyui::{AppContext, Element, Entity, SingletonEntity, View, ViewContext};
use wasm_bindgen::prelude::*;

use super::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::auth_view_modal::AuthRedirectPayload;
use crate::auth::credentials::RefreshToken;
use crate::auth::login_error_modal::LoginErrorModal;
use crate::platform::wasm::{user_handoff, AuthHandoffError};
use crate::report_error;

#[wasm_bindgen]
extern "C" {}

pub struct WebHandoffView {
    state: HandoffState,
}

#[derive(Debug, Clone)]
pub enum WebHandoffEvent {
    Unsupported,
}

pub struct WebHandoffView;

impl WebHandoffView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self
    }

    pub fn import_user(&mut self, _ctx: &mut ViewContext<Self>) {}
}

impl Entity for WebHandoffView {
    type Event = WebHandoffEvent;
}

impl View for WebHandoffView {
    fn ui_name() -> &'static str {
        "WebHandoffView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}
