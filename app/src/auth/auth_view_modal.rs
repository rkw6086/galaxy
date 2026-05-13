use anyhow::Result;
use galaxyui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};
use url::Url;

use super::credentials::RefreshToken;
use super::user_uid::UserUid;

#[derive(Clone, Debug)]
pub struct AuthRedirectPayload {
    pub refresh_token: RefreshToken,
    pub user_uid: Option<UserUid>,
    pub deleted_anonymous_user: Option<bool>,
    pub state: Option<String>,
}

impl AuthRedirectPayload {
    pub fn from_url(_url: Url) -> Result<Self> {
        anyhow::bail!("Auth UI removed")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AuthViewVariant {
    Initial,
    RequireLoginCloseable,
    ShareRequirementCloseable,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum AuthViewEvent {
    Close,
}

pub struct AuthView {
    pub last_login_failure_reason: Option<LoginFailureReason>,
}

impl AuthView {
    pub fn new(_variant: AuthViewVariant, _ctx: &mut ViewContext<Self>) -> Self {
        Self {
            last_login_failure_reason: None,
        }
    }

    pub fn set_variant(&mut self, _ctx: &mut ViewContext<Self>, _variant: AuthViewVariant) {}

    pub fn skip_to_browser_open_step(&mut self, _ctx: &mut ViewContext<Self>) {}
}

impl Entity for AuthView {
    type Event = AuthViewEvent;
}

impl View for AuthView {
    fn ui_name() -> &'static str {
        "AuthView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}

impl TypedActionView for AuthView {
    type Action = ();
    fn handle_action(&mut self, _action: &(), _ctx: &mut ViewContext<Self>) {}
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum LoginFailureReason {
    InvalidRedirectUrl { was_pasted: bool },
}

#[allow(dead_code)]
pub fn init(_app: &mut AppContext) {}
