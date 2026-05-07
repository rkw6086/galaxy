use galaxyui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};
use onboarding::OnboardingIntention;

#[derive(Clone, Debug)]
pub enum LoginSlideSource {
    OnboardingFlow,
    PrivacySettingsFromTerminalIntentionTheme,
    LoginExistingUserFromWelcome,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum LoginSlideEvent {
    BackToOnboarding,
    LoginLaterConfirmed,
}

pub struct LoginSlideView;

impl LoginSlideView {
    pub fn new(
        _ai_enabled: bool,
        _theme_name: &str,
        _use_vertical_tabs: bool,
        _intention: OnboardingIntention,
        _source: LoginSlideSource,
        _ctx: &mut ViewContext<Self>,
    ) -> Self {
        Self
    }

    pub fn is_auth_token_input_visible(&self) -> bool {
        false
    }
}

impl Entity for LoginSlideView {
    type Event = LoginSlideEvent;
}

impl View for LoginSlideView {
    fn ui_name() -> &'static str {
        "LoginSlideView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}

impl TypedActionView for LoginSlideView {
    type Action = ();
    fn handle_action(&mut self, _action: &(), _ctx: &mut ViewContext<Self>) {}
}
