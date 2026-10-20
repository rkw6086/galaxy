use core::fmt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AIExecutionProfile, ActionPermission, WriteToPtyPermission};
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::mcp::templatable_manager::TemplatableMCPServerManagerEvent;
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::cloud_object::CloudObject as _;
use crate::local_object_repository::{LocalObjectRepository, LocalObjectRepositoryEvent};
use crate::server::ids::{ClientId, SyncId};
use crate::settings::AgentModeCommandExecutionPredicate;
use crate::{send_telemetry_from_ctx, LaunchMode, TelemetryEvent};

/// ExecutionProfileId is the identifier that users of the AIExecutionProfilesModel use
/// to refer back to a specific profile. These are unique across the lifespan of the app.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientProfileId(usize);

impl ClientProfileId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> ClientProfileId {
        static NEXT_PROFILE_ID: AtomicUsize = AtomicUsize::new(0);
        let raw = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
        ClientProfileId(raw)
    }
}

impl fmt::Display for ClientProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Debug)]
pub struct AIExecutionProfileInfo {
    id: ClientProfileId,
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    sync_id: Option<SyncId>,
    data: AIExecutionProfile,
}

impl AIExecutionProfileInfo {
    pub fn id(&self) -> &ClientProfileId {
        &self.id
    }

    /// The persisted object ID of this profile, if it has been saved.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub fn sync_id(&self) -> Option<SyncId> {
        self.sync_id
    }

    pub fn data(&self) -> &AIExecutionProfile {
        &self.data
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum DefaultProfileState {
    Unsynced {
        id: ClientProfileId,
        profile: AIExecutionProfile,
    },
    Synced {
        id: ClientProfileId,
    },
    /// Currently, the behavior of the CLI default is that it
    /// cannot be updated and will never be synced.
    #[allow(dead_code)]
    Cli {
        id: ClientProfileId,
        profile: AIExecutionProfile,
    },
}

impl std::fmt::Display for DefaultProfileState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefaultProfileState::Unsynced { .. } => write!(f, "Unsynced"),
            DefaultProfileState::Synced { .. } => write!(f, "Synced"),
            DefaultProfileState::Cli { .. } => write!(f, "CLI"),
        }
    }
}

impl DefaultProfileState {
    pub fn id(&self) -> ClientProfileId {
        match self {
            DefaultProfileState::Unsynced { id, .. } => *id,
            DefaultProfileState::Synced { id } => *id,
            DefaultProfileState::Cli { id, .. } => *id,
        }
    }
}

pub struct AIExecutionProfilesModel {
    /// The default profile begins as an in-memory default and becomes backed
    /// by the local object repository on its first edit. CLI mode retains its
    /// separate, immutable profile.
    default_profile_state: DefaultProfileState,
    profile_id_to_sync_id: HashMap<ClientProfileId, SyncId>,
    /// Only contains entries for non-default profiles.
    active_profiles_per_session: HashMap<EntityId, ClientProfileId>,
}

impl AIExecutionProfilesModel {
    #[allow(unused_variables)]
    pub fn new(launch_mode: &LaunchMode, ctx: &mut ModelContext<Self>) -> Self {
        cfg_if::cfg_if! {
            if #[cfg(feature = "agent_mode_evals")] {
                let default_profile_state = DefaultProfileState::Unsynced {
                    id: ClientProfileId::new(),
                    profile: AIExecutionProfile::create_agent_mode_eval_profile(),
                };
                let profile_id_to_sync_id: HashMap<ClientProfileId, SyncId> = HashMap::new();
                let active_profiles_per_session: HashMap<EntityId, ClientProfileId> = HashMap::new();
            } else {
                let all_local_profiles = LocalObjectRepository::as_ref(ctx).execution_profiles(ctx);

                let default_local_profile = all_local_profiles
                    .iter()
                    .find(|obj| obj.model().string_model.is_default_profile)
                    .cloned();

                let mut profile_id_to_sync_id: HashMap<ClientProfileId, SyncId> = HashMap::new();
                let active_profiles_per_session: HashMap<EntityId, ClientProfileId> = HashMap::new();

                // Insert all non-default profiles from local persistence.
                for local_profile in all_local_profiles.iter().filter(|p| !p.model().string_model.is_default_profile) {
                    let profile_id = ClientProfileId::new();
                    profile_id_to_sync_id.insert(profile_id, local_profile.id);
                }

                let default_profile_state = match launch_mode {
                    // The TUI front-end shares the GUI app's locally persisted
                    // default execution profile.
                    LaunchMode::App { .. }
                    | LaunchMode::Test { .. }
                    | LaunchMode::Tui { .. } => {
                        match default_local_profile {
                            Some(p) => {
                                let execution_profile_id = ClientProfileId::new();
                                profile_id_to_sync_id.insert(execution_profile_id, p.id);
                                DefaultProfileState::Synced {
                                    id: execution_profile_id,
                                }
                            }
                            None => DefaultProfileState::Unsynced {
                                id: ClientProfileId::new(),
                                profile: super::create_default_from_legacy_settings(ctx),
                            },
                        }
                    }
                    // When running as a CLI, we ignore the GUI default and use a more permissive default.
                    LaunchMode::CommandLine { is_sandboxed, computer_use_override, .. } => {
                        DefaultProfileState::Cli {
                            profile: AIExecutionProfile::create_default_cli_profile(*is_sandboxed, *computer_use_override),
                            id: ClientProfileId::new()
                        }
                    }
                    // RemoteServerProxy and RemoteServerDaemon don't use AI
                    // execution profiles. They never reach this code path
                    // since they don't go through initialize_app, but handle
                    // exhaustively.
                    LaunchMode::RemoteServerProxy | LaunchMode::RemoteServerDaemon { .. } => DefaultProfileState::Unsynced {
                        id: ClientProfileId::new(),
                        profile: super::create_default_from_legacy_settings(ctx),
                    },
                };
            }
        }

        // Keep the client-ID map and subscribed views synchronized with local
        // repository changes, including legacy rows adopted at startup.
        if !cfg!(feature = "agent_mode_evals") {
            ctx.subscribe_to_model(&LocalObjectRepository::handle(ctx), |me, _, event, ctx| {
                me.handle_local_repository_event(event, ctx);
            });
        }

        ctx.subscribe_to_model(
            &TemplatableMCPServerManager::handle(ctx),
            |me, _, event, ctx| {
                me.handle_templatable_mcp_server_manager_event(event, ctx);
            },
        );

        log::info!("Initialized execution profile model with state: {default_profile_state}",);

        let mut model = Self {
            default_profile_state,
            profile_id_to_sync_id,
            active_profiles_per_session,
        };

        model.maybe_inherit_from_legacy_settings(ctx);
        model
    }

    /// This function performs one-time migrations from legacy settings into the default profile.
    /// The issue this solves is that, whenever we migrate an existing setting into the profile object,
    /// users will initialize the new field to its default value. We need to manually check to see if
    /// the legacy setting hasn't been migrated and, if it hasn't, do a one-time overwrite on the new profile
    /// field.
    fn maybe_inherit_from_legacy_settings(&mut self, ctx: &mut ModelContext<Self>) {
        let DefaultProfileState::Synced {
            id: default_profile_id,
        } = self.default_profile_state
        else {
            return;
        };

        if let Some(base_llm_id) = ctx
            .private_user_preferences()
            .read_value("PreferredAgentModeLLMId")
            .ok()
            .flatten()
            .map(|s| serde_json::from_str::<Option<LLMId>>(&s))
            .and_then(|res| res.ok())
            .flatten()
        {
            if let Err(e) = ctx
                .private_user_preferences()
                .remove_value("PreferredAgentModeLLMId")
            {
                log::error!("Failed to remove old PreferredAgentModeLLMId user pref: {e}");
            }
            self.set_base_model(default_profile_id, Some(base_llm_id.clone()), ctx);
            log::info!("Overwrote default profile with legacy setting for base llm: {base_llm_id}");
        }
    }

    pub fn create_profile(&mut self, ctx: &mut ModelContext<Self>) -> Option<ClientProfileId> {
        let profile_id = ClientProfileId::new();

        let mut new_profile = self.default_profile(ctx).data().clone();
        new_profile.name = "".to_string();
        new_profile.is_default_profile = false;
        new_profile.autosync_plans_to_warp_drive = true;

        let sync_id = SyncId::ClientId(ClientId::new());
        self.profile_id_to_sync_id.insert(profile_id, sync_id);
        LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
            repository.create_execution_profile_with_id(sync_id, new_profile, ctx);
        });

        send_telemetry_from_ctx!(TelemetryEvent::AIExecutionProfileCreated, ctx);

        ctx.emit(AIExecutionProfilesModelEvent::ProfileCreated);

        Some(profile_id)
    }

    pub fn delete_profile(&mut self, profile_id: ClientProfileId, ctx: &mut ModelContext<Self>) {
        let id = self.default_profile_state.id();
        if id == profile_id {
            log::warn!("Attempted to delete default profile (id: {profile_id})");
            return;
        }

        let Some(sync_id) = self.profile_id_to_sync_id.get(&profile_id).cloned() else {
            return;
        };

        self.active_profiles_per_session
            .retain(|_, active_profile_id| *active_profile_id != profile_id);

        self.profile_id_to_sync_id.remove(&profile_id);

        LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
            repository.delete_execution_profile(sync_id, ctx);
        });

        send_telemetry_from_ctx!(TelemetryEvent::AIExecutionProfileDeleted, ctx);
        ctx.emit(AIExecutionProfilesModelEvent::ProfileDeleted);
    }

    /// Resets the in-memory profile index to conservative defaults.
    pub fn reset(&mut self) {
        self.default_profile_state = DefaultProfileState::Unsynced {
            id: ClientProfileId::new(),
            profile: AIExecutionProfile {
                is_default_profile: true,
                ..Default::default()
            },
        };
        self.profile_id_to_sync_id.clear();
        self.active_profiles_per_session.clear();
    }

    /// Returns the active permissions profile for a specific terminal view.
    /// If no terminal_view is provided, returns the default profile.
    ///
    /// If you need to account for enterprise overrides, call `BlocklistAIPermissions::active_permissions_profile` instead.
    pub fn active_profile(
        &self,
        terminal_view_id: Option<EntityId>,
        ctx: &AppContext,
    ) -> AIExecutionProfileInfo {
        terminal_view_id
            .and_then(|id| self.active_profiles_per_session.get(&id))
            .and_then(|profile_id| self.get_profile_by_id(*profile_id, ctx))
            .unwrap_or_else(|| self.default_profile(ctx))
    }

    pub fn default_profile_id(&self) -> ClientProfileId {
        self.default_profile_state.id()
    }

    pub fn default_profile(&self, ctx: &AppContext) -> AIExecutionProfileInfo {
        match &self.default_profile_state {
            DefaultProfileState::Unsynced { id, profile } => AIExecutionProfileInfo {
                id: *id,
                sync_id: None,
                data: profile.clone(),
            },
            DefaultProfileState::Synced { id } => {
                let Some(sync_id) = self.profile_id_to_sync_id.get(id) else {
                    log::error!(
                        "Default profile is synced but no sync_id found in profile_id_to_sync_id map."
                    );
                    return AIExecutionProfileInfo {
                        id: *id,
                        sync_id: None,
                        data: AIExecutionProfile::default(),
                    };
                };
                let data = LocalObjectRepository::as_ref(ctx)
                    .execution_profile(sync_id, ctx)
                    .map(|o| o.model().string_model.clone())
                    .unwrap_or_default();

                AIExecutionProfileInfo {
                    id: *id,
                    sync_id: Some(*sync_id),
                    data,
                }
            }
            DefaultProfileState::Cli { id, profile } => AIExecutionProfileInfo {
                id: *id,
                sync_id: None,
                data: profile.clone(),
            },
        }
    }

    /// Sets the active profile for a specific terminal view.
    pub fn set_active_profile(
        &mut self,
        terminal_view_id: EntityId,
        profile_id: ClientProfileId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.active_profiles_per_session
            .insert(terminal_view_id, profile_id);
        ctx.emit(AIExecutionProfilesModelEvent::UpdatedActiveProfile { terminal_view_id });
    }

    /// Returns a profile by its client ID.
    /// Returns None if the profile is not found.
    pub fn get_profile_by_id(
        &self,
        profile_id: ClientProfileId,
        ctx: &AppContext,
    ) -> Option<AIExecutionProfileInfo> {
        // Handle an unsynced default profile (including CLI)
        match &self.default_profile_state {
            DefaultProfileState::Unsynced { id, profile }
            | DefaultProfileState::Cli { id, profile } => {
                if profile_id == *id {
                    return Some(AIExecutionProfileInfo {
                        id: *id,
                        sync_id: None,
                        data: profile.clone(),
                    });
                }
            }
            DefaultProfileState::Synced { .. } => {}
        }

        // Handle all synced profiles (default and non-default)
        let sync_id = self.profile_id_to_sync_id.get(&profile_id)?;
        let data = LocalObjectRepository::as_ref(ctx)
            .execution_profile(sync_id, ctx)
            .map(|o| o.model().string_model.clone())
            .unwrap_or_default();

        Some(AIExecutionProfileInfo {
            id: profile_id,
            sync_id: Some(*sync_id),
            data,
        })
    }

    pub fn get_all_profile_ids(&self) -> Vec<ClientProfileId> {
        let default_profile_id = self.default_profile_state.id();

        // Default profile is always first in the list
        std::iter::once(default_profile_id)
            .chain(
                self.profile_id_to_sync_id
                    .keys()
                    .filter(|&&id| id != default_profile_id)
                    .cloned(),
            )
            .collect()
    }

    /// Look up a local client profile ID from its cloud sync ID.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub fn get_profile_id_by_sync_id(&self, sync_id: &SyncId) -> Option<ClientProfileId> {
        self.profile_id_to_sync_id
            .iter()
            .find_map(|(client_id, id)| {
                if id == sync_id {
                    Some(*client_id)
                } else {
                    None
                }
            })
    }

    pub fn has_multiple_profiles(&self) -> bool {
        let default_profile_id = self.default_profile_state.id();

        self.profile_id_to_sync_id
            .keys()
            .any(|&id| id != default_profile_id)
    }

    pub fn set_base_model(
        &mut self,
        profile_id: ClientProfileId,
        llm_id: Option<LLMId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.base_model != llm_id {
                    profile.base_model = llm_id.clone();
                    return true;
                }
                false
            },
            ctx,
        );

        if let Some(model_id) = &llm_id {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileModelSelected {
                    model_type: "base".to_string(),
                    model_value: model_id.to_string(),
                },
                ctx
            );
        }
    }

    pub fn set_coding_model(
        &mut self,
        profile_id: ClientProfileId,
        model_id: Option<LLMId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.coding_model != model_id {
                    profile.coding_model = model_id.clone();
                    return true;
                }
                false
            },
            ctx,
        );

        if let Some(model_id) = &model_id {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileModelSelected {
                    model_type: "coding".to_string(),
                    model_value: model_id.to_string(),
                },
                ctx
            );
        }
    }

    pub fn set_cli_agent_model(
        &mut self,
        profile_id: ClientProfileId,
        model_id: Option<LLMId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.cli_agent_model != model_id {
                    profile.cli_agent_model = model_id.clone();
                    return true;
                }
                false
            },
            ctx,
        );

        if let Some(model_id) = &model_id {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileModelSelected {
                    model_type: "cli_agent".to_string(),
                    model_value: model_id.to_string(),
                },
                ctx
            );
        }
    }

    pub fn set_computer_use_model(
        &mut self,
        profile_id: ClientProfileId,
        model_id: Option<LLMId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.computer_use_model != model_id {
                    profile.computer_use_model = model_id.clone();
                    return true;
                }
                false
            },
            ctx,
        );

        if let Some(model_id) = &model_id {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileModelSelected {
                    model_type: "computer_use".to_string(),
                    model_value: model_id.to_string(),
                },
                ctx
            );
        }
    }

    pub fn set_context_window_limit(
        &mut self,
        profile_id: ClientProfileId,
        limit: Option<u32>,
        ctx: &mut ModelContext<Self>,
    ) {
        let changed = self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.context_window_limit != limit {
                    profile.context_window_limit = limit;
                    return true;
                }
                false
            },
            ctx,
        );

        // Gate on the limit being non-empty. The limit is cleared during
        // reconciliation, which runs inside an `LLMPreferences` update where the
        // `LLMPreferences::as_ref` read below would panic.
        if changed && limit.is_some() {
            let Some(profile) = self.get_profile_by_id(profile_id, ctx) else {
                return;
            };
            let llm_preferences = LLMPreferences::as_ref(ctx);
            let model_info = profile
                .data()
                .base_model
                .as_ref()
                .and_then(|id| llm_preferences.get_llm_info(id))
                .unwrap_or_else(|| llm_preferences.get_default_base_model());
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileContextWindowSelected {
                    tokens: limit,
                    model_id: model_info.id.to_string(),
                },
                ctx
            );
        }
    }

    pub fn set_apply_code_diffs(
        &mut self,
        profile_id: ClientProfileId,
        apply_code_diffs: &ActionPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.apply_code_diffs != *apply_code_diffs {
                    profile.apply_code_diffs = *apply_code_diffs;
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "apply_code_diffs".to_string(),
                setting_value: format!("{apply_code_diffs:?}"),
            },
            ctx
        );
    }

    pub fn set_read_files(
        &mut self,
        profile_id: ClientProfileId,
        read_files: &ActionPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.read_files != *read_files {
                    profile.read_files = *read_files;
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "read_files".to_string(),
                setting_value: format!("{read_files:?}"),
            },
            ctx
        );
    }

    pub fn set_orchestration_providers(
        &mut self,
        profile_id: ClientProfileId,
        providers: Option<Vec<String>>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(profile_id, |profile| {
            if profile.orchestration_providers == providers { return false; }
            profile.orchestration_providers = providers;
            true
        }, ctx);
    }

    pub fn set_permission_mode(
        &mut self,
        profile_id: ClientProfileId,
        mode: super::PermissionMode,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.permission_mode == mode {
                    return false;
                }
                profile.permission_mode = mode;
                profile.approved_command_hashes.clear();
                profile.project_permission_mode();
                true
            },
            ctx,
        );
    }

    pub fn remember_approved_command(
        &mut self,
        profile_id: ClientProfileId,
        fingerprint: String,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.permission_mode != super::PermissionMode::AskFirst
                    || profile.approved_command_hashes.contains(&fingerprint)
                {
                    return false;
                }
                if profile.approved_command_hashes.len() >= 256 {
                    profile.approved_command_hashes.remove(0);
                }
                profile.approved_command_hashes.push(fingerprint);
                true
            },
            ctx,
        );
    }

    pub fn set_execute_commands(
        &mut self,
        profile_id: ClientProfileId,
        execute_commands: &ActionPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.execute_commands != *execute_commands {
                    profile.execute_commands = *execute_commands;
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "execute_commands".to_string(),
                setting_value: format!("{execute_commands:?}"),
            },
            ctx
        );
    }

    pub fn set_write_to_pty(
        &mut self,
        profile_id: ClientProfileId,
        write_to_pty: &WriteToPtyPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.write_to_pty != *write_to_pty {
                    profile.write_to_pty = *write_to_pty;
                    return true;
                }
                false
            },
            ctx,
        );
        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "write_to_pty".to_string(),
                setting_value: format!("{write_to_pty:?}"),
            },
            ctx
        );
    }

    pub fn set_mcp_permissions(
        &mut self,
        profile_id: ClientProfileId,
        mcp_permissions: &ActionPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.mcp_permissions == *mcp_permissions {
                    return false;
                }

                if mcp_permissions == &ActionPermission::AlwaysAllow {
                    profile.mcp_allowlist.clear();
                } else if mcp_permissions == &ActionPermission::AlwaysAsk {
                    profile.mcp_denylist.clear();
                }
                profile.mcp_permissions = *mcp_permissions;
                true
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "mcp_permissions".to_string(),
                setting_value: format!("{mcp_permissions:?}"),
            },
            ctx
        );
    }

    pub fn set_computer_use(
        &mut self,
        profile_id: ClientProfileId,
        permission: &super::ComputerUsePermission,
        ctx: &mut ModelContext<Self>,
    ) {
        let current_value = self
            .get_profile_by_id(profile_id, ctx)
            .map(|p| p.data().computer_use);

        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.computer_use != *permission {
                    profile.computer_use = *permission;
                    return true;
                }
                false
            },
            ctx,
        );

        if current_value != Some(*permission) {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileSettingUpdated {
                    setting_type: "computer_use".to_string(),
                    setting_value: format!("{permission:?}"),
                },
                ctx
            );
        }
    }

    pub fn set_ask_user_question(
        &mut self,
        profile_id: ClientProfileId,
        permission: super::AskUserQuestionPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        let current_value = self
            .get_profile_by_id(profile_id, ctx)
            .map(|p| p.data().ask_user_question);

        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.ask_user_question != permission {
                    profile.ask_user_question = permission;
                    return true;
                }
                false
            },
            ctx,
        );

        if current_value != Some(permission) {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileSettingUpdated {
                    setting_type: "ask_user_question".to_string(),
                    setting_value: format!("{permission:?}"),
                },
                ctx
            );
        }
    }

    pub fn set_run_agents(
        &mut self,
        profile_id: ClientProfileId,
        permission: super::RunAgentsPermission,
        ctx: &mut ModelContext<Self>,
    ) {
        let current_value = self
            .get_profile_by_id(profile_id, ctx)
            .map(|p| p.data().run_agents);

        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.run_agents != permission {
                    profile.run_agents = permission;
                    return true;
                }
                false
            },
            ctx,
        );

        if current_value != Some(permission) {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIExecutionProfileSettingUpdated {
                    setting_type: "run_agents".to_string(),
                    setting_value: format!("{permission:?}"),
                },
                ctx
            );
        }
    }

    pub fn set_web_search_enabled(
        &mut self,
        profile_id: ClientProfileId,
        enabled: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.web_search_enabled != enabled {
                    profile.web_search_enabled = enabled;
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "web_search_enabled".to_string(),
                setting_value: format!("{enabled}"),
            },
            ctx
        );
    }

    pub fn set_autosync_plans_to_warp_drive(
        &mut self,
        profile_id: ClientProfileId,
        enabled: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.autosync_plans_to_warp_drive != enabled {
                    profile.autosync_plans_to_warp_drive = enabled;
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "plan_auto_sync".to_string(),
                setting_value: format!("{enabled}"),
            },
            ctx
        );
    }

    /// Select exactly one persisted default, retaining the former default as a
    /// normal profile. Explicit per-terminal selections remain unchanged.
    pub fn set_default_profile(
        &mut self,
        profile_id: ClientProfileId,
        ctx: &mut ModelContext<Self>,
    ) {
        if profile_id == self.default_profile_state.id()
            || matches!(self.default_profile_state, DefaultProfileState::Cli { .. })
        {
            return;
        }
        let Some(target) = self.get_profile_by_id(profile_id, ctx) else {
            return;
        };
        let Some(sync_id) = target.sync_id() else {
            return;
        };
        if LocalObjectRepository::as_ref(ctx)
            .execution_profile(&sync_id, ctx)
            .is_none()
        {
            return;
        }
        let previous_id = self.default_profile_state.id();
        // Materialize an unsaved default before replacing its in-memory slot.
        self.edit_profile_internal(
            previous_id,
            |profile| {
                if profile.name.trim().is_empty() {
                    profile.name = "Default".to_owned();
                }
                profile.is_default_profile = false;
                true
            },
            ctx,
        );
        for id in self.get_all_profile_ids() {
            self.edit_profile_internal(
                id,
                |profile| {
                    let is_default = id == profile_id;
                    if profile.is_default_profile == is_default {
                        return false;
                    }
                    profile.is_default_profile = is_default;
                    true
                },
                ctx,
            );
        }
        self.default_profile_state = DefaultProfileState::Synced { id: profile_id };
        ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(previous_id));
        ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(profile_id));
    }

    pub fn set_profile_name(
        &mut self,
        profile_id: ClientProfileId,
        name: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if profile.name != name {
                    profile.name = name.to_string();
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileSettingUpdated {
                setting_type: "name".to_string(),
                setting_value: name.to_string(),
            },
            ctx
        );
    }

    pub fn add_to_command_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        predicate: &AgentModeCommandExecutionPredicate,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if !profile.command_allowlist.contains(predicate) {
                    profile.command_allowlist.push(predicate.clone());
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileAddedToAllowlist {
                list_type: "command".to_string(),
                value: predicate.to_string(),
            },
            ctx
        );
    }

    pub fn remove_from_command_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        predicate: &AgentModeCommandExecutionPredicate,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                let original_len = profile.command_allowlist.len();
                profile.command_allowlist.retain(|p| p != predicate);
                profile.command_allowlist.len() != original_len
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileRemovedFromAllowlist {
                list_type: "command".to_string(),
                value: predicate.to_string(),
            },
            ctx
        );
    }

    pub fn add_to_directory_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        path: &PathBuf,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if !profile.directory_allowlist.contains(path) {
                    profile.directory_allowlist.push(path.clone());
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileAddedToAllowlist {
                list_type: "directory".to_string(),
                value: path.to_string_lossy().to_string(),
            },
            ctx
        );
    }

    pub fn remove_from_directory_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        path: &PathBuf,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                let original_len = profile.directory_allowlist.len();
                profile.directory_allowlist.retain(|p| p != path);
                profile.directory_allowlist.len() != original_len
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileRemovedFromAllowlist {
                list_type: "directory".to_string(),
                value: path.to_string_lossy().to_string(),
            },
            ctx
        );
    }

    pub fn add_to_command_denylist(
        &mut self,
        profile_id: ClientProfileId,
        predicate: &AgentModeCommandExecutionPredicate,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if !profile.command_denylist.contains(predicate) {
                    profile.command_denylist.push(predicate.clone());
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileAddedToDenylist {
                list_type: "command".to_string(),
                value: predicate.to_string(),
            },
            ctx
        );
    }

    pub fn remove_from_command_denylist(
        &mut self,
        profile_id: ClientProfileId,
        predicate: &AgentModeCommandExecutionPredicate,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                let original_len = profile.command_denylist.len();
                profile.command_denylist.retain(|p| p != predicate);
                profile.command_denylist.len() != original_len
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileRemovedFromDenylist {
                list_type: "command".to_string(),
                value: predicate.to_string(),
            },
            ctx
        );
    }

    pub fn add_to_mcp_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        id: &Uuid,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if !profile.mcp_allowlist.contains(id) {
                    profile.mcp_allowlist.push(*id);
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileAddedToAllowlist {
                list_type: "mcp".to_string(),
                value: id.to_string(),
            },
            ctx
        );
    }

    pub fn remove_from_mcp_allowlist(
        &mut self,
        profile_id: ClientProfileId,
        id: &Uuid,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                let original_len = profile.mcp_allowlist.len();
                profile.mcp_allowlist.retain(|p| p != id);
                profile.mcp_allowlist.len() != original_len
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileRemovedFromAllowlist {
                list_type: "mcp".to_string(),
                value: id.to_string(),
            },
            ctx
        );
    }

    pub fn add_to_mcp_denylist(
        &mut self,
        profile_id: ClientProfileId,
        id: &Uuid,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                if !profile.mcp_denylist.contains(id) {
                    profile.mcp_denylist.push(*id);
                    return true;
                }
                false
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileAddedToDenylist {
                list_type: "mcp".to_string(),
                value: id.to_string(),
            },
            ctx
        );
    }

    pub fn remove_from_mcp_denylist(
        &mut self,
        profile_id: ClientProfileId,
        id: &Uuid,
        ctx: &mut ModelContext<Self>,
    ) {
        self.edit_profile_internal(
            profile_id,
            |profile| {
                let original_len = profile.mcp_denylist.len();
                profile.mcp_denylist.retain(|p| p != id);
                profile.mcp_denylist.len() != original_len
            },
            ctx,
        );

        send_telemetry_from_ctx!(
            TelemetryEvent::AIExecutionProfileRemovedFromDenylist {
                list_type: "mcp".to_string(),
                value: id.to_string(),
            },
            ctx
        );
    }

    /// Edits an execution profile and persists the changed profile locally.
    /// Parameters:
    /// * `profile_id`: The id of the profile to edit
    /// * `edit_fn`: a closure that safely modifies the AIExecutionProfile. It should return `true` if the profile was changed, `false` otherwise. When `true`, it saves the changes locally, and otherwise exits early to prevent unnecessary persistence work.
    /// * `ctx`: The model context
    ///
    /// Returns `true` if the profile was actually changed and saved,
    /// `false` otherwise. Callers can use this to gate side effects such as
    /// telemetry on real changes.
    fn edit_profile_internal(
        &mut self,
        profile_id: ClientProfileId,
        edit_fn: impl FnOnce(&mut AIExecutionProfile) -> bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        // We don't yet support editing the default profile for the CLI.
        if let DefaultProfileState::Cli { id, .. } = &self.default_profile_state {
            if *id == profile_id {
                log::warn!("Attempted to edit CLI default profile, which is not yet supported.");
                return false;
            }
        }

        // The default profile starts in memory so a fresh install needs no
        // account or database seed. Persist it on its first edit.
        if let DefaultProfileState::Unsynced { id, profile } = &self.default_profile_state {
            if *id == profile_id {
                let mut new_profile = profile.clone();
                let value_changed = edit_fn(&mut new_profile);
                if !value_changed {
                    return false;
                }

                let sync_id = SyncId::ClientId(ClientId::new());
                self.default_profile_state = DefaultProfileState::Synced { id: profile_id };
                self.profile_id_to_sync_id.insert(profile_id, sync_id);
                LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
                    repository.create_execution_profile_with_id(sync_id, new_profile, ctx);
                });

                log::info!("Persisted the default execution profile locally: {profile_id:?}");
                ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(profile_id));
                return true;
            }
        }

        let mut value_changed = false;
        if let Some(sync_id) = self.profile_id_to_sync_id.get(&profile_id) {
            if let Some(object) = LocalObjectRepository::as_ref(ctx).execution_profile(sync_id, ctx)
            {
                let mut data = object.model().string_model.clone();
                value_changed = edit_fn(&mut data);
                if !value_changed {
                    return false;
                }
                LocalObjectRepository::handle(ctx).update(ctx, |repository, ctx| {
                    repository.update_execution_profile(*sync_id, data, ctx);
                });

                log::info!("Edited execution profile with id: {profile_id:?}");
            } else {
                log::error!("Profile id is mapped but no object found: {profile_id:?}");
            }
        }
        ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(profile_id));
        value_changed
    }

    fn handle_local_repository_event(
        &mut self,
        event: &LocalObjectRepositoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            LocalObjectRepositoryEvent::ExecutionProfiles { id: Some(sync_id) } => {
                if LocalObjectRepository::as_ref(ctx)
                    .execution_profile(sync_id, ctx)
                    .is_some()
                {
                    self.handle_execution_profile_upserted(*sync_id, ctx);
                } else {
                    self.handle_execution_profile_deleted(*sync_id, ctx);
                }
            }
            LocalObjectRepositoryEvent::ExecutionProfiles { id: None } => {
                self.reconcile_with_local_repository(ctx);
            }
            LocalObjectRepositoryEvent::Rules
            | LocalObjectRepositoryEvent::Notebooks { .. }
            | LocalObjectRepositoryEvent::NotebookContentEdited { .. }
            | LocalObjectRepositoryEvent::Workflows { .. } => {}
        }
    }

    fn reconcile_with_local_repository(&mut self, ctx: &mut ModelContext<Self>) {
        let profiles = LocalObjectRepository::as_ref(ctx).execution_profiles(ctx);
        let persisted_ids = profiles
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        let default_sync_id = profiles
            .iter()
            .find(|profile| profile.model().string_model.is_default_profile)
            .map(|profile| profile.id);

        if let DefaultProfileState::Unsynced { id, .. } = self.default_profile_state {
            if let Some(sync_id) = default_sync_id {
                self.default_profile_state = DefaultProfileState::Synced { id };
                self.profile_id_to_sync_id.insert(id, sync_id);
                ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(id));
            }
        }

        let removed_profile_ids = self
            .profile_id_to_sync_id
            .iter()
            .filter_map(|(profile_id, sync_id)| {
                (!persisted_ids.contains(sync_id)).then_some(*profile_id)
            })
            .collect::<Vec<_>>();
        for profile_id in removed_profile_ids {
            let sync_id = self.profile_id_to_sync_id[&profile_id];
            self.handle_execution_profile_deleted(sync_id, ctx);
        }

        for profile in profiles {
            if !profile.model().string_model.is_default_profile
                && !self
                    .profile_id_to_sync_id
                    .values()
                    .any(|sync_id| *sync_id == profile.id)
            {
                let profile_id = ClientProfileId::new();
                self.profile_id_to_sync_id.insert(profile_id, profile.id);
                ctx.emit(AIExecutionProfilesModelEvent::ProfileCreated);
            }
        }
    }

    fn handle_templatable_mcp_server_manager_event(
        &mut self,
        event: &TemplatableMCPServerManagerEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            TemplatableMCPServerManagerEvent::TemplatableMCPServersUpdated => {
                self.remove_deleted_mcp_servers(ctx);
            }
            TemplatableMCPServerManagerEvent::LegacyServerConverted
            | TemplatableMCPServerManagerEvent::StateChanged { uuid: _, state: _ }
            | TemplatableMCPServerManagerEvent::ServerInstallationAdded(_)
            | TemplatableMCPServerManagerEvent::ServerInstallationDeleted(_) => {}
        }
    }

    fn handle_execution_profile_upserted(&mut self, sync_id: SyncId, ctx: &mut ModelContext<Self>) {
        let Some(object) = LocalObjectRepository::as_ref(ctx).execution_profile(&sync_id, ctx)
        else {
            log::warn!(
                "Received an execution profile update but no local object was found: {sync_id:?}"
            );
            return;
        };

        if let Some(profile_id) = self.get_profile_id_by_sync_id(&sync_id) {
            ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(profile_id));
            return;
        }

        if object.model().string_model.is_default_profile {
            if matches!(self.default_profile_state, DefaultProfileState::Cli { .. }) {
                log::info!("Ignoring the persisted default profile in CLI mode: {sync_id:?}");
                return;
            }

            if let DefaultProfileState::Unsynced { id, .. } = self.default_profile_state {
                self.default_profile_state = DefaultProfileState::Synced { id };
                self.profile_id_to_sync_id.insert(id, sync_id);
                log::info!("Adopted the persisted default execution profile: {sync_id:?}");
                ctx.emit(AIExecutionProfilesModelEvent::ProfileUpdated(id));
            }

            return;
        }

        let profile_id = ClientProfileId::new();
        self.profile_id_to_sync_id.insert(profile_id, sync_id);
        log::info!("Added a local execution profile to the client map: {sync_id:?}");
        ctx.emit(AIExecutionProfilesModelEvent::ProfileCreated);
    }

    fn handle_execution_profile_deleted(&mut self, sync_id: SyncId, ctx: &mut ModelContext<Self>) {
        // Find and remove the profile from our map
        let profile_id = self
            .profile_id_to_sync_id
            .iter()
            .find_map(|(client_id, id)| {
                if *id == sync_id {
                    Some(*client_id)
                } else {
                    None
                }
            });

        if let Some(profile_id) = profile_id {
            self.profile_id_to_sync_id.remove(&profile_id);

            // Also remove from active profiles per session
            self.active_profiles_per_session
                .retain(|_, active_id| *active_id != profile_id);

            let is_default = matches!(&self.default_profile_state, DefaultProfileState::Synced { id } if *id == profile_id);
            if is_default {
                log::warn!(
                    "Default execution profile was deleted locally. Restoring in-memory defaults: {sync_id:?}"
                );
                self.default_profile_state = DefaultProfileState::Unsynced {
                    id: profile_id,
                    profile: AIExecutionProfile {
                        is_default_profile: true,
                        ..Default::default()
                    },
                };
            }

            log::info!("Removed local execution profile from the client map: {sync_id:?}");
            ctx.emit(AIExecutionProfilesModelEvent::ProfileDeleted);
        }
    }

    /// Handle deleted MCP servers by deleting its uuid from all profiles.
    fn remove_deleted_mcp_servers(&mut self, ctx: &mut ModelContext<Self>) {
        let all_valid_uuids = TemplatableMCPServerManager::get_all_cloud_synced_mcp_servers(ctx);
        for profile_id in self.get_all_profile_ids() {
            self.edit_profile_internal(
                profile_id,
                |profile| {
                    let original_allowlist_len = profile.mcp_allowlist.len();
                    let original_denylist_len = profile.mcp_denylist.len();
                    profile
                        .mcp_allowlist
                        .retain(|uuid| all_valid_uuids.contains_key(uuid));
                    profile
                        .mcp_denylist
                        .retain(|uuid| all_valid_uuids.contains_key(uuid));
                    profile.mcp_allowlist.len() != original_allowlist_len
                        || profile.mcp_denylist.len() != original_denylist_len
                },
                ctx,
            );
        }
    }

    // We don't want stale client ids in our map. We won't be able to find the backing cloud object when
    // an edit occurs.
    pub fn replace_client_id_with_server_id(&mut self, server_id: SyncId, client_id: SyncId) {
        for sync_id in self.profile_id_to_sync_id.values_mut() {
            if *sync_id == client_id {
                *sync_id = server_id;
                log::info!("Updated profile id mapping after creating a new execution profile");
            }
        }
    }

    /// Replaces the given profile's data with CLI defaults for the given sandboxed state.
    /// Use in tests to simulate the profile configuration used by the sandboxed CLI agent.
    #[cfg(test)]
    pub fn apply_cli_profile_defaults_for_test(
        &mut self,
        profile_id: ClientProfileId,
        is_sandboxed: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let cli_profile = AIExecutionProfile::create_default_cli_profile(is_sandboxed, None);
        self.edit_profile_internal(
            profile_id,
            move |profile| {
                *profile = cli_profile;
                true
            },
            ctx,
        );
    }
}

#[allow(clippy::enum_variant_names)]
pub enum AIExecutionProfilesModelEvent {
    ProfileUpdated(ClientProfileId),
    ProfileCreated,
    ProfileDeleted,
    UpdatedActiveProfile { terminal_view_id: EntityId },
}

impl Entity for AIExecutionProfilesModel {
    type Event = AIExecutionProfilesModelEvent;
}

impl SingletonEntity for AIExecutionProfilesModel {}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
