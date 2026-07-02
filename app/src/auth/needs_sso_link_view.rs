#![allow(dead_code)]

use galaxyui::elements::{Align, MouseStateHandle, Shrinkable};
use galaxyui::ui_components::button::ButtonVariant;
use galaxyui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use galaxyui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use super::auth_manager::AuthManager;
use crate::appearance::Appearance;
use crate::auth::login_error_modal::LoginErrorModal;

#[derive(Debug)]
pub enum NeedsSsoLinkViewAction {
    ClickedLinkSsoButton,
}

pub struct NeedsSsoLinkView {
    email: Option<String>,
    mouse_state_handles: MouseStateHandles,
}

#[derive(Default)]
struct MouseStateHandles {
    link_sso_handle: MouseStateHandle,
}

impl NeedsSsoLinkView {
    pub fn new() -> Self {
        Self {
            email: None,
            mouse_state_handles: MouseStateHandles::default(),
        }
    }

    pub fn set_email(&mut self, _email: String) {}
}

impl Entity for NeedsSsoLinkView {
    type Event = ();
}

impl View for NeedsSsoLinkView {
    fn ui_name() -> &'static str {
        "NeedsSsoLinkView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}

impl TypedActionView for NeedsSsoLinkView {
    type Action = ();
    fn handle_action(&mut self, _action: &(), _ctx: &mut ViewContext<Self>) {}
}
