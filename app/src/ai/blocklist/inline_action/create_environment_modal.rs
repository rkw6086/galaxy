use warpui::elements::{Element, Empty};
use warpui::{AppContext, Entity, TypedActionView, View, ViewContext};

#[derive(Debug, Clone)]
pub enum CreateEnvironmentModalEvent {
    Cancelled,
    Created { environment_id: String },
}

pub struct CreateEnvironmentModal {
    visible: bool,
}

impl CreateEnvironmentModal {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self { visible: false }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn show(&mut self, ctx: &mut ViewContext<Self>) {
        self.visible = true;
        ctx.emit(CreateEnvironmentModalEvent::Cancelled);
        ctx.notify();
    }

    pub fn hide(&mut self, ctx: &mut ViewContext<Self>) {
        self.visible = false;
        ctx.notify();
    }
}

impl Entity for CreateEnvironmentModal {
    type Event = CreateEnvironmentModalEvent;
}

impl TypedActionView for CreateEnvironmentModal {
    type Action = ();

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}

impl View for CreateEnvironmentModal {
    fn ui_name() -> &'static str {
        "CreateEnvironmentModal"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

#[cfg(test)]
#[path = "create_environment_modal_tests.rs"]
mod tests;
