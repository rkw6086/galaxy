use galaxy_core::features::FeatureFlag;
use onboarding::slides::{AgentAutonomy, AgentDevelopmentSettings};
use onboarding::{SelectedSettings, SessionDefault, UICustomizationSettings};
use settings::Setting as _;
use warpui::{AppContext, SingletonEntity as _};

use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::PermissionMode;
use crate::drive::settings::WarpDriveSettings;
use crate::report_if_error;
use crate::settings::ai::DefaultSessionMode;
use crate::settings::{AISettings, CodeSettings};
use crate::workspace::tab_settings::TabSettings;

/// Applies onboarding settings based on the user's selected mode.
///
/// `has_account` indicates whether the user has (or is creating) a real Warp
/// account. Warp's AI features run on a Warp account, so agent intent only
/// enables AI when `has_account` is true; skipping login leaves AI off.
pub fn apply_onboarding_settings(
    selected_settings: &SelectedSettings,
    has_account: bool,
    app: &mut AppContext,
) {
    let is_ai_enabled = match selected_settings {
        SelectedSettings::AgentDrivenDevelopment {
            agent_settings,
            ui_customization,
            ..
        } => {
            apply_agent_settings(agent_settings, app);
            if let Some(ui) = ui_customization {
                apply_ui_customization_settings(ui, true, app);
            }
            // Agent intent means the user wants AI, but Warp's AI features run
            // on a Warp account, so AI is only enabled once they have one.
            // Skipping login leaves AI off even for agent intent (including the
            // bring-your-own-agents `disable_oz` path).
            has_account
        }
        SelectedSettings::Terminal {
            ui_customization,
            cli_agent_toolbar_enabled,
            show_agent_notifications,
        } => {
            // In old onboarding, there's nothing to set for terminal intent.
            if !FeatureFlag::OpenWarpNewSettingsModes.is_enabled() {
                true
            } else {
                if let Some(ui) = ui_customization {
                    apply_ui_customization_settings(ui, false, app);
                }
                AISettings::handle(app).update(app, |settings, ctx| {
                    report_if_error!(settings
                        .should_render_cli_agent_footer
                        .set_value(*cli_agent_toolbar_enabled, ctx));
                    report_if_error!(settings
                        .show_agent_notifications
                        .set_value(*show_agent_notifications, ctx));
                });
                false
            }
        }
    };

    if FeatureFlag::OpenWarpNewSettingsModes.is_enabled() {
        AISettings::handle(app).update(app, |settings, ctx| {
            report_if_error!(settings.is_any_ai_enabled.set_value(is_ai_enabled, ctx));
        });
    }
}

/// Applies the explicit UI customization settings chosen during the
/// "Customize your UI" onboarding slide.
fn apply_ui_customization_settings(
    ui: &UICustomizationSettings,
    is_agent_intent: bool,
    app: &mut AppContext,
) {
    // Customize UI slide should only exist with this flag enabled.
    if !FeatureFlag::OpenWarpNewSettingsModes.is_enabled() {
        return;
    }
    TabSettings::handle(app).update(app, |settings, ctx| {
        report_if_error!(settings
            .use_vertical_tabs
            .set_value(ui.use_vertical_tabs, ctx));
        report_if_error!(settings
            .show_code_review_button
            .set_value(ui.show_code_review_button, ctx));
    });

    WarpDriveSettings::handle(app).update(app, |settings, ctx| {
        report_if_error!(settings
            .enable_warp_drive
            .set_value(ui.show_warp_drive, ctx));
    });

    CodeSettings::handle(app).update(app, |settings, ctx| {
        report_if_error!(settings
            .show_project_explorer
            .set_value(ui.show_project_explorer, ctx));
        report_if_error!(settings
            .show_global_search
            .set_value(ui.show_global_search, ctx));
    });

    // For agent intent, configure showing conversation history.
    // For terminal intent, this option was not surfaced in onboarding, so leave the default.
    // It will be hidden anyway because AI is off, but we want to keep the default in case they enable AI later.
    if is_agent_intent {
        AISettings::handle(app).update(app, |settings, ctx| {
            report_if_error!(settings
                .show_conversation_history
                .set_value(ui.show_conversation_history, ctx));
        });
    }
}

fn apply_agent_settings(agent_settings: &AgentDevelopmentSettings, app: &mut AppContext) {
    // Apply session default mode.
    let default_mode = match agent_settings.session_default {
        SessionDefault::Agent => DefaultSessionMode::Agent,
        SessionDefault::Terminal => DefaultSessionMode::Terminal,
    };
    AISettings::handle(app).update(app, |settings, ctx| {
        report_if_error!(settings
            .default_session_mode_internal
            .set_value(default_mode, ctx));
    });

    AISettings::handle(app).update(app, |settings, ctx| {
        report_if_error!(settings
            .should_render_cli_agent_footer
            .set_value(agent_settings.cli_agent_toolbar_enabled, ctx));
        report_if_error!(settings
            .show_agent_notifications
            .set_value(agent_settings.show_agent_notifications, ctx));
    });

    AIExecutionProfilesModel::handle(app).update(app, |profiles, ctx| {
        let default_profile_info = profiles.default_profile(ctx);
        let default_profile_id = *default_profile_info.id();

        // Preserve the existing cloud default profile for users who are
        // already logged in (or who log in at the end of onboarding). A
        // `Some` sync_id means the profile is backed by a cloud object that
        // was either loaded at startup or reconciled during the post-login
        // initial load, and its values represent what the user has stored
        // previously. Overwriting those with the onboarding-selected
        // base_model / autonomy would silently discard their prior
        // customizations. Fresh `Unsynced` default profiles (brand-new
        // users, or users without any cloud default yet) still receive the
        // onboarding values.
        if default_profile_info.sync_id().is_some() {
            log::info!(
                "Preserving existing cloud default execution profile; skipping \
                 onboarding-driven overrides for profile {default_profile_id:?}"
            );
            return;
        }

        profiles.set_base_model(
            default_profile_id,
            Some(agent_settings.selected_model_id.clone()),
            ctx,
        );

        // If autonomy is None, the workspace enforces autonomy settings, so skip setting them.
        let Some(autonomy) = agent_settings.autonomy else {
            return;
        };

        let mode = match autonomy {
            AgentAutonomy::Full => PermissionMode::GalaxyDecides,
            AgentAutonomy::Partial => PermissionMode::AskFirst,
            AgentAutonomy::None => PermissionMode::ReadOnly,
        };
        profiles.set_permission_mode(default_profile_id, mode, ctx);
    });
}

#[cfg(test)]
#[path = "onboarding_tests.rs"]
mod tests;
