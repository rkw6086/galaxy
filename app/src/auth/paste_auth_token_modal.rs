use galaxyui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum PasteAuthTokenModalEvent {
    Cancelled,
}

pub struct PasteAuthTokenModalView;

impl PasteAuthTokenModalView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self
    }
}

impl Entity for PasteAuthTokenModalView {
    type Event = PasteAuthTokenModalEvent;
}

impl View for PasteAuthTokenModalView {
    fn ui_name() -> &'static str {
        "PasteAuthTokenModalView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        galaxyui::elements::Empty::new().finish()
    }
}

impl TypedActionView for PasteAuthTokenModalView {
    type Action = ();
    fn handle_action(&mut self, _action: &(), _ctx: &mut ViewContext<Self>) {}
}
