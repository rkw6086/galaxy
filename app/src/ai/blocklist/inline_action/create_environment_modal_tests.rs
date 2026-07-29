use warpui::elements::{Element, Empty};
use warpui::platform::WindowStyle;
use warpui::{App, AppContext, Entity, TypedActionView, View};

use super::CreateEnvironmentModal;

#[derive(Default)]
struct TestRootView;

impl Entity for TestRootView {
    type Event = ();
}

impl TypedActionView for TestRootView {
    type Action = ();
}

impl View for TestRootView {
    fn ui_name() -> &'static str {
        "CreateEnvironmentModalTestRoot"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

#[test]
fn create_environment_modal_visibility_can_be_toggled() {
    App::test((), |mut app| async move {
        let (window_id, _) = app.add_window(WindowStyle::NotStealFocus, |_| TestRootView);
        let modal =
            app.update(|ctx| ctx.add_typed_action_view(window_id, CreateEnvironmentModal::new));

        modal.update(&mut app, |modal, ctx| {
            assert!(!modal.is_visible());

            modal.show(ctx);
            assert!(modal.is_visible());

            modal.hide(ctx);
            assert!(!modal.is_visible());
        });
    });
}
