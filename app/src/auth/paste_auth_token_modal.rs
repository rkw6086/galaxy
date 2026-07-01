//! Modal shown when the user clicks "Click here to paste your token from
//! the browser" on the onboarding agent-slide upgrade-prompt bar. Accepts a
//! pasted auth redirect URL and routes it through
//! `AuthManager::initialize_user_from_auth_payload`.
//!
//! This lives in the app crate (not the onboarding crate) because it reuses
//! `EditorView` for the text input, which the onboarding crate doesn't
//! depend on.
use pathfinder_color::ColorU;
use ui_components::{button, Component as _, Options as _};
use galaxy_core::safe_error;
use galaxy_core::ui::theme::color::internal_colors;
use galaxyui::actions::StandardAction;
use galaxyui::elements::{
    Align, Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss, Fill,
    Flex, FormattedTextElement, HighlightedHyperlink, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, Radius, Shrinkable, Stack,
};
use galaxyui::fonts::Weight;
use galaxyui::keymap::{FixedBinding, Keystroke};
use galaxyui::text_layout::TextAlignment;
use galaxyui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use galaxyui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::auth_view_modal::AuthRedirectPayload;
use crate::auth::login_failure_notification::LoginFailureReason;
use crate::editor::{
    EditorView, InteractionState, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::server::server_api::auth::UserAuthenticationError;
use crate::themes::theme::Fill as ThemeFill;
use crate::util::bindings::CustomAction;

const MODAL_WIDTH: f32 = 460.;
const AUTH_TOKEN_INPUT_BORDER_RADIUS: Radius = Radius::Pixels(4.);

pub fn init(app: &mut AppContext) {
    use galaxyui::keymap::macros::*;
    app.register_fixed_bindings([
        FixedBinding::new(
            "enter",
            PasteAuthTokenModalAction::Confirm,
            id!(PasteAuthTokenModalView::ui_name()),
        ),
        FixedBinding::new(
            "escape",
            PasteAuthTokenModalAction::Cancel,
            id!(PasteAuthTokenModalView::ui_name()),
        ),
        FixedBinding::custom(
            CustomAction::Paste,
            PasteAuthTokenModalAction::PasteIntoEditor,
            "Paste",
            id!(PasteAuthTokenModalView::ui_name()),
        ),
        FixedBinding::standard(
            StandardAction::Paste,
            PasteAuthTokenModalAction::PasteIntoEditor,
            id!(PasteAuthTokenModalView::ui_name()),
        ),
    ]);

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
    app.register_fixed_bindings([FixedBinding::new(
        "cmdorctrl-v",
        PasteAuthTokenModalAction::PasteIntoEditor,
        id!(PasteAuthTokenModalView::ui_name()),
    )]);
}

#[derive(Clone, Copy, Debug)]
pub enum PasteAuthTokenModalAction {
    Confirm,
    Cancel,
    /// Cmd+V/Ctrl+V at the modal level — routes paste into the editor even
    /// when focus is still on the modal itself rather than the input.
    PasteIntoEditor,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum PasteAuthTokenModalEvent {
    Cancelled,
}

pub struct PasteAuthTokenModalView;

impl PasteAuthTokenModalView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let auth_token_input = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let theme = appearance.theme();
            let bg_solid = theme.surface_2().into_solid();
            let default_color = ThemeFill::Solid(internal_colors::text_main(theme, bg_solid));
            let disabled_color = ThemeFill::Solid(internal_colors::text_disabled(theme, bg_solid));
            let hint_color = ThemeFill::Solid(internal_colors::text_sub(theme, bg_solid));
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(12.),
                        font_family_override: Some(appearance.ui_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color,
                            disabled_color,
                            hint_color,
                        }),
                        ..Default::default()
                    },
                    soft_wrap: false,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Enter auth token", ctx);
            editor
        });

        // When the editor sees an Enter/Paste/etc. commit, submit the current
        // buffer text upward. This matches the semantics of the inline editor
        // in `login_slide.rs`.
        ctx.subscribe_to_view(&auth_token_input, |me, _, event, ctx| {
            use crate::editor::Event::{AltEnter, CmdEnter, Enter, Paste, ShiftEnter};
            match event {
                AltEnter | CmdEnter | Enter | Paste | ShiftEnter => {
                    me.submit(ctx);
                }
                _ => {}
            };
            ctx.notify();
        });

        // Handle AuthFailed for attempts that originated from this modal: show
        // an inline error and re-enable the editor so the user can try again.
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |me, _, event, ctx| {
            if let AuthManagerEvent::AuthFailed(err) = event {
                me.last_failure_reason = Some(match err {
                    UserAuthenticationError::InvalidStateParameter => {
                        LoginFailureReason::InvalidStateParameter
                    }
                    UserAuthenticationError::MissingStateParameter => {
                        LoginFailureReason::MissingStateParameter
                    }
                    UserAuthenticationError::DeniedAccessToken(_)
                    | UserAuthenticationError::UserAccountDisabled(_)
                    | UserAuthenticationError::Unexpected(_) => {
                        LoginFailureReason::FailedUserAuthentication
                    }
                });
                me.set_editor_enabled(true, ctx);
                ctx.notify();
            }
        });

        Self {
            auth_token_input,
            cancel_button: button::Button::default(),
            continue_button: button::Button::default(),
            close_mouse_state: MouseStateHandle::default(),
            last_failure_reason: None,
            highlighted_hyperlink_state: HighlightedHyperlink::default(),
        }
    }

    /// Disables the editor while the auth request is in flight. Re-enabled
    /// automatically on `AuthManagerEvent::AuthFailed` or on local parse
    /// failure in `submit`.
    fn set_editor_enabled(&mut self, is_enabled: bool, ctx: &mut ViewContext<Self>) {
        let state = if is_enabled {
            InteractionState::Editable
        } else {
            InteractionState::Disabled
        };
        self.auth_token_input
            .update(ctx, |editor, ctx| editor.set_interaction_state(state, ctx));
    }

    fn submit(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.auth_token_input.as_ref(ctx).buffer_text(ctx);
        if text.trim().is_empty() {
            return;
        }
        // Clear any previous error before the next attempt.
        self.last_failure_reason = None;
        self.set_editor_enabled(false, ctx);
        match AuthRedirectPayload::from_raw_url(text) {
            Ok(payload) => {
                AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                    auth_manager.initialize_user_from_auth_payload(payload, true, ctx);
                });
            }
            Err(error) => {
                safe_error!(
                    safe: ("Failed to parse pasted auth URL"),
                    full: ("Failed to parse pasted auth URL: {error:#}")
                );
                self.last_failure_reason =
                    Some(LoginFailureReason::InvalidRedirectUrl { was_pasted: true });
                self.set_editor_enabled(true, ctx);
                ctx.notify();
            }
        }
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
