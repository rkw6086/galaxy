use galaxyui::{AppContext, Element, Entity, View, ViewContext};

#[derive(Clone, Debug)]
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
