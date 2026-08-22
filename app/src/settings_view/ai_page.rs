use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use ::ai::api_keys::ApiKeyManager;
use chrono::{DateTime, Local};
use enum_iterator::all;
use galaxy_core::channel::ChannelState;
use galaxy_core::context_flag::ContextFlag;
use galaxy_core::features::FeatureFlag;
use galaxy_core::ui::color::contrast::MinimumAllowedContrast;
use galaxy_core::ui::color::ContrastingColor;
use galaxy_core::ui::theme::color::internal_colors;
use galaxy_core::ui::theme::Fill as ThemeFill;
use galaxy_editor::editor::NavigationKey;
use galaxyui::clipboard::ClipboardContent;
use galaxyui::elements::{
    Border, ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Dismiss, Empty, Expanded, Fill, Flex, FormattedTextElement, HighlightedHyperlink, Hoverable,
    HyperlinkLens, HyperlinkUrl, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
    Shrinkable, Stack, Text,
};
use galaxyui::fonts::{Properties, Weight};
use galaxyui::keymap::{ContextPredicate, FixedBinding, Keystroke};
use galaxyui::platform::Cursor;
use galaxyui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use galaxyui::ui_components::slider::SliderStateHandle;
use galaxyui::ui_components::switch::{SwitchStateHandle, TooltipConfig};
use galaxyui::{
    id, Action, AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};
use itertools::Itertools;
use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use pathfinder_geometry::vector::vec2f;
use regex::Regex;
use settings::{Setting, ToggleableSetting};
use strum::IntoEnumIterator;

use super::execution_profile_view::{ExecutionProfileView, ExecutionProfileViewEvent};
use super::provider_setup_view::{
    AcpProviderDraft, BedrockProviderDraft, ProviderSetupProviderType, ProviderSetupView,
    ProviderSetupViewEvent,
};
use super::settings_page::{
    build_sub_header, build_toggle_element, render_body_item_label,
    render_body_item_label_with_icon, render_custom_size_header, render_dropdown_item,
    render_dropdown_item_label, render_full_pane_width_ai_button, render_input_list,
    render_separator, render_settings_info_banner, InputListItem, LocalOnlyIconState, MatchData,
    PageType, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget, ToggleState,
    HEADER_PADDING, TOGGLE_BUTTON_RIGHT_PADDING,
};
use super::{
    editor_text_colors, flags, SettingActionPairContexts, SettingActionPairDescriptions,
    SettingsAction, SettingsSection, ToggleSettingActionPair,
};
#[cfg(not(target_family = "wasm"))]
use crate::ai::aws_credentials::refresh_aws_credentials;
use crate::ai::blocklist::agent_view::agent_input_footer::editor::{
    AgentToolbarEditorMode, AgentToolbarInlineEditor,
};
use crate::ai::blocklist::BlocklistAIPermissions;
#[cfg(not(target_family = "wasm"))]
use crate::ai::chatgpt_auth::{ChatGPTAuthModel, ChatGPTAuthModelEvent};
use crate::ai::execution_profiles::model_menu_items::available_model_menu_items;
#[cfg(not(target_family = "wasm"))]
use crate::ai::execution_profiles::profiles::{
    AIExecutionProfilesModel, AIExecutionProfilesModelEvent, ClientProfileId,
};
use crate::ai::execution_profiles::{
    long_context_pricing_warning_title, AIExecutionProfile, AIExecutionProfileAppExt,
    ActionPermission, WriteToPtyPermission,
};
use crate::ai::llms::{
    is_using_api_key_for_provider, LLMContextWindow, LLMId, LLMPreferences, LLMPreferencesEvent,
    LLMProvider,
};
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::ai::paths::host_native_absolute_path;
use crate::appearance::{Appearance, AppearanceEvent};
use crate::cloud_object::model::persistence::{CloudModel, CloudModelEvent};
use crate::cloud_object::GenericStringObjectFormat::Json;
use crate::cloud_object::{JsonObjectType, ObjectType};
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, InteractionState,
    PropagateAndNoOpNavigationKeys, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::menu::{MenuItem, MenuItemFields};
use crate::server::telemetry::{
    AgentModeAutoDetectionSettingOrigin, AutonomySettingToggleSource,
    ToggleCodeSuggestionsSettingSource,
};
use crate::settings::ai::OpenAIProviderKind;
use crate::settings::{
    AIAutoDetectionEnabled, AICommandDenylist, AISettings, AISettingsChangedEvent, AcpEnabled,
    AcpProviderConfig, AgentModeCodingPermissionsType, AgentModeCommandExecutionDenylist,
    AgentModeCommandExecutionPredicate, AgentModeQuerySuggestionsEnabled, BedrockAutoLogin,
    BedrockEnabled, BedrockModelConfig, CodeSettings, CodebaseContextEnabled, CrosscheckEnabled,
    FileBasedMcpEnabled, GitOperationsAutogenEnabled, IncludeAgentCommandsInHistory, InputSettings,
    IntelligentAutosuggestionsEnabled, LongRunningCommandSubmissionMode, MemoryEnabled,
    NLDInTerminalEnabled, NaturalLanguageAutosuggestionsEnabled, OpenAIModelConfig,
    OpenAIProviderConfig, OrchestrationMessageDisplayMode, PromptSubmissionMode,
    RuleSuggestionsEnabled, SharedBlockTitleGenerationEnabled, ShouldRenderCLIAgentToolbar,
    ShouldRenderUseAgentToolbarForUserCommands, ShowAgentTips, ShowConversationHistory,
    ShowHintText, ThinkingDisplayMode, VoiceInputEnabled, VoiceInputToggleKey,
    WarpDriveContextEnabled,
};
use crate::terminal::session_settings::{SessionSettings, SessionSettingsChangedEvent};
use crate::terminal::CLIAgent;
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon;
use crate::util::bindings;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, DangerSecondaryTheme, SecondaryTheme,
};
use crate::view_components::dropdown::DropdownAction;
use crate::view_components::{
    render_warning_box, Dropdown, DropdownItem, FilterableDropdown, SubmittableTextInput,
    SubmittableTextInputEvent, WarningBoxConfig,
};
use crate::workspace::ToastStack;
use crate::workspaces::user_workspaces::UserWorkspacesEvent;
use crate::workspaces::workspace::{AdminEnablementSetting, CustomerType};
use crate::{
    report_error, report_if_error, send_telemetry_from_ctx, TelemetryEvent, UserWorkspaces,
};

const CONTENT_FONT_SIZE: f32 = 12.;
const AI_SETTINGS_DROPDOWN_WIDTH: f32 = 250.;
const AI_SETTINGS_DROPDOWN_MAX_HEIGHT: f32 = 250.;
const CONTEXT_WINDOW_SLIDER_WIDTH: f32 = 220.;
const CONTEXT_WINDOW_INPUT_BOX_WIDTH: f32 = 120.;

const NEXT_COMMAND_DESCRIPTION: &str = "Let AI suggest the next command to run based on your command history, outputs, and common workflows.";
const PROMPT_SUGGESTIONS_DESCRIPTION: &str = "Let AI suggest natural language prompts, as inline banners in the input, based on recent commands and their outputs.";
const SUGGESTED_CODE_BANNERS_DESCRIPTION: &str = "Let AI suggest code diffs and queries as inline banners in the blocklist, based on recent commands and their outputs.";
const NATURAL_LANGUAGE_AUTOSUGGESTIONS: &str =
    "Let AI suggest natural language autosuggestions, based on recent commands and their outputs.";
const SHARED_BLOCK_TITLE_GENERATION_DESCRIPTION: &str =
    "Let AI generate a title for your shared block based on the command and output.";
const GIT_OPERATIONS_AUTOGEN_DESCRIPTION: &str =
    "Let AI generate commit messages and pull request titles and descriptions.";
const WISPR_FLOW_URL: &str = "https://wisprflow.ai/";

/// Identifies which subpage of the AI settings the user is viewing.
/// When `None`, the page shows all widgets (legacy/full view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AISubpage {
    /// The main Galaxy Agent page: suggestions, input, and other agent settings.
    WarpAgent,
    /// Agent profiles and permissions.
    Profiles,
    /// Knowledge / Rules settings.
    Knowledge,
    /// Third-party CLI agent settings.
    ThirdPartyCLIAgents,
    /// Unified model and provider configuration.
    Models,
    /// OpenAI provider settings.
    ProviderOpenAI,
    /// LiteLLM provider settings.
    ProviderLiteLLM,
    /// ChatGPT subscription provider settings.
    ProviderChatGPTSubscription,
    /// AWS Bedrock provider settings.
    ProviderBedrock,
    /// ACP provider settings.
    ProviderACP,
    /// Experimental features.
    Experiments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InlineProviderSetupState {
    provider_type: ProviderSetupProviderType,
}

impl AISubpage {
    pub fn from_section(section: SettingsSection) -> Option<Self> {
        match section {
            SettingsSection::WarpAgent => Some(Self::WarpAgent),
            SettingsSection::AgentProfiles => Some(Self::Profiles),
            SettingsSection::Knowledge => Some(Self::Knowledge),
            SettingsSection::ThirdPartyCLIAgents => Some(Self::ThirdPartyCLIAgents),
            SettingsSection::Models => Some(Self::Models),
            SettingsSection::ProviderOpenAI => Some(Self::ProviderOpenAI),
            SettingsSection::ProviderLiteLLM => Some(Self::ProviderLiteLLM),
            SettingsSection::ProviderChatGPTSubscription => Some(Self::ProviderChatGPTSubscription),
            SettingsSection::ProviderBedrock => Some(Self::ProviderBedrock),
            SettingsSection::ProviderACP => Some(Self::ProviderACP),
            SettingsSection::Experiments => Some(Self::Experiments),
            // AgentMCPServers renders the standalone MCPServers page, not an AI subpage.
            _ => None,
        }
    }
}

pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "Agent Client Protocol",
            builder(SettingsAction::AI(AISettingsPageAction::ToggleAcpEnabled)),
            context,
            flags::ACP_ENABLED_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| cfg!(unix) && FeatureFlag::AgentClientProtocol.is_enabled())],
        app,
    );

    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            if FeatureFlag::AgentView.is_enabled() {
                "terminal command autodetection in agent input"
            } else {
                "natural language detection"
            },
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleAIInputAutoDetection,
            )),
            &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
            flags::AI_INPUT_AUTODETECTION_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| FeatureFlag::AgentMode.is_enabled())],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "agent prompt autodetection in terminal input",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleNLDInTerminal,
            )),
            &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
            flags::NLD_IN_TERMINAL_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| FeatureFlag::AgentView.is_enabled())],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "Next Command",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleIntelligentAutosuggestions,
            )),
            &(context.clone() & id!(flags::IS_ACTIVE_AI_ENABLED)),
            flags::INTELLIGENT_AUTOSUGGESTIONS_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "prompt suggestions",
            builder(SettingsAction::AI(
                AISettingsPageAction::TogglePromptSuggestions,
            )),
            &(context.clone() & id!(flags::IS_ACTIVE_AI_ENABLED)),
            flags::PROMPT_SUGGESTIONS_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "code suggestions",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleCodeSuggestions,
            )),
            &(context.clone()
                & id!(flags::IS_ACTIVE_AI_ENABLED)
                & id!(flags::PROMPT_SUGGESTIONS_FLAG)),
            flags::CODE_SUGGESTIONS_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::custom(
            SettingActionPairDescriptions::new("Show agent tips", "Hide agent tips"),
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleShowAgentTips,
            )),
            SettingActionPairContexts::new(
                context.clone() & id!(flags::IS_ANY_AI_ENABLED) & !id!(flags::SHOW_AGENT_TIPS_FLAG),
                context.clone() & id!(flags::IS_ANY_AI_ENABLED) & id!(flags::SHOW_AGENT_TIPS_FLAG),
            ),
            None,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| FeatureFlag::AgentTips.is_enabled())],
        app,
    );
    {
        use galaxyui::keymap::FixedBinding;

        use crate::settings::ThinkingDisplayMode;

        let ai_context = context.clone() & id!(flags::IS_ANY_AI_ENABLED);
        let mode_bindings: Vec<FixedBinding> = ThinkingDisplayMode::iter()
            .map(|mode| {
                let context_flag = match mode {
                    ThinkingDisplayMode::ShowAndCollapse => {
                        flags::THINKING_DISPLAY_SHOW_AND_COLLAPSE
                    }
                    ThinkingDisplayMode::AlwaysShow => flags::THINKING_DISPLAY_ALWAYS_SHOW,
                    ThinkingDisplayMode::NeverShow => flags::THINKING_DISPLAY_NEVER_SHOW,
                };
                FixedBinding::empty(
                    mode.command_palette_description(),
                    builder(SettingsAction::AI(
                        AISettingsPageAction::SetThinkingDisplayMode(mode),
                    )),
                    ai_context.clone() & !id!(context_flag),
                )
                .with_group(bindings::BindingGroup::WarpAi.as_str())
            })
            .collect();
        app.register_fixed_bindings(mode_bindings);
    }
    {
        use warpui::keymap::FixedBinding;

        let ai_context = context.clone() & id!(flags::IS_ANY_AI_ENABLED);
        let mode_bindings: Vec<FixedBinding> = OrchestrationMessageDisplayMode::iter()
            .map(|mode| {
                let context_flag = match mode {
                    OrchestrationMessageDisplayMode::ShowAndCollapse => {
                        flags::ORCHESTRATION_MESSAGE_DISPLAY_SHOW_AND_COLLAPSE
                    }
                    OrchestrationMessageDisplayMode::AlwaysShow => {
                        flags::ORCHESTRATION_MESSAGE_DISPLAY_ALWAYS_SHOW
                    }
                    OrchestrationMessageDisplayMode::AlwaysCollapse => {
                        flags::ORCHESTRATION_MESSAGE_DISPLAY_ALWAYS_COLLAPSE
                    }
                };
                FixedBinding::empty(
                    mode.command_palette_description(),
                    builder(SettingsAction::AI(
                        AISettingsPageAction::SetOrchestrationMessageDisplayMode(mode),
                    )),
                    ai_context.clone() & !id!(context_flag),
                )
                .with_group(bindings::BindingGroup::WarpAi.as_str())
            })
            .collect();
        app.register_fixed_bindings(mode_bindings);
    }
    if FeatureFlag::QueueSlashCommand.is_enabled() {
        let ai_context = context.clone() & id!(flags::IS_ANY_AI_ENABLED);
        let mode_bindings: Vec<FixedBinding> = PromptSubmissionMode::iter()
            .map(|mode| {
                let context_flag = match mode {
                    PromptSubmissionMode::Interrupt => flags::PROMPT_SUBMISSION_INTERRUPT,
                    PromptSubmissionMode::Queue => flags::PROMPT_SUBMISSION_QUEUE,
                };
                FixedBinding::empty(
                    mode.command_palette_description(),
                    builder(SettingsAction::AI(
                        AISettingsPageAction::SetPromptSubmissionMode(mode),
                    )),
                    ai_context.clone() & !id!(context_flag),
                )
                .with_group(bindings::BindingGroup::WarpAi.as_str())
            })
            .collect();
        app.register_fixed_bindings(mode_bindings);

        // The LRC submission mode only applies (and is only shown) when the default
        // prompt submission mode is Interrupt, so its palette entries are gated on it.
        let lrc_mode_bindings: Vec<FixedBinding> = LongRunningCommandSubmissionMode::iter()
            .map(|mode| {
                let context_flag = match mode {
                    LongRunningCommandSubmissionMode::SendImmediately => {
                        flags::LRC_SUBMISSION_SEND_IMMEDIATELY
                    }
                    LongRunningCommandSubmissionMode::QueueUntilCommandCompletes => {
                        flags::LRC_SUBMISSION_QUEUE_UNTIL_COMMAND_COMPLETES
                    }
                };
                FixedBinding::empty(
                    mode.command_palette_description(),
                    builder(SettingsAction::AI(
                        AISettingsPageAction::SetLongRunningCommandSubmissionMode(mode),
                    )),
                    ai_context.clone()
                        & id!(flags::PROMPT_SUBMISSION_INTERRUPT)
                        & !id!(context_flag),
                )
                .with_group(bindings::BindingGroup::WarpAi.as_str())
            })
            .collect();
        app.register_fixed_bindings(lrc_mode_bindings);
    }
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "natural language autosuggestions",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleNaturalLanguageAutosuggestions,
            )),
            &(context.clone() & id!(flags::IS_ACTIVE_AI_ENABLED)),
            flags::NATURAL_LANGUAGE_AUTOSUGGESTIONS_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| FeatureFlag::PredictAMQueries.is_enabled())],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "shared block title generation",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleSharedTitleGeneration,
            )),
            &(context.clone() & id!(flags::IS_ACTIVE_AI_ENABLED)),
            flags::SHARED_BLOCK_TITLE_GENERATION_FLAG,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| FeatureFlag::SharedBlockTitleGeneration.is_enabled())],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "commit and pull request generation",
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleGitOperationsAutogen,
            )),
            &(context.clone() & id!(flags::IS_ACTIVE_AI_ENABLED)),
            flags::GIT_OPERATIONS_AUTOGEN_FLAG,
        )
        .with_enabled(|| FeatureFlag::GitOperationsInCodeReview.is_enabled())
        .is_supported_on_current_platform(
            AISettings::as_ref(app)
                .git_operations_autogen_enabled_internal
                .is_supported_on_current_platform()
                && UserWorkspaces::as_ref(app).is_git_operations_ai_enabled(),
        )],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::new(
            "voice input",
            builder(SettingsAction::AI(AISettingsPageAction::ToggleVoiceInput)),
            &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
            flags::IS_VOICE_INPUT_ENABLED,
        )
        .with_group(bindings::BindingGroup::WarpAi)
        .with_enabled(|| cfg!(feature = "voice_input"))],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![ToggleSettingActionPair::custom(
            SettingActionPairDescriptions::new(
                "Show \"Use Agent\" footer",
                "Hide \"Use Agent\" footer",
            ),
            builder(SettingsAction::AI(
                AISettingsPageAction::ToggleUseAgentToolbar,
            )),
            SettingActionPairContexts::new(
                context.clone()
                    & id!(flags::IS_ANY_AI_ENABLED)
                    & !id!(flags::USE_AGENT_FOOTER_FLAG),
                context.clone() & id!(flags::IS_ANY_AI_ENABLED) & id!(flags::USE_AGENT_FOOTER_FLAG),
            ),
            None,
        )
        .with_group(bindings::BindingGroup::WarpAi)],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![
            ToggleSettingActionPair::new(
                "include agent-executed commands in history",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleIncludeAgentCommandsInHistory,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::INCLUDE_AGENT_COMMANDS_IN_HISTORY_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi),
            ToggleSettingActionPair::new(
                "conversation history in tools panel",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleShowConversationHistory,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::SHOW_CONVERSATION_HISTORY,
            )
            .with_group(bindings::BindingGroup::WarpAi),
            ToggleSettingActionPair::new(
                "model picker in prompt",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleShowBaseModelPickerInPrompt,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::SHOW_BASE_MODEL_PICKER_IN_PROMPT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi),
            ToggleSettingActionPair::new(
                "coding agent toolbar",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleCLIAgentToolbar,
                )),
                context,
                flags::CLI_AGENT_FOOTER_ENABLED,
            )
            .with_group(bindings::BindingGroup::WarpAi),
        ],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![
            ToggleSettingActionPair::new(
                "Rules",
                builder(SettingsAction::AI(AISettingsPageAction::ToggleRules)),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::AI_RULES_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::AIRules.is_enabled()),
            ToggleSettingActionPair::new(
                "Suggested Rules",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleRuleSuggestions,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::SUGGESTED_RULES_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| {
                FeatureFlag::AIRules.is_enabled() && FeatureFlag::SuggestedRules.is_enabled()
            }),
            ToggleSettingActionPair::new(
                "Galaxy Drive as agent context",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleGalaxyDriveContext,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::WARP_DRIVE_CONTEXT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::AIRules.is_enabled()),
            ToggleSettingActionPair::new(
                "Auto-spawn servers from third-party agents",
                builder(SettingsAction::AI(AISettingsPageAction::ToggleFileBasedMcp)),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::FILE_BASED_MCP_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| {
                FeatureFlag::McpServer.is_enabled()
                    && FeatureFlag::FileBasedMcp.is_enabled()
                    && ContextFlag::ShowMCPServers.is_enabled()
            }),
        ],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![
            ToggleSettingActionPair::new(
                "auto show or hide Rich Input based on agent status",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleAutoToggleRichInput,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_TOGGLE_RICH_INPUT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
            ToggleSettingActionPair::new(
                "auto open Rich Input when a coding agent session starts",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_OPEN_RICH_INPUT_ON_CLI_AGENT_START_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
            ToggleSettingActionPair::new(
                "auto dismiss Rich Input after prompt submission",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleAutoDismissRichInputAfterSubmit,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_DISMISS_RICH_INPUT_AFTER_SUBMIT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
        ],
        app,
    );
    if !FeatureFlag::FullSourceCodeEmbedding.is_enabled() {
        ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
            vec![ToggleSettingActionPair::new(
                "codebase index",
                builder(SettingsAction::AI(
                    AISettingsPageAction::ToggleCodebaseContext,
                )),
                &(context.clone() & id!(flags::IS_ANY_AI_ENABLED)),
                flags::IS_CODEBASE_INDEXING_ENABLED,
            )],
            app,
        );
    }
}

pub struct AISettingsPageView {
    page: PageType<Self>,
    active_subpage: Option<AISubpage>,
    voice_input_toggle_key_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
    autodetection_denylist_editor: ViewHandle<EditorView>,
    autonomy_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,

    code_read_autonomy_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,

    code_read_allowlist_editor: ViewHandle<SubmittableTextInput>,
    code_read_allowlist_mouse_state_handles: Vec<MouseStateHandle>,

    command_execution_allowlist_editor: ViewHandle<SubmittableTextInput>,
    command_execution_allowlist_mouse_state_handles: Vec<MouseStateHandle>,
    command_execution_denylist_editor: ViewHandle<SubmittableTextInput>,
    command_execution_denylist_mouse_state_handles: Vec<MouseStateHandle>,
    cli_agent_footer_command_editor: ViewHandle<SubmittableTextInput>,
    cli_agent_footer_command_mouse_state_handles: Vec<MouseStateHandle>,
    cli_agent_footer_command_agent_dropdowns: Vec<ViewHandle<Dropdown<AISettingsPageAction>>>,
    agent_toolbar_inline_editor: ViewHandle<AgentToolbarInlineEditor>,
    cli_agent_toolbar_inline_editor: ViewHandle<AgentToolbarInlineEditor>,

    apply_code_diffs_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,
    read_files_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,
    execute_commands_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,
    write_to_pty_autonomy_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,
    mcp_permissions_dropdown_menu: ViewHandle<Dropdown<AISettingsPageAction>>,

    // Allowlisting directories (default profile)
    directory_allowlist_mouse_state_handles: Vec<MouseStateHandle>,
    directory_allowlist_editor: ViewHandle<SubmittableTextInput>,

    // Allowlisting commands (default profile)
    command_allowlist_mouse_state_handles: Vec<MouseStateHandle>,
    command_allowlist_editor: ViewHandle<SubmittableTextInput>,

    // Denylisting commands (default profile)
    command_denylist_mouse_state_handles: Vec<MouseStateHandle>,
    command_denylist_tooltip_mouse_state_handles: Vec<MouseStateHandle>,
    command_denylist_editor: ViewHandle<SubmittableTextInput>,

    mcp_allowlist_mouse_state_handles: Vec<MouseStateHandle>,
    mcp_allowlist_dropdown: ViewHandle<FilterableDropdown<AISettingsPageAction>>,

    mcp_denylist_mouse_state_handles: Vec<MouseStateHandle>,
    mcp_denylist_dropdown: ViewHandle<FilterableDropdown<AISettingsPageAction>>,

    base_model_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    coding_model_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,

    context_window_slider_state: SliderStateHandle,
    context_window_editor: ViewHandle<EditorView>,
    last_synced_context_window_editor_value: Option<u32>,
    dragged_context_window_value: Option<u32>,

    thinking_display_mode_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    orchestration_message_display_mode_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    default_prompt_submission_mode_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    lrc_submission_mode_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,
    #[cfg(feature = "local_fs")]
    conversation_layout_dropdown: ViewHandle<Dropdown<AISettingsPageAction>>,

    // Profile views
    profile_views: Vec<ViewHandle<ExecutionProfileView>>,
    add_profile_button: ViewHandle<ActionButton>,
    provider_setup_body: ViewHandle<ProviderSetupView>,
    inline_provider_setup: Option<InlineProviderSetupState>,

    // Custom model router views (gated on FeatureFlag::CustomModelRouters)
    #[cfg(feature = "local_fs")]
    router_views: Vec<ViewHandle<super::custom_router_view::CustomRouterView>>,
    #[cfg(feature = "local_fs")]
    add_router_button: ViewHandle<ActionButton>,
}

impl AISettingsPageView {
    #[cfg(not(target_family = "wasm"))]
    fn refresh_acp_discovery_for_draft(
        &mut self,
        draft: &AcpProviderDraft,
        ctx: &mut ViewContext<Self>,
    ) {
        let config = match crate::ai::acp::AcpRuntimeModel::discovery_config_for_values(
            &draft.agent_id,
            &draft.command,
            &draft.args,
        ) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("Could not resolve ACP launch configuration for discovery: {error}");
                self.provider_setup_body.update(ctx, |body, ctx| {
                    body.finish_acp_discovery(Err(error), Vec::new(), ctx);
                });
                return;
            }
        };
        self.start_acp_discovery(config, draft.agent_id.clone(), ctx);
    }

    #[cfg(not(target_family = "wasm"))]
    fn start_acp_discovery(
        &mut self,
        config: galaxy_acp::AcpManagerConfig,
        agent_id: String,
        ctx: &mut ViewContext<Self>,
    ) {
        let provider_setup_body = self.provider_setup_body.clone();
        /*
         * The settings borrow must end before updating the runtime singleton.
         */
        let manager =
            match crate::ai::acp::AcpRuntimeModel::handle(ctx).update(ctx, |runtime, ctx| {
                runtime.begin_discovery(ctx);
                runtime.manager(config)
            }) {
                Ok(manager) => manager,
                Err(error) => {
                    log::warn!("Could not start ACP discovery: {error}");
                    let error_text = error.to_string();
                    provider_setup_body.update(ctx, |body, ctx| {
                        body.finish_acp_discovery(Err(error_text), Vec::new(), ctx);
                    });
                    return;
                }
            };
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        let _ = ctx.spawn(
            async move { manager.discover_config_options(cwd, Vec::new()).await },
            move |_me, result, ctx| {
                match result {
                    Ok(options) => {
                        let option_count = options.len();
                        let config_options =
                            crate::ai::acp::AcpRuntimeModel::normalize_config_options(
                                options.clone(),
                            );
                        AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            if let Err(error) =
                                crate::ai::acp::AcpRuntimeModel::upsert_agent_settings(
                                    settings, &agent_id, options, ctx,
                                )
                            {
                                log::warn!("Failed to persist ACP discovery: {error}");
                            }
                            if let Err(error) =
                                crate::ai::acp::AcpRuntimeModel::mark_discovery_success(
                                    settings,
                                    &agent_id,
                                    option_count,
                                    ctx,
                                )
                            {
                                log::warn!("Failed to persist ACP discovery metadata: {error}");
                            }
                        });
                        crate::ai::acp::AcpRuntimeModel::handle(ctx).update(ctx, |runtime, ctx| {
                            runtime.finish_discovery_success(option_count, ctx);
                        });
                        provider_setup_body.update(ctx, |body, ctx| {
                            body.finish_acp_discovery(Ok(()), config_options, ctx);
                        });
                    }
                    Err(error) => {
                        log::warn!("ACP discovery failed: {error}");
                        let error_text = error.to_string();
                        AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            if let Err(error) =
                                crate::ai::acp::AcpRuntimeModel::mark_discovery_failure(
                                    settings,
                                    &agent_id,
                                    error_text.clone(),
                                    ctx,
                                )
                            {
                                log::warn!("Failed to persist ACP discovery failure: {error}");
                            }
                        });
                        crate::ai::acp::AcpRuntimeModel::handle(ctx).update(ctx, |runtime, ctx| {
                            runtime.finish_discovery_failure(error_text.clone(), ctx);
                        });
                        provider_setup_body.update(ctx, |body, ctx| {
                            body.finish_acp_discovery(Err(error_text), Vec::new(), ctx);
                        });
                    }
                }
                ctx.notify();
            },
        );
    }

    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let is_any_ai_enabled = AISettings::as_ref(ctx).is_any_ai_enabled(ctx);

        let workspace = UserWorkspaces::handle(ctx);
        let ai_autonomy_settings = workspace.as_ref(ctx).ai_autonomy_settings();
        ctx.subscribe_to_model(&workspace, |me, workspace, event, ctx| {
            if let UserWorkspacesEvent::TeamsChanged = event {
                me.refresh_all_execution_profile_ui(ctx);
                me.reset_execution_profile_mouse_state_handles(ctx);

                let is_any_ai_enabled = AISettings::as_ref(ctx).is_any_ai_enabled(ctx);
                let ai_autonomy_settings = workspace.as_ref(ctx).ai_autonomy_settings();

                Self::update_editor_interaction_state(
                    me.command_denylist_editor.as_ref(ctx).editor().clone(),
                    is_any_ai_enabled,
                    ctx,
                );

                Self::update_editor_interaction_state(
                    me.command_allowlist_editor.as_ref(ctx).editor().clone(),
                    is_any_ai_enabled
                        && !ai_autonomy_settings.has_override_for_execute_commands_allowlist(),
                    ctx,
                );

                Self::update_editor_interaction_state(
                    me.directory_allowlist_editor.as_ref(ctx).editor().clone(),
                    is_any_ai_enabled
                        && !ai_autonomy_settings.has_override_for_read_files_allowlist(),
                    ctx,
                );

                ctx.notify();
            }
        });

        let voice_input_toggle_key_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            if !AISettings::as_ref(ctx).is_voice_input_enabled(ctx) {
                dropdown.set_disabled(ctx);
            }

            let values = VoiceInputToggleKey::all_possible_values();
            let current_value = AISettings::as_ref(ctx).voice_input_toggle_key.value();
            let selected_index = values
                .iter()
                .position(|val| val == current_value)
                .unwrap_or_else(|| {
                    log::warn!(
                        "Could not find current VoiceInputToggleKey value in dropdown option list"
                    );
                    0
                });

            dropdown.add_items(
                values
                    .into_iter()
                    .map(|val| {
                        DropdownItem::new(
                            val.display_name(),
                            AISettingsPageAction::SetVoiceInputToggleKey(val),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown.set_selected_by_index(selected_index, ctx);

            dropdown
        });

        let coding_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);
            dropdown
        });
        Self::refresh_coding_model_menu(&coding_model_dropdown, ctx);

        let base_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);

            dropdown
        });
        Self::refresh_base_model_menu(&base_model_dropdown, ctx);

        let initial_context_window_value = Self::initial_context_window_value(ctx);
        let clamped_initial = Self::configurable_context_window(ctx)
            .map(|cw| initial_context_window_value.clamp(cw.min, cw.max))
            .unwrap_or(initial_context_window_value);
        let context_window_slider_state = SliderStateHandle::default();

        let context_window_editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(Appearance::as_ref(ctx).ui_font_size()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_buffer_text(&clamped_initial.to_string(), ctx);
            editor
        });
        ctx.subscribe_to_view(&context_window_editor, |me, _, event, ctx| {
            me.handle_context_window_editor_event(event, ctx);
        });
        let last_synced_context_window_editor_value = Some(clamped_initial);

        let thinking_display_mode_dropdown =
            OtherAIWidget::create_thinking_display_mode_dropdown(ctx);
        // Set initial selection based on current setting value.
        {
            let current_mode = AISettings::as_ref(ctx).thinking_display_mode;
            thinking_display_mode_dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    AISettingsPageAction::SetThinkingDisplayMode(current_mode),
                    ctx,
                );
            });
        }
        let orchestration_message_display_mode_dropdown =
            OtherAIWidget::create_orchestration_message_display_mode_dropdown(ctx);
        {
            let current_mode = AISettings::as_ref(ctx).orchestration_message_display_mode;
            orchestration_message_display_mode_dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    AISettingsPageAction::SetOrchestrationMessageDisplayMode(current_mode),
                    ctx,
                );
            });
        }

        let default_prompt_submission_mode_dropdown =
            OtherAIWidget::create_default_prompt_submission_mode_dropdown(ctx);
        {
            let current_mode = AISettings::as_ref(ctx).default_prompt_submission_mode;
            default_prompt_submission_mode_dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    AISettingsPageAction::SetPromptSubmissionMode(current_mode),
                    ctx,
                );
            });
        }

        let lrc_submission_mode_dropdown = OtherAIWidget::create_lrc_submission_mode_dropdown(ctx);
        {
            let current_mode = AISettings::as_ref(ctx).long_running_command_submission_mode;
            lrc_submission_mode_dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    AISettingsPageAction::SetLongRunningCommandSubmissionMode(current_mode),
                    ctx,
                );
            });
        }

        let autonomy_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown
        });
        Self::refresh_autonomy_dropdown_menu(&autonomy_dropdown_menu, ctx);

        let code_read_autonomy_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown
        });
        Self::refresh_code_read_autonomy_dropdown_menu(&code_read_autonomy_dropdown_menu, ctx);

        // While the data model supports arbitrary files in the allowlist,
        // it's most intuitive to allowlist whole directories.
        let code_read_allowlist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx).validate_on_submit(|s| {
                let expanded = host_native_absolute_path(s, &None, &None);
                Path::new(&expanded).is_dir()
            });
            input.set_placeholder_text("e.g. ~/code-repos/repo", ctx);
            input
        });
        Self::update_editor_interaction_state(
            code_read_allowlist_editor.as_ref(ctx).editor().clone(),
            is_any_ai_enabled,
            ctx,
        );

        ctx.subscribe_to_view(&code_read_allowlist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let expanded = host_native_absolute_path(s, &None, &None);
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(
                        model.add_filepath_to_code_read_allowlist(PathBuf::from(expanded), ctx)
                    );
                });
            }
        });

        let autodetection_denylist_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                autogrow: true,
                soft_wrap: true,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    text_colors_override: Some(TextColors {
                        default_color: appearance.theme().active_ui_text_color(),
                        disabled_color: appearance.theme().disabled_ui_text_color(),
                        hint_color: appearance.theme().disabled_ui_text_color(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);

            editor.set_placeholder_text("Commands, comma separated", ctx);

            let current_value = AISettings::as_ref(ctx)
                .autodetection_command_denylist
                .value()
                .clone();
            editor.set_buffer_text(current_value.as_str(), ctx);
            editor
        });
        Self::update_editor_interaction_state(
            autodetection_denylist_editor.clone(),
            is_any_ai_enabled,
            ctx,
        );

        ctx.subscribe_to_view(&autodetection_denylist_editor, move |me, _, event, ctx| {
            me.handle_detection_denylist_editor_event(event, ctx);
        });

        let command_execution_allowlist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. ls .*", ctx);
            input
        });
        Self::update_editor_interaction_state(
            command_execution_allowlist_editor
                .as_ref(ctx)
                .editor()
                .clone(),
            is_any_ai_enabled,
            ctx,
        );

        ctx.subscribe_to_view(&command_execution_allowlist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let predicate = match AgentModeCommandExecutionPredicate::new_regex(s) {
                    Ok(regex) => regex,
                    Err(e) => {
                        log::warn!(
                            "Failed to convert string to regex for cmd execution allowlist: {e}"
                        );
                        return;
                    }
                };
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(model.add_command_to_autoexecution_allowlist(predicate, ctx));
                })
            }
        });

        let command_execution_denylist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. rm .*", ctx);
            input
        });
        Self::update_editor_interaction_state(
            command_execution_denylist_editor
                .as_ref(ctx)
                .editor()
                .clone(),
            is_any_ai_enabled,
            ctx,
        );

        ctx.subscribe_to_view(&command_execution_denylist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let predicate = match AgentModeCommandExecutionPredicate::new_regex(s) {
                    Ok(regex) => regex,
                    Err(e) => {
                        log::warn!(
                            "Failed to convert string to regex for cmd execution denylist: {e}"
                        );
                        return;
                    }
                };
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(model.add_command_to_autoexecution_denylist(predicate, ctx));
                })
            }
        });

        let cli_agent_footer_command_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });
        // The coding agent footer command editor is always enabled,
        // independent of the global AI toggle, because it controls
        // third-party coding agents rather than Warp's own AI.
        Self::update_editor_interaction_state(
            cli_agent_footer_command_editor.as_ref(ctx).editor().clone(),
            true,
            ctx,
        );
        ctx.subscribe_to_view(
            &cli_agent_footer_command_editor,
            |_, _, event, ctx| match event {
                SubmittableTextInputEvent::Submit(command) => {
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        settings.add_cli_agent_footer_enabled_command(command, ctx);
                    });
                }
                SubmittableTextInputEvent::Escape => ctx.emit(AISettingsPageEvent::FocusSearch),
            },
        );

        ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |me, _handle, _event, ctx| {
            // Re-render if teams-related data changed that may affect whether features such as voice input are enabled.
            Self::refresh_base_model_menu(&me.base_model_dropdown, ctx);
            Self::refresh_coding_model_menu(&me.coding_model_dropdown, ctx);
            ctx.notify();
        });

        ctx.subscribe_to_model(
            &AIExecutionProfilesModel::handle(ctx),
            |me, _, event, ctx| {
                match event {
                    AIExecutionProfilesModelEvent::ProfileCreated
                    | AIExecutionProfilesModelEvent::ProfileDeleted => {
                        me.refresh_profile_views(ctx);
                    }
                    AIExecutionProfilesModelEvent::ProfileUpdated(_) => {
                        me.refresh_all_execution_profile_ui(ctx);
                        me.reset_execution_profile_mouse_state_handles(ctx);
                        me.sync_context_window_editor(ctx, false);
                    }
                    AIExecutionProfilesModelEvent::UpdatedActiveProfile { .. } => (),
                }
                ctx.notify();
            },
        );

        let cloud_model = CloudModel::handle(ctx);
        ctx.subscribe_to_model(&cloud_model, |me, _, event, ctx| {
            let added_or_deleted_mcp_servers = matches!(
                event,
                CloudModelEvent::ObjectCreated { type_and_id } | CloudModelEvent::ObjectDeleted { type_and_id, .. }
                if matches!(
                    type_and_id.object_type(),
                    ObjectType::GenericStringObject(Json(JsonObjectType::MCPServer))
                )
            );

            if added_or_deleted_mcp_servers {
                Self::refresh_mcp_allowlist_dropdown(&me.mcp_allowlist_dropdown, ctx);
                Self::refresh_mcp_denylist_dropdown(&me.mcp_denylist_dropdown, ctx);
                ctx.notify();
            }
        });

        let templatable_manager = TemplatableMCPServerManager::handle(ctx);
        ctx.subscribe_to_model(&templatable_manager, |me, _, _event, ctx| {
            Self::refresh_mcp_allowlist_dropdown(&me.mcp_allowlist_dropdown, ctx);
            Self::refresh_mcp_denylist_dropdown(&me.mcp_denylist_dropdown, ctx);
            ctx.notify();
        });

        ctx.subscribe_to_model(
            &LLMPreferences::handle(ctx),
            |me, _, event, ctx| match event {
                LLMPreferencesEvent::UpdatedAvailableLLMs => {
                    Self::refresh_base_model_menu(&me.base_model_dropdown, ctx);
                    Self::refresh_coding_model_menu(&me.coding_model_dropdown, ctx);
                    me.sync_context_window_editor(ctx, false);
                }
                LLMPreferencesEvent::UpdatedActiveAgentModeLLM => {
                    Self::refresh_base_model_menu(&me.base_model_dropdown, ctx);
                    me.sync_context_window_editor(ctx, false);
                }
                LLMPreferencesEvent::UpdatedActiveCodingLLM => {
                    Self::refresh_coding_model_menu(&me.coding_model_dropdown, ctx);
                }
            },
        );

        // Refresh model dropdowns when Bedrock credentials update so key icons reflect latest state.
        ctx.subscribe_to_model(&ApiKeyManager::handle(ctx), |me, _model, _event, ctx| {
            Self::refresh_base_model_menu(&me.base_model_dropdown, ctx);
            Self::refresh_coding_model_menu(&me.coding_model_dropdown, ctx);
            me.sync_context_window_editor(ctx, false);
            ctx.notify();
        });

        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            match event {
                AISettingsChangedEvent::AICommandDenylist { .. } => {
                    me.autodetection_denylist_editor.update(ctx, |editor, ctx| {
                        let denylist_value = &AISettings::as_ref(ctx)
                            .autodetection_command_denylist
                            .value()
                            .clone();
                        editor.set_buffer_text(denylist_value, ctx);
                    });
                }
                AISettingsChangedEvent::IsAnyAIEnabled { .. } => {
                    let is_enabled = AISettings::as_ref(ctx).is_any_ai_enabled(ctx);
                    let ai_autonomy_settings = UserWorkspaces::as_ref(ctx).ai_autonomy_settings();

                    Self::update_editor_interaction_state(
                        me.autodetection_denylist_editor.clone(),
                        is_enabled,
                        ctx,
                    );
                    Self::update_editor_interaction_state(
                        me.command_execution_allowlist_editor
                            .as_ref(ctx)
                            .editor()
                            .clone(),
                        is_enabled,
                        ctx,
                    );
                    Self::update_editor_interaction_state(
                        me.command_execution_denylist_editor
                            .as_ref(ctx)
                            .editor()
                            .clone(),
                        is_enabled,
                        ctx,
                    );
                    Self::update_editor_interaction_state(
                        me.code_read_allowlist_editor.as_ref(ctx).editor().clone(),
                        is_enabled,
                        ctx,
                    );

                    Self::update_editor_interaction_state(
                        me.directory_allowlist_editor.as_ref(ctx).editor().clone(),
                        is_enabled && !ai_autonomy_settings.has_override_for_read_files_allowlist(),
                        ctx,
                    );

                    Self::update_editor_interaction_state(
                        me.command_denylist_editor.as_ref(ctx).editor().clone(),
                        is_enabled,
                        ctx,
                    );

                    Self::update_editor_interaction_state(
                        me.command_allowlist_editor.as_ref(ctx).editor().clone(),
                        is_enabled
                            && !ai_autonomy_settings.has_override_for_execute_commands_allowlist(),
                        ctx,
                    );

                    me.update_voice_input_dropdown_enablement(ctx);
                    Self::refresh_autonomy_dropdown_menu(&me.autonomy_dropdown_menu, ctx);

                    me.refresh_all_execution_profile_ui(ctx);

                    Self::refresh_code_read_autonomy_dropdown_menu(
                        &me.code_read_autonomy_dropdown_menu,
                        ctx,
                    );
                    Self::refresh_base_model_menu(&me.base_model_dropdown, ctx);
                    Self::refresh_coding_model_menu(&me.coding_model_dropdown, ctx);
                    Self::refresh_mcp_allowlist_dropdown(&me.mcp_allowlist_dropdown, ctx);
                    Self::refresh_mcp_denylist_dropdown(&me.mcp_denylist_dropdown, ctx);
                    me.sync_context_window_editor(ctx, true);
                }
                AISettingsChangedEvent::VoiceInputEnabled { .. } => {
                    me.update_voice_input_dropdown_enablement(ctx);
                }
                AISettingsChangedEvent::AgentModeExecuteReadonlyCommands { .. } => {
                    Self::refresh_autonomy_dropdown_menu(&me.autonomy_dropdown_menu, ctx);
                    Self::refresh_code_read_autonomy_dropdown_menu(
                        &me.code_read_autonomy_dropdown_menu,
                        ctx,
                    );
                }
                AISettingsChangedEvent::AgentModeCodingPermissions { .. } => {
                    Self::refresh_code_read_autonomy_dropdown_menu(
                        &me.code_read_autonomy_dropdown_menu,
                        ctx,
                    );
                }
                AISettingsChangedEvent::VoiceInputToggleKey { .. } => {
                    let current_value = AISettings::as_ref(ctx)
                        .voice_input_toggle_key
                        .value()
                        .display_name();
                    me.voice_input_toggle_key_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_name(current_value, ctx)
                        });
                }
                AISettingsChangedEvent::AgentModeCommandExecutionAllowlist { .. } => {
                    me.command_execution_allowlist_mouse_state_handles = AISettings::as_ref(ctx)
                        .agent_mode_command_execution_allowlist
                        .value()
                        .iter()
                        .map(|_| Default::default())
                        .collect();
                }
                AISettingsChangedEvent::AgentModeCommandExecutionDenylist { .. } => {
                    me.command_execution_denylist_mouse_state_handles = AISettings::as_ref(ctx)
                        .agent_mode_command_execution_denylist
                        .value()
                        .iter()
                        .map(|_| Default::default())
                        .collect();
                }
                AISettingsChangedEvent::AgentModeCodingFileReadAllowlist { .. } => {
                    me.code_read_allowlist_mouse_state_handles = AISettings::as_ref(ctx)
                        .agent_mode_coding_file_read_allowlist
                        .value()
                        .iter()
                        .map(|_| Default::default())
                        .collect();
                }
                AISettingsChangedEvent::CLIAgentToolbarEnabledCommands { .. } => {
                    me.cli_agent_footer_command_mouse_state_handles = AISettings::as_ref(ctx)
                        .cli_agent_footer_enabled_commands
                        .value()
                        .keys()
                        .map(|_| Default::default())
                        .collect();
                    me.cli_agent_footer_command_agent_dropdowns =
                        Self::create_cli_agent_dropdowns(ctx);
                }
                AISettingsChangedEvent::ThinkingDisplayMode { .. } => {
                    let current_mode = *AISettings::as_ref(ctx).thinking_display_mode.value();
                    me.thinking_display_mode_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                AISettingsPageAction::SetThinkingDisplayMode(current_mode),
                                ctx,
                            );
                        });
                }
                AISettingsChangedEvent::OrchestrationMessageDisplayMode { .. } => {
                    let current_mode = AISettings::as_ref(ctx).orchestration_message_display_mode;
                    me.orchestration_message_display_mode_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                AISettingsPageAction::SetOrchestrationMessageDisplayMode(
                                    current_mode,
                                ),
                                ctx,
                            );
                        });
                }
                AISettingsChangedEvent::PromptSubmissionMode { .. } => {
                    let current_mode = AISettings::as_ref(ctx).default_prompt_submission_mode;
                    me.default_prompt_submission_mode_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                AISettingsPageAction::SetPromptSubmissionMode(current_mode),
                                ctx,
                            );
                        });
                }
                AISettingsChangedEvent::LongRunningCommandSubmissionMode { .. } => {
                    let current_mode = AISettings::as_ref(ctx).long_running_command_submission_mode;
                    me.lrc_submission_mode_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                AISettingsPageAction::SetLongRunningCommandSubmissionMode(
                                    current_mode,
                                ),
                                ctx,
                            );
                        });
                }
                _ => (),
            }
            ctx.notify();
        });

        ctx.subscribe_to_model(&SessionSettings::handle(ctx), |_, _, event, ctx| {
            if let SessionSettingsChangedEvent::ShowModelSelectorsInPrompt { .. } = event {
                ctx.notify();
            }
        });

        ctx.subscribe_to_model(&InputSettings::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });

        let current_permission =
            BlocklistAIPermissions::as_ref(ctx).active_permissions_profile(ctx, None);

        let apply_code_diffs_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);

            dropdown.set_items(
                vec![
                    DropdownItem::new(
                        "Agent decides",
                        AISettingsPageAction::SetApplyCodeDiffs(ActionPermission::AgentDecides),
                    ),
                    DropdownItem::new(
                        "Always allow",
                        AISettingsPageAction::SetApplyCodeDiffs(ActionPermission::AlwaysAllow),
                    ),
                    DropdownItem::new(
                        "Always ask",
                        AISettingsPageAction::SetApplyCodeDiffs(ActionPermission::AlwaysAsk),
                    ),
                ],
                ctx,
            );
            dropdown
        });
        Self::refresh_execution_profile_dropdown_menu(
            &apply_code_diffs_dropdown_menu,
            current_permission.apply_code_diffs,
            !AISettings::as_ref(ctx).is_code_diffs_permissions_editable(ctx),
            ctx,
        );

        let read_files_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_items(
                vec![
                    DropdownItem::new(
                        "Agent decides",
                        AISettingsPageAction::SetReadFiles(ActionPermission::AgentDecides),
                    ),
                    DropdownItem::new(
                        "Always allow",
                        AISettingsPageAction::SetReadFiles(ActionPermission::AlwaysAllow),
                    ),
                    DropdownItem::new(
                        "Always ask",
                        AISettingsPageAction::SetReadFiles(ActionPermission::AlwaysAsk),
                    ),
                ],
                ctx,
            );
            dropdown
        });
        Self::refresh_execution_profile_dropdown_menu(
            &read_files_dropdown_menu,
            current_permission.read_files,
            !AISettings::as_ref(ctx).is_read_files_permissions_editable(ctx),
            ctx,
        );

        let execute_commands_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_items(
                vec![
                    DropdownItem::new(
                        "Agent decides",
                        AISettingsPageAction::SetExecuteCommands(ActionPermission::AgentDecides),
                    ),
                    DropdownItem::new(
                        "Always allow",
                        AISettingsPageAction::SetExecuteCommands(ActionPermission::AlwaysAllow),
                    ),
                    DropdownItem::new(
                        "Always ask",
                        AISettingsPageAction::SetExecuteCommands(ActionPermission::AlwaysAsk),
                    ),
                ],
                ctx,
            );
            dropdown
        });
        Self::refresh_execution_profile_dropdown_menu(
            &execute_commands_dropdown_menu,
            current_permission.execute_commands,
            !AISettings::as_ref(ctx).is_execute_commands_permissions_editable(ctx),
            ctx,
        );

        let write_to_pty_autonomy_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_items(
                vec![
                    DropdownItem::new(
                        "Always allow",
                        AISettingsPageAction::SetWriteToPty(WriteToPtyPermission::AlwaysAllow),
                    ),
                    DropdownItem::new(
                        "Always ask",
                        AISettingsPageAction::SetWriteToPty(WriteToPtyPermission::AlwaysAsk),
                    ),
                    DropdownItem::new(
                        "Ask on first write",
                        AISettingsPageAction::SetWriteToPty(WriteToPtyPermission::AskOnFirstWrite),
                    ),
                ],
                ctx,
            );
            dropdown
        });
        Self::refresh_write_to_pty_dropdown_menu(
            &write_to_pty_autonomy_dropdown_menu,
            current_permission.write_to_pty,
            !AISettings::as_ref(ctx).is_write_to_pty_permissions_editable(ctx),
            ctx,
        );

        let mcp_permissions_dropdown_menu = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_items(
                vec![
                    DropdownItem::new(
                        "Agent decides",
                        AISettingsPageAction::SetMCPPermissions(ActionPermission::AgentDecides),
                    ),
                    DropdownItem::new(
                        "Always allow",
                        AISettingsPageAction::SetMCPPermissions(ActionPermission::AlwaysAllow),
                    ),
                    DropdownItem::new(
                        "Always ask",
                        AISettingsPageAction::SetMCPPermissions(ActionPermission::AlwaysAsk),
                    ),
                ],
                ctx,
            );
            dropdown
        });
        Self::refresh_execution_profile_dropdown_menu(
            &mcp_permissions_dropdown_menu,
            current_permission.mcp_permissions,
            !AISettings::as_ref(ctx).is_mcp_permission_editable(ctx),
            ctx,
        );

        let mcp_allowlist_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_header_to_static("Select MCP servers");
            dropdown
        });
        Self::refresh_mcp_allowlist_dropdown(&mcp_allowlist_dropdown, ctx);
        let mcp_allowlist_mouse_state_handles = BlocklistAIPermissions::as_ref(ctx)
            .get_mcp_allowlist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        let mcp_denylist_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_header_to_static("Select MCP servers");
            dropdown
        });
        Self::refresh_mcp_denylist_dropdown(&mcp_denylist_dropdown, ctx);
        let mcp_denylist_mouse_state_handles = BlocklistAIPermissions::as_ref(ctx)
            .get_mcp_denylist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        let command_execution_allowlist_mouse_state_handles = AISettings::as_ref(ctx)
            .agent_mode_command_execution_allowlist
            .value()
            .iter()
            .map(|_| Default::default())
            .collect();

        let command_execution_denylist_mouse_state_handles = AISettings::as_ref(ctx)
            .agent_mode_command_execution_denylist
            .value()
            .iter()
            .map(|_| Default::default())
            .collect();
        let cli_agent_footer_command_mouse_state_handles = AISettings::as_ref(ctx)
            .cli_agent_footer_enabled_commands
            .value()
            .keys()
            .map(|_| Default::default())
            .collect();

        let code_read_allowlist_mouse_state_handles = AISettings::as_ref(ctx)
            .agent_mode_coding_file_read_allowlist
            .value()
            .iter()
            .map(|_| Default::default())
            .collect();

        let directory_allowlist_mouse_state_handles = current_permission
            .directory_allowlist
            .iter()
            .map(|_| Default::default())
            .collect();

        let directory_allowlist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx).validate_on_submit(|s| {
                let expanded = host_native_absolute_path(s, &None, &None);
                Path::new(&expanded).is_dir()
            });
            input.set_placeholder_text("e.g. ~/code-repos/repo", ctx);
            input
        });

        Self::update_editor_interaction_state(
            directory_allowlist_editor.as_ref(ctx).editor().clone(),
            is_any_ai_enabled,
            ctx,
        );

        ctx.subscribe_to_view(&directory_allowlist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let expanded = host_native_absolute_path(s, &None, &None);
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();

                    model.add_to_directory_allowlist(*profile_id, &PathBuf::from(expanded), ctx);
                });
                ctx.notify();
            }
        });

        let org_denylist = BlocklistAIPermissions::get_org_execute_commands_denylist(ctx);
        let command_denylist_mouse_state_handles = current_permission
            .command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();
        let command_denylist_tooltip_mouse_state_handles: Vec<MouseStateHandle> =
            org_denylist.iter().map(|_| Default::default()).collect();

        let command_denylist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. rm .*", ctx);
            input
        });
        Self::update_editor_interaction_state(
            command_denylist_editor.as_ref(ctx).editor().clone(),
            is_any_ai_enabled && !ai_autonomy_settings.has_override_for_execute_commands_denylist(),
            ctx,
        );

        ctx.subscribe_to_view(&command_denylist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let predicate = match AgentModeCommandExecutionPredicate::new_regex(s) {
                    Ok(regex) => regex,
                    Err(e) => {
                        log::warn!(
                            "Failed to convert string to regex for cmd execution denylist: {e}"
                        );
                        return;
                    }
                };
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.add_to_command_denylist(*profile_id, &predicate, ctx);
                });
                ctx.notify();
            }
        });

        let command_allowlist_mouse_state_handles = current_permission
            .command_allowlist
            .iter()
            .map(|_| Default::default())
            .collect();

        let command_allowlist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. ls .*", ctx);
            input
        });
        Self::update_editor_interaction_state(
            command_allowlist_editor.as_ref(ctx).editor().clone(),
            is_any_ai_enabled
                && !ai_autonomy_settings.has_override_for_execute_commands_allowlist(),
            ctx,
        );

        ctx.subscribe_to_view(&command_allowlist_editor, |_, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(s) = event {
                let predicate = match AgentModeCommandExecutionPredicate::new_regex(s) {
                    Ok(regex) => regex,
                    Err(e) => {
                        log::warn!(
                            "Failed to convert string to regex for cmd execution allowlist: {e}"
                        );
                        return;
                    }
                };
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.add_to_command_allowlist(*profile_id, &predicate, ctx);
                });
                ctx.notify();
            }
        });

        let profile_views = Self::create_profile_views(ctx);

        // Custom model router views
        #[cfg(feature = "local_fs")]
        let router_views = Self::create_router_views(ctx);
        #[cfg(feature = "local_fs")]
        let add_router_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("+ Add router", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::OpenAddCustomRouter);
                })
        });
        #[cfg(feature = "local_fs")]
        {
            let is_enabled = galaxy_core::features::FeatureFlag::CustomModelRouters.is_enabled()
                && is_any_ai_enabled;
            add_router_button.update(ctx, |button, ctx| {
                button.set_disabled(!is_enabled, ctx);
            });
        }

        let add_profile_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add Profile", SecondaryTheme)
                .with_icon(Icon::Plus)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::CreateProfile);
                })
        });

        add_profile_button.update(ctx, |button, ctx| {
            button.set_disabled(!is_any_ai_enabled, ctx);
        });

        let provider_setup_body = ctx.add_typed_action_view(ProviderSetupView::new);
        ctx.subscribe_to_view(&provider_setup_body, |me, _, event, ctx| match event {
            ProviderSetupViewEvent::Close => me.clear_inline_provider_setup(ctx),
            ProviderSetupViewEvent::RequestAcpDiscovery(draft) => {
                #[cfg(not(target_family = "wasm"))]
                me.refresh_acp_discovery_for_draft(draft, ctx);
            }
            ProviderSetupViewEvent::SaveOpenAI {
                editing_index,
                provider,
            } => me.save_provider_setup(*editing_index, provider.clone(), ctx),
            ProviderSetupViewEvent::SaveBedrock(draft) => {
                me.save_bedrock_provider(draft.clone(), ctx)
            }
            ProviderSetupViewEvent::SaveAcp(draft) => me.save_acp_provider(draft.clone(), ctx),
        });

        let agent_toolbar_inline_editor = ctx.add_typed_action_view(|ctx| {
            AgentToolbarInlineEditor::new(AgentToolbarEditorMode::AgentView, ctx)
        });
        let cli_agent_toolbar_inline_editor = ctx.add_typed_action_view(|ctx| {
            AgentToolbarInlineEditor::new(AgentToolbarEditorMode::CLIAgent, ctx)
        });

        #[cfg(feature = "local_fs")]
        let conversation_layout_dropdown = ctx.add_typed_action_view(|ctx| {
            use crate::util::file::external_editor::settings::OpenConversationPreference;

            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);

            let items = vec![
                DropdownItem::new(
                    "New Tab",
                    AISettingsPageAction::SetConversationLayout(OpenConversationPreference::NewTab),
                ),
                DropdownItem::new(
                    "Split Pane",
                    AISettingsPageAction::SetConversationLayout(
                        OpenConversationPreference::SplitPane,
                    ),
                ),
            ];
            dropdown.set_items(items, ctx);

            let current = *crate::util::file::external_editor::EditorSettings::as_ref(ctx)
                .open_conversation_layout_preference;
            match current {
                OpenConversationPreference::NewTab => dropdown.set_selected_by_name("New Tab", ctx),
                OpenConversationPreference::SplitPane => {
                    dropdown.set_selected_by_name("Split Pane", ctx)
                }
            };
            dropdown
        });

        // Subscribe to GalaxyConfig to refresh router views when files change.
        #[cfg(feature = "local_fs")]
        ctx.subscribe_to_model(
            &crate::user_config::GalaxyConfig::handle(ctx),
            |me, _, event, ctx| {
                use crate::user_config::GalaxyConfigUpdateEvent;
                if matches!(event, GalaxyConfigUpdateEvent::ModelConfigs) {
                    me.router_views = Self::create_router_views(ctx);
                    ctx.notify();
                }
            },
        );

        let (page, _) = Self::build_page(None, ctx);

        Self {
            page,
            active_subpage: None,
            voice_input_toggle_key_dropdown,
            autodetection_denylist_editor,
            local_only_icon_tooltip_states: Default::default(),
            command_execution_allowlist_editor,
            command_execution_denylist_editor,
            command_execution_allowlist_mouse_state_handles,
            command_execution_denylist_mouse_state_handles,
            cli_agent_footer_command_editor,
            cli_agent_footer_command_mouse_state_handles,
            cli_agent_footer_command_agent_dropdowns: Self::create_cli_agent_dropdowns(ctx),
            agent_toolbar_inline_editor,
            cli_agent_toolbar_inline_editor,
            base_model_dropdown,
            coding_model_dropdown,
            context_window_slider_state,
            context_window_editor,
            last_synced_context_window_editor_value,
            dragged_context_window_value: None,
            autonomy_dropdown_menu,
            code_read_allowlist_editor,
            code_read_autonomy_dropdown_menu,
            code_read_allowlist_mouse_state_handles,
            apply_code_diffs_dropdown_menu,
            read_files_dropdown_menu,
            execute_commands_dropdown_menu,
            write_to_pty_autonomy_dropdown_menu,
            mcp_permissions_dropdown_menu,
            directory_allowlist_mouse_state_handles,
            directory_allowlist_editor,
            command_denylist_mouse_state_handles,
            command_denylist_tooltip_mouse_state_handles,
            command_denylist_editor,
            command_allowlist_mouse_state_handles,
            command_allowlist_editor,
            mcp_allowlist_dropdown,
            mcp_allowlist_mouse_state_handles,
            mcp_denylist_dropdown,
            mcp_denylist_mouse_state_handles,
            thinking_display_mode_dropdown,
            orchestration_message_display_mode_dropdown,
            default_prompt_submission_mode_dropdown,
            lrc_submission_mode_dropdown,
            #[cfg(feature = "local_fs")]
            conversation_layout_dropdown,
            profile_views,
            add_profile_button,
            provider_setup_body,
            inline_provider_setup: None,
            #[cfg(feature = "local_fs")]
            router_views,
            #[cfg(feature = "local_fs")]
            add_router_button,
        }
    }

    fn update_voice_input_dropdown_enablement(&mut self, ctx: &mut ViewContext<Self>) {
        let is_voice_enabled = AISettings::as_ref(ctx).is_voice_input_enabled(ctx);
        self.voice_input_toggle_key_dropdown
            .update(ctx, |dropdown, ctx| {
                if is_voice_enabled {
                    dropdown.set_enabled(ctx);
                } else {
                    dropdown.set_disabled(ctx);
                }
            });
        ctx.notify();
    }

    fn provider_setup_type_for_kind(kind: OpenAIProviderKind) -> ProviderSetupProviderType {
        match kind {
            OpenAIProviderKind::OpenAI => ProviderSetupProviderType::OpenAI,
            OpenAIProviderKind::LiteLLM => ProviderSetupProviderType::LiteLLM,
            OpenAIProviderKind::ChatGPTSubscription => {
                ProviderSetupProviderType::ChatGPTSubscription
            }
            OpenAIProviderKind::Anthropic => ProviderSetupProviderType::Anthropic,
            OpenAIProviderKind::Gemini => ProviderSetupProviderType::Gemini,
            OpenAIProviderKind::VertexAI => ProviderSetupProviderType::VertexAI,
        }
    }

    fn begin_inline_provider_create(
        &mut self,
        provider_type: ProviderSetupProviderType,
        ctx: &mut ViewContext<Self>,
    ) {
        self.provider_setup_body
            .update(ctx, |body, ctx| body.begin_create(provider_type, ctx));
        self.inline_provider_setup = Some(InlineProviderSetupState { provider_type });
        ctx.notify();
    }

    fn begin_inline_provider_edit(&mut self, provider_index: usize, ctx: &mut ViewContext<Self>) {
        let Some(provider) = AISettings::as_ref(ctx)
            .openai_providers
            .value()
            .get(provider_index)
            .cloned()
        else {
            return;
        };
        let provider_type = Self::provider_setup_type_for_kind(provider.kind);
        self.provider_setup_body.update(ctx, |body, ctx| {
            body.begin_edit(provider_index, provider, ctx)
        });
        self.inline_provider_setup = Some(InlineProviderSetupState { provider_type });
        ctx.notify();
    }

    fn begin_inline_bedrock_setup(&mut self, ctx: &mut ViewContext<Self>) {
        let settings = AISettings::as_ref(ctx);
        let draft = BedrockProviderDraft {
            name: settings.bedrock_connection_name.value().clone(),
            auth_method: *settings.bedrock_auth_method.value(),
            profile: settings.bedrock_profile.value().clone(),
            region: settings.bedrock_region.value().clone(),
            cross_region_inference: *settings.bedrock_cross_region_inference.value(),
            auto_login: *settings.bedrock_auto_login.value(),
            auth_refresh_command: settings.bedrock_auth_refresh_command.value().clone(),
            access_key_id: settings.bedrock_access_key_id.value().clone(),
            secret_access_key: settings.bedrock_secret_access_key.value().clone(),
            models: settings.bedrock_models.value().clone(),
        };
        self.provider_setup_body
            .update(ctx, |body, ctx| body.begin_edit_bedrock(draft.clone(), ctx));
        self.inline_provider_setup = Some(InlineProviderSetupState {
            provider_type: ProviderSetupProviderType::Bedrock,
        });
        ctx.notify();
    }

    fn clear_inline_provider_setup(&mut self, ctx: &mut ViewContext<Self>) {
        self.inline_provider_setup = None;
        self.rebuild_active_subpage(ctx);
    }

    fn save_provider_setup(
        &mut self,
        editing_index: Option<usize>,
        mut provider: OpenAIProviderConfig,
        ctx: &mut ViewContext<Self>,
    ) {
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            let mut providers = settings.openai_providers.value().clone();
            match editing_index {
                Some(index) => {
                    if let Some(existing) = providers.get_mut(index) {
                        provider.enabled = existing.enabled;
                        *existing = provider.clone();
                    }
                }
                None => providers.push(provider.clone()),
            }
            report_if_error!(settings.openai_providers.set_value(providers, ctx));
        });
        self.clear_inline_provider_setup(ctx);
    }

    fn save_bedrock_provider(&mut self, draft: BedrockProviderDraft, ctx: &mut ViewContext<Self>) {
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.bedrock_enabled.set_value(true, ctx));
            report_if_error!(settings
                .bedrock_auth_method
                .set_value(draft.auth_method, ctx));
            report_if_error!(settings.bedrock_profile.set_value(draft.profile, ctx));
            report_if_error!(settings.bedrock_region.set_value(draft.region, ctx));
            report_if_error!(settings
                .bedrock_cross_region_inference
                .set_value(draft.cross_region_inference, ctx));
            report_if_error!(settings.bedrock_auto_login.set_value(draft.auto_login, ctx));
            report_if_error!(settings
                .bedrock_auth_refresh_command
                .set_value(draft.auth_refresh_command, ctx));
            report_if_error!(settings
                .bedrock_access_key_id
                .set_value(draft.access_key_id, ctx));
            report_if_error!(settings
                .bedrock_secret_access_key
                .set_value(draft.secret_access_key, ctx));
            report_if_error!(settings.bedrock_connection_name.set_value(draft.name, ctx));
            report_if_error!(settings.bedrock_models.set_value(draft.models, ctx));
        });
        self.clear_inline_provider_setup(ctx);
    }

    fn save_acp_provider(&mut self, draft: AcpProviderDraft, ctx: &mut ViewContext<Self>) {
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            let legacy_provider = if settings.acp_providers.value().is_empty() {
                settings.legacy_acp_provider()
            } else {
                None
            };
            let mut providers = settings.acp_providers.value().clone();
            if let Some(legacy_provider) = legacy_provider {
                providers.push(legacy_provider);
            }
            providers.push(AcpProviderConfig::new(
                draft.name.clone(),
                draft.agent_id.clone(),
                draft.command.clone(),
                draft.args.clone(),
                draft.config_options.clone(),
            ));
            report_if_error!(settings.acp_enabled.set_value(true, ctx));
            report_if_error!(settings.acp_providers.set_value(providers, ctx));
            report_if_error!(settings.acp_agent_id.set_value(draft.agent_id, ctx));
            report_if_error!(settings.acp_agent_command.set_value(draft.command, ctx));
            report_if_error!(settings.acp_agent_args.set_value(draft.args, ctx));
            report_if_error!(settings.acp_connection_name.set_value(draft.name, ctx));
        });
        self.clear_inline_provider_setup(ctx);
    }

    /// Set the active subpage and rebuild the widget list to show only relevant widgets.
    pub fn set_active_subpage(&mut self, subpage: Option<AISubpage>, ctx: &mut ViewContext<Self>) {
        if self.active_subpage != subpage {
            self.active_subpage = subpage;
            let (page, _) = Self::build_page(subpage, ctx);
            self.page = page;
            ctx.notify();
        }
    }

    fn fetch_openai_provider_models(&mut self, provider_index: usize, ctx: &mut ViewContext<Self>) {
        LLMPreferences::handle(ctx).update(ctx, |llm_prefs, ctx| {
            llm_prefs.fetch_openai_provider_models(provider_index, ctx);
        });
    }

    fn rebuild_active_subpage(&mut self, ctx: &mut ViewContext<Self>) {
        let (page, _) = Self::build_page(self.active_subpage, ctx);
        self.page = page;
        ctx.notify();
    }

    fn build_page(
        subpage: Option<AISubpage>,
        ctx: &mut ViewContext<Self>,
    ) -> (PageType<Self>, Option<ViewHandle<ActionButton>>) {
        let ai_settings = AISettings::as_ref(ctx);

        let mut widgets: Vec<Box<dyn SettingsWidget<View = AISettingsPageView>>> = Vec::new();

        // When viewing a specific subpage, only include its widgets.
        // When subpage is None (legacy/backward-compat), show all widgets.
        match subpage {
            None => {
                // Full page: all widgets (legacy behavior)
                if ai_settings
                    .intelligent_autosuggestions_enabled_internal
                    .is_supported_on_current_platform()
                    || ai_settings
                        .prompt_suggestions_enabled_internal
                        .is_supported_on_current_platform()
                    || (FeatureFlag::PredictAMQueries.is_enabled()
                        && ai_settings
                            .natural_language_autosuggestions_enabled_internal
                            .is_supported_on_current_platform())
                    || (FeatureFlag::SharedBlockTitleGeneration.is_enabled()
                        && ai_settings
                            .shared_block_title_generation_enabled_internal
                            .is_supported_on_current_platform())
                    || (FeatureFlag::GitOperationsInCodeReview.is_enabled()
                        && ai_settings
                            .git_operations_autogen_enabled_internal
                            .is_supported_on_current_platform())
                {
                    widgets.push(Box::new(ActiveAIWidget::default()));
                }
                widgets.push(Box::new(AgentsWidget::default()));
                widgets.push(Box::new(AIInputWidget::default()));
                if MCPServersWidget::should_show_mcp() {
                    widgets.push(Box::new(MCPServersWidget::default()));
                }
                if FeatureFlag::AIRules.is_enabled() {
                    widgets.push(Box::new(AIFactWidget::default()));
                }
                if cfg!(feature = "voice_input")
                    && ai_settings
                        .voice_input_enabled_internal
                        .is_supported_on_current_platform()
                {
                    widgets.push(Box::new(VoiceWidget::default()));
                }
                widgets.push(Box::new(CloudHandoffWidget::default()));
                widgets.push(Box::new(CLIAgentWidget::default()));
                widgets.push(Box::new(AgentAttributionWidget::default()));
                widgets.push(Box::new(OtherAIWidget::default()));
            }
            Some(AISubpage::WarpAgent) => {
                // Galaxy Agent page: suggestions, input, and other agent settings.
                if ai_settings
                    .intelligent_autosuggestions_enabled_internal
                    .is_supported_on_current_platform()
                    || ai_settings
                        .prompt_suggestions_enabled_internal
                        .is_supported_on_current_platform()
                    || (FeatureFlag::PredictAMQueries.is_enabled()
                        && ai_settings
                            .natural_language_autosuggestions_enabled_internal
                            .is_supported_on_current_platform())
                    || (FeatureFlag::SharedBlockTitleGeneration.is_enabled()
                        && ai_settings
                            .shared_block_title_generation_enabled_internal
                            .is_supported_on_current_platform())
                    || (FeatureFlag::GitOperationsInCodeReview.is_enabled()
                        && ai_settings
                            .git_operations_autogen_enabled_internal
                            .is_supported_on_current_platform())
                {
                    widgets.push(Box::new(ActiveAIWidget::default()));
                }
                widgets.push(Box::new(AIInputWidget::default()));
                let voice_supported = cfg!(feature = "voice_input")
                    && ai_settings
                        .voice_input_enabled_internal
                        .is_supported_on_current_platform();
                if voice_supported {
                    widgets.push(Box::new(VoiceWidget::default()));
                }
                widgets.push(Box::new(CloudHandoffWidget::default()));
                if FeatureFlag::CustomModelRouters.is_enabled() {
                    widgets.push(Box::new(CustomModelRoutersWidget));
                }
                widgets.push(Box::new(AgentAttributionWidget::default()));
                widgets.push(Box::new(OtherAIWidget::default()));
            }
            Some(AISubpage::Profiles) => {
                widgets.push(Box::new(AgentsWidget::default()));
            }
            Some(AISubpage::Knowledge) => {
                if FeatureFlag::AIRules.is_enabled() {
                    widgets.push(Box::new(AIFactWidget::default()));
                }
            }
            Some(AISubpage::ThirdPartyCLIAgents) => {
                widgets.push(Box::new(CLIAgentWidget::default()));
            }
            Some(AISubpage::Models) => {
                widgets.push(Box::new(ModelsOverviewWidget));
                let title: Option<&str> = None;
                return (PageType::new_uncategorized(widgets, title), None);
            }
            Some(AISubpage::ProviderOpenAI) => {
                widgets.push(Box::new(ProviderSettingsWidget::new(
                    ctx,
                    ProviderSetupProviderType::OpenAI,
                )));
            }
            Some(AISubpage::ProviderLiteLLM) => {
                widgets.push(Box::new(ProviderSettingsWidget::new(
                    ctx,
                    ProviderSetupProviderType::LiteLLM,
                )));
            }
            Some(AISubpage::ProviderChatGPTSubscription) => {
                widgets.push(Box::new(ProviderSettingsWidget::new(
                    ctx,
                    ProviderSetupProviderType::ChatGPTSubscription,
                )));
            }
            Some(AISubpage::ProviderBedrock) => {
                widgets.push(Box::new(ProviderSettingsWidget::new(
                    ctx,
                    ProviderSetupProviderType::Bedrock,
                )));
            }
            Some(AISubpage::ProviderACP) => {
                widgets.push(Box::new(ProviderSettingsWidget::new(
                    ctx,
                    ProviderSetupProviderType::Acp,
                )));
            }
            Some(AISubpage::Experiments) => {
                widgets.push(Box::new(ExperimentsWidget::default()));
            }
        }

        // Subpage widgets render their own subheader-sized titles internally,
        // so we don't pass a page-level title to PageType.
        let title: Option<&str> = None;
        (PageType::new_uncategorized(widgets, title), None)
    }

    fn handle_context_window_editor_event(
        &mut self,
        event: &EditorEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            EditorEvent::Blurred | EditorEvent::Enter => {
                if !AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    self.sync_context_window_editor(ctx, true);
                    return;
                }
                if let Some(cw) = Self::configurable_context_window(ctx) {
                    let buffer_text = self.context_window_editor.as_ref(ctx).buffer_text(ctx);
                    let cleaned: String = buffer_text
                        .chars()
                        .filter(|c| !c.is_whitespace() && *c != ',')
                        .collect();
                    if let Ok(parsed) = cleaned.parse::<u32>() {
                        let clamped = parsed.clamp(cw.min, cw.max);
                        if Some(clamped) != Self::current_context_window_display_value(ctx) {
                            AIExecutionProfilesModel::handle(ctx).update(
                                ctx,
                                |profiles_model, ctx| {
                                    let profile_id = *profiles_model.active_profile(None, ctx).id();
                                    profiles_model.set_context_window_limit(
                                        profile_id,
                                        Some(clamped),
                                        ctx,
                                    );
                                },
                            );
                        }
                    }
                }
                self.sync_context_window_editor(ctx, true);
                if let EditorEvent::Enter = event {
                    ctx.emit(AISettingsPageEvent::FocusSearch);
                }
                ctx.notify();
            }
            EditorEvent::Escape => ctx.emit(AISettingsPageEvent::FocusSearch),
            _ => {}
        }
    }

    fn active_profile_data(app: &AppContext) -> AIExecutionProfile {
        AIExecutionProfilesModel::as_ref(app)
            .active_profile(None, app)
            .data()
            .clone()
    }

    fn configurable_context_window(app: &AppContext) -> Option<LLMContextWindow> {
        Self::active_profile_data(app).configurable_context_window(app)
    }

    fn current_context_window_display_value(app: &AppContext) -> Option<u32> {
        Self::active_profile_data(app).context_window_display_value(app)
    }

    fn initial_context_window_value(app: &AppContext) -> u32 {
        Self::current_context_window_display_value(app).unwrap_or_else(|| {
            LLMPreferences::as_ref(app)
                .get_active_base_model(app, None)
                .context_window
                .default_max
        })
    }

    fn sync_context_window_editor(&mut self, ctx: &mut ViewContext<Self>, force: bool) {
        self.dragged_context_window_value = None;
        let Some(value) = Self::current_context_window_display_value(ctx) else {
            self.last_synced_context_window_editor_value = None;
            self.context_window_slider_state.reset_offset();
            ctx.notify();
            return;
        };

        let formatted = value.to_string();
        let should_update = if force {
            true
        } else {
            match self.last_synced_context_window_editor_value {
                Some(last_value) => {
                    self.context_window_editor.as_ref(ctx).buffer_text(ctx)
                        == last_value.to_string()
                }
                None => true,
            }
        };

        if should_update {
            self.context_window_editor.update(ctx, |editor, ctx| {
                if editor.buffer_text(ctx) != formatted {
                    editor.system_reset_buffer_text(&formatted, ctx);
                }
            });
            self.last_synced_context_window_editor_value = Some(value);
            self.context_window_slider_state.reset_offset();
            ctx.notify();
        }
    }

    fn handle_detection_denylist_editor_event(
        &mut self,
        event: &EditorEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            EditorEvent::Blurred | EditorEvent::Enter => {
                let buffer_text = self
                    .autodetection_denylist_editor
                    .as_ref(ctx)
                    .buffer_text(ctx);
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    if let Err(e) = settings
                        .autodetection_command_denylist
                        .set_value(buffer_text, ctx)
                    {
                        log::warn!("Failed to set AI autodetection blacklist commands: {e:?}");
                    }
                })
            }
            EditorEvent::Escape => ctx.emit(AISettingsPageEvent::FocusSearch),
            _ => {}
        }
    }

    fn update_editor_interaction_state(
        editor: ViewHandle<EditorView>,
        is_enabled: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        editor.update(ctx, |editor, ctx| {
            let interaction_state = if is_enabled {
                InteractionState::Editable
            } else {
                InteractionState::Disabled
            };
            editor.set_interaction_state(interaction_state, ctx);
            ctx.notify();
        })
    }

    pub fn refresh_base_model_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            let disabled_by_ai_toggle = !AISettings::as_ref(ctx).is_any_ai_enabled(ctx);

            if disabled_by_ai_toggle {
                menu.set_disabled(ctx);
            } else {
                menu.set_enabled(ctx);
            }

            let choices = LLMPreferences::as_ref(ctx)
                .get_base_llm_choices_for_agent_mode(ctx)
                .collect_vec();

            let items = available_model_menu_items(
                choices,
                |llm| {
                    DropdownAction::select_action_and_close(AISettingsPageAction::SetBaseModel(
                        llm.id.clone(),
                    ))
                },
                None,
                None,
                false,
                false,
                ctx,
            );
            menu.set_rich_items(items, ctx);

            let active = LLMPreferences::as_ref(ctx).get_active_base_model(ctx, None);
            menu.set_selected_by_action(AISettingsPageAction::SetBaseModel(active.id.clone()), ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    pub fn refresh_coding_model_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            let disabled_by_ai_toggle = !AISettings::as_ref(ctx).is_any_ai_enabled(ctx);

            if disabled_by_ai_toggle {
                menu.set_disabled(ctx);
            } else {
                menu.set_enabled(ctx);
            }

            let choices = LLMPreferences::as_ref(ctx)
                .get_coding_llm_choices(ctx)
                .collect_vec();

            let items = available_model_menu_items(
                choices,
                |llm| {
                    DropdownAction::select_action_and_close(AISettingsPageAction::SetCodingModel(
                        llm.id.clone(),
                    ))
                },
                None,
                None,
                false,
                false,
                ctx,
            );
            menu.set_rich_items(items, ctx);
            let active = LLMPreferences::as_ref(ctx).get_active_coding_model(ctx, None);

            menu.set_selected_by_action(
                AISettingsPageAction::SetCodingModel(active.id.clone()),
                ctx,
            );
            ctx.notify();
        });
        ctx.notify();
    }

    fn refresh_autonomy_dropdown_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            if AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                menu.set_enabled(ctx);
            } else {
                menu.set_disabled(ctx);
            }

            menu.set_items(
                vec![
                    DropdownItem::new(
                        "Read only",
                        AISettingsPageAction::SetAutonomyReadonlyCommandsSetting,
                    ),
                    DropdownItem::new(
                        "Supervised",
                        AISettingsPageAction::SetAutonomySupervisedSetting,
                    ),
                ],
                ctx,
            );
            let active = if *AISettings::as_ref(ctx).agent_mode_execute_read_only_commands {
                0
            } else {
                1
            };
            menu.set_selected_by_index(active, ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    fn refresh_all_execution_profile_ui(&self, ctx: &mut ViewContext<Self>) {
        let permissions = BlocklistAIPermissions::handle(ctx);

        let apply_code_diffs_setting = permissions
            .as_ref(ctx)
            .get_apply_code_diffs_setting(ctx, None);
        Self::refresh_execution_profile_dropdown_menu(
            &self.apply_code_diffs_dropdown_menu,
            apply_code_diffs_setting,
            !AISettings::as_ref(ctx).is_code_diffs_permissions_editable(ctx),
            ctx,
        );

        let read_files_setting = permissions.as_ref(ctx).get_read_files_setting(ctx, None);
        Self::refresh_execution_profile_dropdown_menu(
            &self.read_files_dropdown_menu,
            read_files_setting,
            !AISettings::as_ref(ctx).is_read_files_permissions_editable(ctx),
            ctx,
        );

        let execute_commands_setting: ActionPermission = permissions
            .as_ref(ctx)
            .get_execute_commands_setting(ctx, None);
        Self::refresh_execution_profile_dropdown_menu(
            &self.execute_commands_dropdown_menu,
            execute_commands_setting,
            !AISettings::as_ref(ctx).is_execute_commands_permissions_editable(ctx),
            ctx,
        );

        let write_to_pty_setting: WriteToPtyPermission =
            permissions.as_ref(ctx).get_write_to_pty_setting(ctx, None);
        Self::refresh_write_to_pty_dropdown_menu(
            &self.write_to_pty_autonomy_dropdown_menu,
            write_to_pty_setting,
            !AISettings::as_ref(ctx).is_write_to_pty_permissions_editable(ctx),
            ctx,
        );

        let mcp_permissions_setting = permissions
            .as_ref(ctx)
            .get_mcp_permissions_setting(ctx, None);
        Self::refresh_execution_profile_dropdown_menu(
            &self.mcp_permissions_dropdown_menu,
            mcp_permissions_setting,
            !AISettings::as_ref(ctx).is_mcp_permission_editable(ctx),
            ctx,
        );
        Self::refresh_mcp_allowlist_dropdown(&self.mcp_allowlist_dropdown, ctx);
        Self::refresh_mcp_denylist_dropdown(&self.mcp_denylist_dropdown, ctx);

        let is_any_ai_enabled = AISettings::as_ref(ctx).is_any_ai_enabled(ctx);
        self.add_profile_button.update(ctx, |button, ctx| {
            button.set_disabled(!is_any_ai_enabled, ctx);
        });
    }

    fn reset_execution_profile_mouse_state_handles(&mut self, ctx: &mut ViewContext<Self>) {
        let blocklist_permissions = BlocklistAIPermissions::as_ref(ctx);

        self.directory_allowlist_mouse_state_handles = blocklist_permissions
            .get_read_files_allowlist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        self.command_denylist_mouse_state_handles = blocklist_permissions
            .get_execute_commands_denylist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        let org_denylist = BlocklistAIPermissions::get_org_execute_commands_denylist(ctx);
        self.command_denylist_tooltip_mouse_state_handles =
            org_denylist.iter().map(|_| Default::default()).collect();

        self.command_allowlist_mouse_state_handles = blocklist_permissions
            .get_execute_commands_allowlist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        self.mcp_allowlist_mouse_state_handles = blocklist_permissions
            .get_mcp_allowlist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();

        self.mcp_denylist_mouse_state_handles = blocklist_permissions
            .get_mcp_denylist(ctx, None)
            .iter()
            .map(|_| Default::default())
            .collect();
    }

    fn refresh_execution_profile_dropdown_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        current_permission: ActionPermission,
        disabled: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            if !disabled {
                menu.set_enabled(ctx);
            } else {
                menu.set_disabled(ctx);
            }

            let active = match current_permission {
                ActionPermission::AgentDecides | ActionPermission::Unknown => 0,
                ActionPermission::AlwaysAllow => 1,
                ActionPermission::AlwaysAsk => 2,
            };

            menu.set_selected_by_index(active, ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    fn refresh_write_to_pty_dropdown_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        current_permission: WriteToPtyPermission,
        disabled: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            if !disabled {
                menu.set_enabled(ctx);
            } else {
                menu.set_disabled(ctx);
            }

            let active = match current_permission {
                WriteToPtyPermission::AlwaysAllow => 0,
                WriteToPtyPermission::AlwaysAsk | WriteToPtyPermission::Unknown => 1,
                WriteToPtyPermission::AskOnFirstWrite => 2,
            };

            menu.set_selected_by_index(active, ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    /// Currently, the coding permissions only support "read" access.
    fn refresh_code_read_autonomy_dropdown_menu(
        menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |menu, ctx| {
            if AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                menu.set_enabled(ctx);
            } else {
                menu.set_disabled(ctx);
            }

            menu.set_items(
                AgentModeCodingPermissionsType::iter()
                    .map(|t| {
                        let display = match t {
                            AgentModeCodingPermissionsType::AlwaysAskBeforeReading => "Always ask",
                            AgentModeCodingPermissionsType::AlwaysAllowReading => "Always allow",
                            AgentModeCodingPermissionsType::AllowReadingSpecificFiles => {
                                "Allow in specific directories"
                            }
                        };
                        DropdownItem::new(display, AISettingsPageAction::SetCodingPermission(t))
                    })
                    .collect(),
                ctx,
            );
            let ai_settings = AISettings::as_ref(ctx);

            let active = if *ai_settings.agent_mode_execute_read_only_commands {
                menu.set_disabled(ctx);
                AgentModeCodingPermissionsType::AlwaysAllowReading
            } else {
                *ai_settings.agent_mode_coding_permissions
            };
            menu.set_selected_by_action(AISettingsPageAction::SetCodingPermission(active), ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    fn get_non_allowlisted_or_denylisted_mcp_servers(
        ctx: &mut ViewContext<Self>,
    ) -> Vec<(uuid::Uuid, String)> {
        let all_mcp_servers = TemplatableMCPServerManager::get_all_cloud_synced_mcp_servers(ctx);
        let already_allowlisted_mcp_servers =
            BlocklistAIPermissions::as_ref(ctx).get_mcp_allowlist(ctx, None);
        let already_denylisted_mcp_servers =
            BlocklistAIPermissions::as_ref(ctx).get_mcp_denylist(ctx, None);

        all_mcp_servers
            .into_iter()
            .filter(|(uuid, _)| {
                let is_allowlisted = already_allowlisted_mcp_servers.contains(uuid);
                let is_denylisted = already_denylisted_mcp_servers.contains(uuid);
                !is_allowlisted && !is_denylisted
            })
            .collect()
    }

    fn refresh_menu_dropdown<F>(
        menu: &ViewHandle<FilterableDropdown<AISettingsPageAction>>,
        action_fn: F,
        ctx: &mut ViewContext<Self>,
    ) where
        F: Fn(uuid::Uuid) -> AISettingsPageAction,
    {
        let mcps_in_dropdown = Self::get_non_allowlisted_or_denylisted_mcp_servers(ctx);
        menu.update(ctx, |menu, ctx| {
            if AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                menu.set_enabled(ctx);
            } else {
                menu.set_disabled(ctx);
            }

            let items: Vec<DropdownItem<AISettingsPageAction>> = mcps_in_dropdown
                .iter()
                .map(|(uuid, server_name)| DropdownItem::new(server_name, action_fn(*uuid)))
                .collect();

            menu.set_items(items, ctx);
            ctx.notify()
        });
        ctx.notify();
    }

    fn refresh_mcp_allowlist_dropdown(
        menu: &ViewHandle<FilterableDropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        Self::refresh_menu_dropdown(menu, AISettingsPageAction::AddToMCPAllowlist, ctx);
    }

    #[cfg(feature = "local_fs")]
    fn create_router_views(
        ctx: &mut ViewContext<Self>,
    ) -> Vec<ViewHandle<super::custom_router_view::CustomRouterView>> {
        use super::custom_router_view::{CustomRouterView, CustomRouterViewEvent};
        use crate::user_config::GalaxyConfig;
        if !galaxy_core::features::FeatureFlag::CustomModelRouters.is_enabled() {
            return Vec::new();
        }
        let routers: Vec<crate::ai::custom_model_routers::CustomModelRouter> =
            GalaxyConfig::as_ref(ctx).custom_model_routers().clone();
        routers
            .into_iter()
            .map(|router| {
                let router_clone = router.clone();
                let view = ctx.add_typed_action_view(|ctx| CustomRouterView::new(router, ctx));
                ctx.subscribe_to_view(&view, move |me, _, event, ctx| match event {
                    CustomRouterViewEvent::OpenFile(path) => {
                        ctx.emit(AISettingsPageEvent::OpenCustomRouterFile(path.clone()));
                    }
                    CustomRouterViewEvent::Edit => {
                        let r = router_clone.clone();
                        ctx.emit(AISettingsPageEvent::OpenCustomRouterEditor(Some(r)));
                    }
                    CustomRouterViewEvent::Delete => {
                        if let Some(path) = &router_clone.source_path {
                            #[cfg(feature = "local_fs")]
                            {
                                if let Err(e) =
                                    crate::user_config::GalaxyConfig::delete_custom_model_router(
                                        path,
                                    )
                                {
                                    log::warn!("Failed to delete custom router: {e:?}");
                                }
                            }
                            me.router_views = Self::create_router_views(ctx);
                            ctx.notify();
                        }
                    }
                });
                view
            })
            .collect()
    }

    fn create_profile_views(ctx: &mut ViewContext<Self>) -> Vec<ViewHandle<ExecutionProfileView>> {
        let profiles_model = AIExecutionProfilesModel::as_ref(ctx);
        let profile_ids = profiles_model.get_all_profile_ids();

        profile_ids
            .iter()
            .map(|&profile_id| {
                let profile_view =
                    ctx.add_typed_action_view(|ctx| ExecutionProfileView::new(profile_id, ctx));

                ctx.subscribe_to_view(&profile_view, move |_me, _, event, ctx| match event {
                    ExecutionProfileViewEvent::EditProfile => {
                        ctx.emit(AISettingsPageEvent::OpenExecutionProfileEditor(profile_id));
                    }
                });

                profile_view
            })
            .collect()
    }

    fn refresh_profile_views(&mut self, ctx: &mut ViewContext<Self>) {
        let new_profile_views = Self::create_profile_views(ctx);
        self.profile_views = new_profile_views;
    }

    fn refresh_mcp_denylist_dropdown(
        menu: &ViewHandle<FilterableDropdown<AISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        Self::refresh_menu_dropdown(menu, AISettingsPageAction::AddToMCPDenylist, ctx);
    }

    fn create_cli_agent_dropdowns(
        ctx: &mut ViewContext<Self>,
    ) -> Vec<ViewHandle<Dropdown<AISettingsPageAction>>> {
        let entries: Vec<(String, CLIAgent)> = AISettings::as_ref(ctx)
            .cli_agent_footer_enabled_commands
            .value()
            .iter()
            .map(|(pattern, agent_value)| {
                (pattern.clone(), CLIAgent::from_serialized_name(agent_value))
            })
            .collect();

        entries
            .into_iter()
            .map(|(pattern_clone, current_agent)| {
                ctx.add_typed_action_view(move |ctx| {
                    let mut dropdown = Dropdown::new(ctx);
                    dropdown.set_top_bar_max_width(160.);
                    dropdown.set_menu_width(180., ctx);
                    dropdown.set_main_axis_size(MainAxisSize::Min, ctx);

                    let mut items: Vec<MenuItem<DropdownAction>> = Vec::new();

                    for agent in all::<CLIAgent>() {
                        if matches!(agent, CLIAgent::Unknown) {
                            continue;
                        }
                        let icon = agent.icon();
                        let mut fields = MenuItemFields::new(agent.display_name())
                            .with_on_select_action(DropdownAction::select_action_and_close(
                                AISettingsPageAction::SetCLIAgentForCommand {
                                    pattern: pattern_clone.clone(),
                                    agent: Some(agent),
                                },
                            ));
                        if let Some(icon) = icon {
                            fields = fields.with_icon(icon);
                        }
                        items.push(fields.into_item());
                    }

                    items.push(
                        MenuItemFields::new("Other")
                            .with_on_select_action(DropdownAction::select_action_and_close(
                                AISettingsPageAction::SetCLIAgentForCommand {
                                    pattern: pattern_clone.clone(),
                                    agent: None,
                                },
                            ))
                            .into_item(),
                    );

                    dropdown.set_rich_items(items, ctx);

                    dropdown.set_menu_header_text_override(|label| {
                        if label == "Other" {
                            "Select coding agent".to_string()
                        } else {
                            label.to_string()
                        }
                    });

                    let selected_name = if matches!(current_agent, CLIAgent::Unknown) {
                        "Other"
                    } else {
                        current_agent.display_name()
                    };
                    dropdown.set_selected_by_name(selected_name, ctx);

                    dropdown
                })
            })
            .collect()
    }
}

impl View for AISettingsPageView {
    fn ui_name() -> &'static str {
        "AISettingsPage"
    }

    fn render(&self, app: &galaxyui::AppContext) -> Box<dyn galaxyui::Element> {
        self.page.render(self, app)
    }
}

#[allow(clippy::large_enum_variant)]
pub enum AISettingsPageEvent {
    FocusSearch,
    OpenAIFactCollection,
    OpenMCPServerCollection,
    #[cfg(feature = "local_fs")]
    OpenCustomRouterEditor(Option<crate::ai::custom_model_routers::CustomModelRouter>),
    #[cfg(feature = "local_fs")]
    OpenCustomRouterFile(PathBuf),
    OpenExecutionProfileEditor(ClientProfileId),
    SignupAnonymousUser,
}

impl Entity for AISettingsPageView {
    type Event = AISettingsPageEvent;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AISettingsPageAction {
    OpenUrl(String),
    SetVoiceInputToggleKey(VoiceInputToggleKey),
    ToggleGlobalAI,
    ToggleActiveAI,
    ToggleIntelligentAutosuggestions,
    TogglePromptSuggestions,
    ToggleCodeSuggestions,
    ToggleNaturalLanguageAutosuggestions,
    ToggleSharedTitleGeneration,
    ToggleGitOperationsAutogen,
    ToggleAIInputAutoDetection,
    ToggleNLDInTerminal,
    ToggleCLIAgentToolbar,
    ToggleUseAgentToolbar,
    ToggleVoiceInput,
    HyperlinkClick(HyperlinkUrl),
    ToggleCodebaseContext,
    ToggleShowInputHintText,
    ToggleShowAgentTips,
    SetThinkingDisplayMode(ThinkingDisplayMode),
    SetOrchestrationMessageDisplayMode(OrchestrationMessageDisplayMode),
    SetPromptSubmissionMode(PromptSubmissionMode),
    SetLongRunningCommandSubmissionMode(LongRunningCommandSubmissionMode),
    RemoveCLIAgentToolbarEnabledCommand(String),
    RemoveFromCommandExecutionAllowlist(AgentModeCommandExecutionPredicate),
    RemoveFromCommandExecutionDenylist(AgentModeCommandExecutionPredicate),
    OpenAIFactCollection,
    OpenMCPServerCollection,
    OpenExecutionProfileEditor(ClientProfileId),
    SetBaseModel(LLMId),
    SetCodingModel(LLMId),
    /// Called while the user is actively dragging the context window slider.
    ContextWindowSliderDragged(u32),
    /// Called when the user commits a new context window value (slider drop or
    /// input box commit).
    SetContextWindowSize(u32),
    SetAutonomyReadonlyCommandsSetting,
    SetAutonomySupervisedSetting,
    SetCodingPermission(AgentModeCodingPermissionsType),
    RemoveDirectoryFromCodeReadAllowlist(PathBuf),
    ToggleRules,
    ToggleRuleSuggestions,
    ToggleGalaxyDriveContext,
    SetApplyCodeDiffs(ActionPermission),
    SetReadFiles(ActionPermission),
    SetExecuteCommands(ActionPermission),
    SetWriteToPty(WriteToPtyPermission),
    SetMCPPermissions(ActionPermission),
    RemoveFromProfileDirectoryAllowlist(PathBuf),
    RemoveFromProfileCommandDenylist(AgentModeCommandExecutionPredicate),
    RemoveFromProfileCommandAllowlist(AgentModeCommandExecutionPredicate),
    ToggleShowBaseModelPickerInPrompt,
    AddToMCPAllowlist(uuid::Uuid),
    RemoveFromMCPAllowlist(uuid::Uuid),
    AddToMCPDenylist(uuid::Uuid),
    RemoveFromMCPDenylist(uuid::Uuid),
    CreateProfile,
    ToggleBedrockEnabled,
    ToggleOpenAIEnabled,
    ConnectChatGPTSubscription,
    OpenChatGPTDevicePage,
    CopyChatGPTDeviceCode,
    ToggleAcpEnabled,
    FetchOpenAIProviderModels(usize),
    AddOpenAIProvider(ProviderSetupProviderType),
    EditOpenAIProvider(usize),
    ToggleOpenAIProviderEnabled(usize),
    RemoveOpenAIProvider(usize),
    EditBedrockProvider,
    RemoveBedrockProvider,
    RemoveAcpProvider(String),
    ToggleFileBasedMcp,
    ToggleIncludeAgentCommandsInHistory,
    ToggleAgentAttribution,

    // Custom model routers
    #[cfg(feature = "local_fs")]
    OpenAddCustomRouter,

    #[cfg(feature = "local_fs")]
    SetConversationLayout(crate::util::file::external_editor::settings::OpenConversationPreference),
    ToggleCloudHandoff,
    ToggleAmpersandHandoff,
    ToggleAutoHandoffOnSleep,
    ToggleShowConversationHistory,
    ToggleAutoToggleRichInput,
    ToggleAutoOpenRichInputOnCLIAgentStart,
    ToggleAutoDismissRichInputAfterSubmit,
    ToggleSubmitRichInputOnCtrlEnter,
    SetCLIAgentForCommand {
        pattern: String,
        agent: Option<CLIAgent>,
    },
    ToggleCloudAgentComputerUse,
    ToggleCrosscheckEnabled,
}

impl TypedActionView for AISettingsPageView {
    type Action = AISettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AISettingsPageAction::OpenUrl(url) => {
                ctx.open_url(url.as_str());
            }
            AISettingsPageAction::SetVoiceInputToggleKey(key) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.voice_input_toggle_key.set_value(*key, ctx));
                    report_if_error!(settings
                        .explicitly_interacted_with_voice
                        .set_value(true, ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleGlobalAI => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.is_any_ai_enabled.toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleGlobalAI {
                                is_ai_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Global AI setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleActiveAI => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .is_active_ai_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleActiveAI {
                                is_active_ai_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Active AI setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleIntelligentAutosuggestions => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .intelligent_autosuggestions_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleIntelligentAutosuggestionsSetting {
                                is_intelligent_autosuggestions_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Next Command setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::TogglePromptSuggestions => {
                if !UserWorkspaces::as_ref(ctx).is_prompt_suggestions_toggleable() {
                    return;
                }
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .prompt_suggestions_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::TogglePromptSuggestionsSetting {
                                is_prompt_suggestions_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Prompt Suggestions setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleCodeSuggestions => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .code_suggestions_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleCodeSuggestionsSetting {
                                source: ToggleCodeSuggestionsSettingSource::Settings,
                                is_code_suggestions_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Code Suggestions setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleNaturalLanguageAutosuggestions => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .natural_language_autosuggestions_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleNaturalLanguageAutosuggestionsSetting {
                                is_natural_language_autosuggestions_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to set value for Natural Language Autosuggestions setting: {e:?}"
                        );
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleSharedTitleGeneration => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .shared_block_title_generation_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(_new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleSharedBlockTitleGenerationSetting {
                                is_shared_block_title_generation_enabled: true,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to set value for Shared Block Title Generation setting: {e:?}"
                        );
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleGitOperationsAutogen => {
                if !UserWorkspaces::as_ref(ctx).is_git_operations_ai_enabled() {
                    return;
                }
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .git_operations_autogen_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleGitOperationsAutogenSetting {
                                is_git_operations_autogen_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Git Operations Autogen setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleAIInputAutoDetection => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .ai_autodetection_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::AgentModeToggleAutoDetectionSetting {
                                is_autodetection_enabled: new_value,
                                origin: AgentModeAutoDetectionSettingOrigin::SettingsPage
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Input Auto-detection: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleNLDInTerminal => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .nld_in_terminal_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(_new_value) => {}
                    Err(e) => {
                        log::warn!("Failed to set value for NLD in Terminal: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleCLIAgentToolbar => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .should_render_cli_agent_footer
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleCLIAgentToolbarSetting {
                                is_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for CLI Agent Footer setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleAutoToggleRichInput => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.auto_toggle_rich_input.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .auto_open_rich_input_on_cli_agent_start
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleAutoDismissRichInputAfterSubmit => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .auto_dismiss_rich_input_after_submit
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleSubmitRichInputOnCtrlEnter => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.submit_on_ctrl_enter.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleUseAgentToolbar => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .should_render_use_agent_footer_for_user_commands
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleUseAgentToolbarSetting {
                                is_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Use Agent Footer setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleCodebaseContext => {
                match CodeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.codebase_context_enabled.toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleCodebaseContext {
                                is_codebase_context_enabled: new_value
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Codebase Context: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleVoiceInput => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .voice_input_enabled_internal
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleVoiceInputSetting {
                                is_voice_input_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Voice Input: {e:?}");
                    }
                }
                ctx.notify();
            }
            AISettingsPageAction::HyperlinkClick(hyperlink) => {
                ctx.notify();
                ctx.open_url(&hyperlink.url);
            }
            AISettingsPageAction::ToggleShowInputHintText => {
                InputSettings::handle(ctx).update(ctx, |input_settings, ctx| {
                    report_if_error!(input_settings.show_hint_text.toggle_and_save_value(ctx));
                    send_telemetry_from_ctx!(
                        // We purposely keep the FeaturesPageAction event, even though we have moved the setting to AI settings.
                        TelemetryEvent::FeaturesPageAction {
                            action: "ToggleShowInputHintText".to_string(),
                            value: format!("{}", *input_settings.show_hint_text),
                        },
                        ctx
                    );
                });
            }
            AISettingsPageAction::ToggleShowAgentTips => {
                InputSettings::handle(ctx).update(ctx, |input_settings, ctx| match input_settings
                    .show_agent_tips
                    .toggle_and_save_value(ctx)
                {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleShowAgentTips {
                                is_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for Show Agent Tips setting: {e:?}");
                    }
                });
                ctx.notify();
            }
            AISettingsPageAction::SetThinkingDisplayMode(mode) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.thinking_display_mode.set_value(*mode, ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::SetOrchestrationMessageDisplayMode(mode) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .orchestration_message_display_mode
                        .set_value(*mode, ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::SetPromptSubmissionMode(mode) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .default_prompt_submission_mode
                        .set_value(*mode, ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::SetLongRunningCommandSubmissionMode(mode) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .long_running_command_submission_mode
                        .set_value(*mode, ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveCLIAgentToolbarEnabledCommand(command) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.remove_cli_agent_footer_enabled_command(command, ctx);
                });
            }
            AISettingsPageAction::SetCLIAgentForCommand { pattern, agent } => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.set_cli_agent_for_command(pattern, *agent, ctx);
                });
            }
            AISettingsPageAction::RemoveFromCommandExecutionAllowlist(cmd) => {
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(model.remove_command_from_autoexecution_allowlist(cmd, ctx));
                })
            }
            AISettingsPageAction::RemoveFromCommandExecutionDenylist(cmd) => {
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(model.remove_command_from_denylist(cmd, ctx));
                })
            }
            AISettingsPageAction::OpenAIFactCollection => {
                ctx.emit(AISettingsPageEvent::OpenAIFactCollection)
            }
            AISettingsPageAction::OpenMCPServerCollection => {
                ctx.emit(AISettingsPageEvent::OpenMCPServerCollection)
            }
            AISettingsPageAction::OpenExecutionProfileEditor(profile_id) => {
                ctx.emit(AISettingsPageEvent::OpenExecutionProfileEditor(*profile_id))
            }
            AISettingsPageAction::SetBaseModel(id) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    let profile_id = *profiles_model.active_profile(None, ctx).id();
                    profiles_model.set_base_model(profile_id, Some(id.clone()), ctx);
                    profiles_model.set_context_window_limit(profile_id, None, ctx);
                });
                self.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
            AISettingsPageAction::SetCodingModel(id) => {
                LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
                    prefs.update_preferred_coding_llm(id, None, ctx);
                });
            }
            AISettingsPageAction::ContextWindowSliderDragged(value) => {
                if !AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    self.sync_context_window_editor(ctx, true);
                    return;
                }
                if Self::configurable_context_window(ctx).is_some() {
                    self.dragged_context_window_value = Some(*value);
                    let formatted = value.to_string();
                    self.context_window_editor.update(ctx, |editor, ctx| {
                        editor.system_reset_buffer_text(&formatted, ctx);
                    });
                    ctx.notify();
                }
            }
            AISettingsPageAction::SetContextWindowSize(value) => {
                self.dragged_context_window_value = None;
                if !AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    self.sync_context_window_editor(ctx, true);
                    return;
                }
                let Some(cw) = Self::configurable_context_window(ctx) else {
                    return;
                };
                let clamped = (*value).clamp(cw.min, cw.max);
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    let profile_id = *profiles_model.active_profile(None, ctx).id();
                    profiles_model.set_context_window_limit(profile_id, Some(clamped), ctx);
                });
                self.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
            AISettingsPageAction::SetAutonomyReadonlyCommandsSetting
            | AISettingsPageAction::SetAutonomySupervisedSetting => {
                let readonly_cmd_execution_enabled = matches!(
                    action,
                    AISettingsPageAction::SetAutonomyReadonlyCommandsSetting
                );
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    match model.set_should_autoexecute_readonly_commands(
                        readonly_cmd_execution_enabled,
                        ctx,
                    ) {
                        Ok(_) => {
                            send_telemetry_from_ctx!(
                                TelemetryEvent::ToggledAgentModeAutoexecuteReadonlyCommandsSetting {
                                    src: AutonomySettingToggleSource::SettingsPage,
                                    enabled: readonly_cmd_execution_enabled,
                                },
                                ctx);
                        }
                        Err(e) => report_error!(e),
                    }
                });
            }
            AISettingsPageAction::SetCodingPermission(p) => {
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    match model.set_coding_permissions(*p, ctx) {
                        Ok(_) => {
                            send_telemetry_from_ctx!(
                                TelemetryEvent::ChangedAgentModeCodingPermissions {
                                    src: AutonomySettingToggleSource::SettingsPage,
                                    new: *p,
                                },
                                ctx
                            );
                        }
                        Err(e) => report_error!(e),
                    }
                });
            }
            AISettingsPageAction::SetApplyCodeDiffs(permission) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    model.set_apply_code_diffs(*profile.id(), permission, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::SetReadFiles(permission) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    model.set_read_files(*profile.id(), permission, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::SetExecuteCommands(permission) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    model.set_execute_commands(*profile.id(), permission, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::SetWriteToPty(permission) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    model.set_write_to_pty(*profile.id(), permission, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::SetMCPPermissions(permission) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    model.set_mcp_permissions(*profile.id(), permission, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveDirectoryFromCodeReadAllowlist(dir) => {
                BlocklistAIPermissions::handle(ctx).update(ctx, |model, ctx| {
                    report_if_error!(
                        model.remove_filepath_from_code_read_allowlist(dir.to_owned(), ctx)
                    );
                });
            }
            AISettingsPageAction::ToggleRules => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let _ = settings.memory_enabled.toggle_and_save_value(ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleRuleSuggestions => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let _ = settings
                        .rule_suggestions_enabled_internal
                        .toggle_and_save_value(ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleGalaxyDriveContext => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let _ = settings
                        .warp_drive_context_enabled
                        .toggle_and_save_value(ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveFromProfileDirectoryAllowlist(path_buf) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.remove_from_directory_allowlist(
                        *profile_id,
                        &PathBuf::from(path_buf),
                        ctx,
                    );
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveFromProfileCommandDenylist(cmd) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();

                    model.remove_from_command_denylist(*profile_id, cmd, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveFromProfileCommandAllowlist(command) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();

                    model.remove_from_command_allowlist(*profile_id, command, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleShowBaseModelPickerInPrompt => {
                SessionSettings::handle(ctx).update(ctx, |settings, ctx| {
                    if let Err(e) = settings
                        .show_model_selectors_in_prompt
                        .toggle_and_save_value(ctx)
                    {
                        log::warn!(
                            "Failed to set value for Show Base Model Picker in Prompt: {e:?}"
                        );
                    }
                });
                ctx.notify();
            }
            AISettingsPageAction::AddToMCPAllowlist(id) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.add_to_mcp_allowlist(*profile_id, id, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveFromMCPAllowlist(id) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.remove_from_mcp_allowlist(*profile_id, id, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::AddToMCPDenylist(id) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.add_to_mcp_denylist(*profile_id, id, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::RemoveFromMCPDenylist(id) => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |model, ctx| {
                    let profile = model.default_profile(ctx);
                    let profile_id = profile.id();
                    model.remove_from_mcp_denylist(*profile_id, id, ctx);
                });
                ctx.notify();
            }
            AISettingsPageAction::CreateProfile => {
                let new_profile_id = AIExecutionProfilesModel::handle(ctx)
                    .update(ctx, |model, ctx| model.create_profile(ctx));

                if let Some(profile_id) = new_profile_id {
                    self.profile_views = Self::create_profile_views(ctx);
                    ctx.emit(AISettingsPageEvent::OpenExecutionProfileEditor(profile_id));
                }
                ctx.notify();
            }
            AISettingsPageAction::ToggleCloudAgentComputerUse => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .cloud_agent_computer_use_enabled
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleCrosscheckEnabled => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.crosscheck_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleBedrockEnabled => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    if !*settings.bedrock_enabled.value() {
                        report_if_error!(settings.bedrock_models.set_value(Vec::new(), ctx));
                    }
                    report_if_error!(settings.bedrock_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleOpenAIEnabled => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.openai_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ConnectChatGPTSubscription => {
                #[cfg(not(target_family = "wasm"))]
                ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.connect(ctx));
                ctx.notify();
            }
            AISettingsPageAction::OpenChatGPTDevicePage => {
                // No-op: device-code flow removed in favor of browser OAuth.
            }
            AISettingsPageAction::CopyChatGPTDeviceCode => {
                // No-op: device-code flow removed in favor of browser OAuth.
            }
            AISettingsPageAction::ToggleAcpEnabled => {
                if cfg!(unix) {
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        report_if_error!(settings.acp_enabled.toggle_and_save_value(ctx));
                    });
                    ctx.notify();
                }
            }
            AISettingsPageAction::FetchOpenAIProviderModels(provider_index) => {
                self.fetch_openai_provider_models(*provider_index, ctx);
            }
            AISettingsPageAction::AddOpenAIProvider(provider_type) => {
                self.begin_inline_provider_create(*provider_type, ctx);
            }
            AISettingsPageAction::EditOpenAIProvider(provider_index) => {
                self.begin_inline_provider_edit(*provider_index, ctx);
            }
            AISettingsPageAction::ToggleOpenAIProviderEnabled(provider_index) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.openai_providers.value().clone();
                    if let Some(provider) = providers.get_mut(*provider_index) {
                        provider.enabled = !provider.enabled;
                    }
                    report_if_error!(settings.openai_providers.set_value(providers, ctx));
                });
            }
            AISettingsPageAction::RemoveOpenAIProvider(provider_index) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.openai_providers.value().clone();
                    if *provider_index < providers.len() {
                        providers.remove(*provider_index);
                        report_if_error!(settings.openai_providers.set_value(providers, ctx));
                    }
                });
                self.rebuild_active_subpage(ctx);
            }
            AISettingsPageAction::EditBedrockProvider => {
                self.begin_inline_bedrock_setup(ctx);
            }
            AISettingsPageAction::RemoveBedrockProvider => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.bedrock_enabled.set_value(false, ctx));
                    report_if_error!(settings
                        .bedrock_auth_method
                        .set_value(crate::settings::ai::BedrockAuthMethod::default(), ctx));
                    report_if_error!(settings
                        .bedrock_profile
                        .set_value("default".to_string(), ctx));
                    report_if_error!(settings.bedrock_region.set_value(String::new(), ctx));
                    report_if_error!(settings.bedrock_cross_region_inference.set_value(true, ctx));
                    report_if_error!(settings.bedrock_auto_login.set_value(true, ctx));
                    report_if_error!(settings
                        .bedrock_auth_refresh_command
                        .set_value("aws sso login".to_string(), ctx));
                    report_if_error!(settings.bedrock_access_key_id.set_value(String::new(), ctx));
                    report_if_error!(settings
                        .bedrock_secret_access_key
                        .set_value(String::new(), ctx));
                    report_if_error!(settings.bedrock_models.set_value(Vec::new(), ctx));
                    report_if_error!(settings
                        .bedrock_connection_name
                        .set_value("AWS Bedrock".to_string(), ctx,));
                });
                self.rebuild_active_subpage(ctx);
            }
            AISettingsPageAction::RemoveAcpProvider(provider_id) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut providers = settings.acp_providers.value().clone();
                    if providers.is_empty() && provider_id == "legacy" {
                        report_if_error!(settings.acp_enabled.set_value(false, ctx));
                        report_if_error!(settings.acp_agent_id.set_value("codex".to_string(), ctx));
                        report_if_error!(settings.acp_agent_command.set_value(String::new(), ctx));
                        report_if_error!(settings.acp_agent_args.set_value(Vec::new(), ctx));
                        report_if_error!(settings
                            .acp_connection_name
                            .set_value("ACP agent runtime".to_string(), ctx,));
                        report_if_error!(settings.acp_agents.set_value(Vec::new(), ctx));
                        return;
                    }

                    providers.retain(|provider| provider.id != provider_id.as_str());
                    report_if_error!(settings.acp_providers.set_value(providers.clone(), ctx));

                    if let Some(first_provider) = providers.first() {
                        report_if_error!(settings.acp_enabled.set_value(true, ctx));
                        report_if_error!(settings
                            .acp_agent_id
                            .set_value(first_provider.agent_id.clone(), ctx));
                        report_if_error!(settings
                            .acp_agent_command
                            .set_value(first_provider.command.clone(), ctx));
                        report_if_error!(settings
                            .acp_agent_args
                            .set_value(first_provider.args.clone(), ctx));
                        report_if_error!(settings
                            .acp_connection_name
                            .set_value(first_provider.name.clone(), ctx));
                    } else {
                        report_if_error!(settings.acp_enabled.set_value(false, ctx));
                        report_if_error!(settings.acp_agent_id.set_value("codex".to_string(), ctx));
                        report_if_error!(settings.acp_agent_command.set_value(String::new(), ctx));
                        report_if_error!(settings.acp_agent_args.set_value(Vec::new(), ctx));
                        report_if_error!(settings
                            .acp_connection_name
                            .set_value("ACP agent runtime".to_string(), ctx,));
                        report_if_error!(settings.acp_agents.set_value(Vec::new(), ctx));
                    }
                });
                self.clear_inline_provider_setup(ctx);
            }
            AISettingsPageAction::ToggleFileBasedMcp => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.file_based_mcp_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleIncludeAgentCommandsInHistory => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .include_agent_commands_in_history
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            #[cfg(feature = "local_fs")]
            AISettingsPageAction::SetConversationLayout(layout) => {
                crate::util::file::external_editor::EditorSettings::handle(ctx).update(
                    ctx,
                    |settings, ctx| {
                        report_if_error!(settings
                            .open_conversation_layout_preference
                            .set_value(*layout, ctx));
                    },
                );
                send_telemetry_from_ctx!(
                    TelemetryEvent::FeaturesPageAction {
                        action: "SetConversationLayout".to_string(),
                        value: format!("{layout:?}")
                    },
                    ctx
                );
                ctx.notify();
            }
            AISettingsPageAction::ToggleShowConversationHistory => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .show_conversation_history
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            #[cfg(feature = "local_fs")]
            AISettingsPageAction::OpenAddCustomRouter => {
                ctx.emit(AISettingsPageEvent::OpenCustomRouterEditor(None));
            }
            AISettingsPageAction::ToggleCloudHandoff => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .should_force_disable_cloud_handoff
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleAmpersandHandoff => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .should_force_disable_ampersand_handoff
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleAutoHandoffOnSleep => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .auto_handoff_on_sleep_enabled
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            AISettingsPageAction::ToggleAgentAttribution => {
                // The updated value syncs to warp-server automatically via
                // `CloudPreferencesSyncer` as a `JsonPreference` GSO keyed
                // `Global_AgentAttributionEnabled`; no bespoke server call needed.
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .agent_attribution_enabled
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
        }
    }
}

impl SettingsPageMeta for AISettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::AI
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::AgentMode.is_enabled()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<AISettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AISettingsPageView>) -> Self {
        SettingsPageViewHandle::AI(view_handle)
    }
}

fn render_ai_setting_toggle<S: Setting>(
    label: impl Into<String>,
    action: AISettingsPageAction,
    is_setting_enabled: bool,
    is_setting_toggleable: bool,
    switch_state: SwitchStateHandle,
    tooltip_states: &RefCell<HashMap<String, MouseStateHandle>>,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    build_toggle_element(
        render_body_item_label::<AISettingsPageAction>(
            label.into(),
            Some(styles::header_font_color(is_setting_toggleable, app)),
            None,
            LocalOnlyIconState::for_setting(
                S::storage_key(),
                S::sync_to_cloud(),
                &mut tooltip_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
        ),
        render_ai_feature_switch(
            switch_state,
            is_setting_enabled,
            is_setting_toggleable,
            action,
            app,
        ),
        appearance,
        None,
    )
}

fn render_ai_setting_label<S: Setting>(
    label: impl Into<String>,
    is_setting_toggleable: bool,
    tooltip_states: &RefCell<HashMap<String, MouseStateHandle>>,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    Container::new(render_body_item_label::<AISettingsPageAction>(
        label.into(),
        Some(styles::header_font_color(is_setting_toggleable, app)),
        None,
        LocalOnlyIconState::for_setting(
            S::storage_key(),
            S::sync_to_cloud(),
            &mut tooltip_states.borrow_mut(),
            app,
        ),
        ToggleState::Enabled,
        appearance,
    ))
    .with_margin_bottom(HEADER_PADDING)
    .finish()
}

fn render_ai_setting_description(
    description: impl Into<Cow<'static, str>>,
    is_setting_toggleable: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let default_font_size = Appearance::as_ref(app).ui_font_size();
    render_ai_setting_description_with_font_size(
        description,
        default_font_size,
        is_setting_toggleable,
        app,
    )
}

fn render_ai_setting_description_with_font_size(
    description: impl Into<Cow<'static, str>>,
    font_size: f32,
    is_setting_toggleable: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let ui_builder = Appearance::as_ref(app).ui_builder();
    ui_builder
        .paragraph(description)
        .with_style(UiComponentStyles {
            font_size: Some(font_size),
            font_color: Some(styles::description_font_color(is_setting_toggleable, app).into()),
            margin: Some(
                Coords::default()
                    .top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                    .bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                    .right(styles::TOGGLE_WIDTH_MARGIN),
            ),
            ..Default::default()
        })
        .build()
        .finish()
}

fn render_toolbar_layout_editor(
    editor: &ViewHandle<AgentToolbarInlineEditor>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let label = Container::new(
        appearance
            .ui_builder()
            .span("Toolbar layout".to_string())
            .with_style(UiComponentStyles {
                font_size: Some(CONTENT_FONT_SIZE),
                ..Default::default()
            })
            .build()
            .finish(),
    )
    .with_margin_bottom(4.)
    .finish();
    let editor = Container::new(ChildView::new(editor).finish())
        .with_margin_bottom(16.)
        .finish();

    Flex::column().with_child(label).with_child(editor).finish()
}

fn render_ai_feature_switch(
    state_handle: SwitchStateHandle,
    is_setting_enabled: bool,
    is_setting_toggleable: bool,
    toggle_action: AISettingsPageAction,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let ui_builder = appearance.ui_builder();
    ui_builder
        .switch(state_handle)
        .check(is_setting_enabled)
        .with_disabled(!is_setting_toggleable)
        .with_disabled_styles(UiComponentStyles {
            background: Some(Fill::Solid(internal_colors::neutral_4(appearance.theme()))),
            foreground: Some(Fill::Solid(internal_colors::neutral_5(appearance.theme()))),
            ..Default::default()
        })
        .build()
        .on_click(move |ctx, _, _| {
            if !is_setting_toggleable {
                return;
            }
            ctx.dispatch_typed_action(toggle_action.clone());
        })
        .finish()
}

fn render_ai_list(
    header: &str,
    description: &str,
    input_list: Box<dyn Element>,
    view: &AISettingsPageView,
    ai_settings: &AISettings,
    app: &AppContext,
) -> Box<dyn Element> {
    let setting_header = render_ai_setting_label::<AgentModeCommandExecutionDenylist>(
        header.to_string(),
        ai_settings.is_any_ai_enabled(app),
        &view.local_only_icon_tooltip_states,
        app,
    );

    let description = render_ai_setting_description(
        description.to_string(),
        ai_settings.is_any_ai_enabled(app),
        app,
    );

    Flex::column()
        .with_child(setting_header)
        .with_child(Container::new(description).with_margin_bottom(-8.).finish())
        .with_child(input_list)
        .finish()
}

#[derive(Default)]
struct ActiveAIWidget {
    intelligent_autosuggestions_toggle: SwitchStateHandle,
    prompt_suggestions_toggle: SwitchStateHandle,
    code_suggestions_toggle: SwitchStateHandle,
    natural_language_autosuggestions_toggle: SwitchStateHandle,
    shared_block_title_generation_toggle: SwitchStateHandle,
    git_operations_autogen_toggle: SwitchStateHandle,
}

impl ActiveAIWidget {
    fn is_next_command_toggleable(&self, app: &AppContext) -> bool {
        UserWorkspaces::as_ref(app).is_next_command_enabled()
            && AISettings::as_ref(app)
                .intelligent_autosuggestions_enabled_internal
                .is_supported_on_current_platform()
    }

    fn is_prompt_suggestions_toggleable(&self, app: &AppContext) -> bool {
        UserWorkspaces::as_ref(app).is_prompt_suggestions_toggleable()
            && AISettings::as_ref(app)
                .prompt_suggestions_enabled_internal
                .is_supported_on_current_platform()
    }

    fn is_suggested_code_banners_toggleable(&self, app: &AppContext) -> bool {
        (self.is_prompt_suggestions_toggleable(app)
            || UserWorkspaces::as_ref(app).is_code_suggestions_toggleable())
            && AISettings::as_ref(app)
                .code_suggestions_enabled_internal
                .is_supported_on_current_platform()
    }

    fn is_natural_language_autosuggestions_toggleable(&self, app: &AppContext) -> bool {
        FeatureFlag::PredictAMQueries.is_enabled()
            && AISettings::as_ref(app)
                .natural_language_autosuggestions_enabled_internal
                .is_supported_on_current_platform()
    }

    // TODO: Check if the user's enterprise billing policy allows toggling this feature.
    fn is_shared_block_title_generation_toggleable(&self, app: &AppContext) -> bool {
        FeatureFlag::SharedBlockTitleGeneration.is_enabled()
            && AISettings::as_ref(app)
                .shared_block_title_generation_enabled_internal
                .is_supported_on_current_platform()
            && (!UserWorkspaces::as_ref(app)
                .current_team()
                .is_some_and(|team| {
                    team.billing_metadata.customer_type == CustomerType::Enterprise
                })
                // Override the enterprise check for dogfood builds, as our dogfood team
                // is an enterprise team.
                || ChannelState::channel().is_dogfood())
    }

    fn is_git_operations_autogen_toggleable(&self, app: &AppContext) -> bool {
        FeatureFlag::GitOperationsInCodeReview.is_enabled()
            && AISettings::as_ref(app)
                .git_operations_autogen_enabled_internal
                .is_supported_on_current_platform()
            && UserWorkspaces::as_ref(app).is_git_operations_ai_enabled()
    }

    fn render_next_command_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);

        Flex::column()
            .with_child(
                render_ai_setting_toggle::<IntelligentAutosuggestionsEnabled>(
                    "Next Command",
                    AISettingsPageAction::ToggleIntelligentAutosuggestions,
                    *ai_settings.intelligent_autosuggestions_enabled_internal,
                    is_toggleable,
                    self.intelligent_autosuggestions_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
            )
            .with_child(render_ai_setting_description(
                NEXT_COMMAND_DESCRIPTION,
                is_toggleable,
                app,
            ))
            .finish()
    }

    fn render_prompt_suggestions_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);
        Flex::column()
            .with_child(
                render_ai_setting_toggle::<AgentModeQuerySuggestionsEnabled>(
                    "Prompt Suggestions",
                    AISettingsPageAction::TogglePromptSuggestions,
                    *ai_settings.prompt_suggestions_enabled_internal,
                    is_toggleable,
                    self.prompt_suggestions_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
            )
            .with_child(render_ai_setting_description(
                PROMPT_SUGGESTIONS_DESCRIPTION,
                is_toggleable,
                app,
            ))
            .finish()
    }

    fn render_suggested_code_banners_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);
        Flex::column()
            .with_child(
                render_ai_setting_toggle::<AgentModeQuerySuggestionsEnabled>(
                    "Suggested Code Banners",
                    AISettingsPageAction::ToggleCodeSuggestions,
                    *ai_settings.code_suggestions_enabled_internal,
                    is_toggleable,
                    self.code_suggestions_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
            )
            .with_child(render_ai_setting_description(
                SUGGESTED_CODE_BANNERS_DESCRIPTION,
                is_toggleable,
                app,
            ))
            .finish()
    }

    fn render_natural_language_autosuggestions_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);
        Flex::column()
            .with_child(render_ai_setting_toggle::<
                NaturalLanguageAutosuggestionsEnabled,
            >(
                "Natural Language Autosuggestions",
                AISettingsPageAction::ToggleNaturalLanguageAutosuggestions,
                *ai_settings.natural_language_autosuggestions_enabled_internal,
                is_toggleable,
                self.natural_language_autosuggestions_toggle.clone(),
                &view.local_only_icon_tooltip_states,
                app,
            ))
            .with_child(render_ai_setting_description(
                NATURAL_LANGUAGE_AUTOSUGGESTIONS,
                is_toggleable,
                app,
            ))
            .finish()
    }

    fn render_shared_block_title_generation_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);
        Flex::column()
            .with_child(
                render_ai_setting_toggle::<SharedBlockTitleGenerationEnabled>(
                    "Shared Block Title Generation",
                    AISettingsPageAction::ToggleSharedTitleGeneration,
                    *ai_settings.shared_block_title_generation_enabled_internal,
                    is_toggleable,
                    self.shared_block_title_generation_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
            )
            .with_child(render_ai_setting_description(
                SHARED_BLOCK_TITLE_GENERATION_DESCRIPTION,
                is_toggleable,
                app,
            ))
            .finish()
    }

    fn render_git_operations_autogen_section(
        &self,
        view: &AISettingsPageView,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_active_ai_enabled(app);
        Flex::column()
            .with_child(render_ai_setting_toggle::<GitOperationsAutogenEnabled>(
                "Commit & Pull Request Generation",
                AISettingsPageAction::ToggleGitOperationsAutogen,
                *ai_settings.git_operations_autogen_enabled_internal,
                is_toggleable,
                self.git_operations_autogen_toggle.clone(),
                &view.local_only_icon_tooltip_states,
                app,
            ))
            .with_child(render_ai_setting_description(
                GIT_OPERATIONS_AUTOGEN_DESCRIPTION,
                is_toggleable,
                app,
            ))
            .finish()
    }
}

impl SettingsWidget for ActiveAIWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "active ai a.i. next command prompt suggestions code diffs suggested banners passive unit tests commit pull request pr git code review autogen generate"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        self.is_next_command_toggleable(app)
            || self.is_prompt_suggestions_toggleable(app)
            || self.is_suggested_code_banners_toggleable(app)
            || self.is_natural_language_autosuggestions_toggleable(app)
            || self.is_shared_block_title_generation_toggleable(app)
            || self.is_git_operations_autogen_toggleable(app)
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                Container::new(build_sub_header(appearance, "AI suggestions", None).finish())
                    .with_padding_bottom(HEADER_PADDING)
                    .finish(),
            );

        if self.is_next_command_toggleable(app) {
            column.add_child(self.render_next_command_section(view, app));
        }

        if self.is_prompt_suggestions_toggleable(app) {
            column.add_child(self.render_prompt_suggestions_section(view, app));
        }

        if self.is_suggested_code_banners_toggleable(app) {
            column.add_child(self.render_suggested_code_banners_section(view, app));
        }

        if self.is_natural_language_autosuggestions_toggleable(app) {
            column.add_child(self.render_natural_language_autosuggestions_section(view, app));
        }

        if self.is_shared_block_title_generation_toggleable(app) {
            column.add_child(self.render_shared_block_title_generation_section(view, app));
        }

        if self.is_git_operations_autogen_toggleable(app) {
            column.add_child(self.render_git_operations_autogen_section(view, app));
        }

        column.finish()
    }
}

#[derive(Default)]
struct AgentsWidget {
    codebase_context_toggle: SwitchStateHandle,
    codebase_context_link_index: HighlightedHyperlink,
    show_in_prompt_checkbox: MouseStateHandle,
}

impl SettingsWidget for AgentsWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        if MCPServersWidget::should_show_mcp() {
            "ai a.i. agent autonomy profiles allowlist denylist autoexecute permissions models llms planning mcp server"
        } else {
            "ai a.i. agent autonomy profiles allowlist denylist autoexecute permissions models llms planning"
        }
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let mut column = Flex::column();

        if FeatureFlag::ProfilesDesignRevamp.is_enabled() {
            column.add_child(
                Container::new(self.render_profiles_section(view, ai_settings, appearance, app))
                    .with_margin_bottom(8.)
                    .finish(),
            );
        } else {
            // Legacy layout: show Agents header + Models + Permissions
            let mut agents_header = Flex::column();
            agents_header.add_child(
                build_sub_header(
                    appearance,
                    "Agents",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            );
            agents_header.add_child(render_ai_setting_description(
                "Set the boundaries for how your Agent operates. Choose what it can access, how much autonomy it has, and when it must ask for your approval. You can also fine-tune behavior around natural language input, codebase awareness, and more.",
                ai_settings.is_any_ai_enabled(app),
                app,
            ));
            let agents_header = agents_header.finish();
            column.add_children([
                render_separator(appearance),
                Container::new(agents_header)
                    .with_margin_bottom(8.)
                    .finish(),
            ]);
            column.add_children([
                Container::new(self.render_models_section(view, ai_settings, appearance, app))
                    .with_margin_bottom(8.)
                    .finish(),
                Container::new(self.render_permissions_section(view, ai_settings, appearance, app))
                    .with_margin_bottom(8.)
                    .finish(),
            ]);
        };

        column.finish()
    }
}

impl AgentsWidget {
    fn render_profiles_section(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let header_and_description = Flex::column()
            .with_child(
                build_sub_header(
                    appearance,
                    "Profiles",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .finish(),
            )
            .with_child(
                Container::new(
                    render_ai_setting_description(
                        "Profiles let you define how your Agent operates — from the actions it can take and when it needs approval, to the models it uses for tasks like coding and planning. You can also scope them to individual projects.",
                        is_any_ai_enabled,
                        app,
                    )
                )
                .with_margin_top(12.)
                .finish()
            )
            .finish();

        let mut profiles_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(Shrinkable::new(1., header_and_description).finish());

        if FeatureFlag::MultiProfile.is_enabled() {
            profiles_row.add_child(
                Container::new(view.add_profile_button.as_ref(app).render(app))
                    .with_margin_left(16.)
                    .finish(),
            );
        }

        let profiles_header = Container::new(profiles_row.finish())
            .with_margin_bottom(12.0)
            .finish();

        let mut profile_elements = vec![profiles_header];

        for profile_view in &view.profile_views {
            profile_elements.push(
                Container::new(ChildView::new(profile_view).finish())
                    .with_margin_bottom(8.)
                    .finish(),
            );
        }

        Flex::column().with_children(profile_elements).finish()
    }

    fn render_models_section(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);
        let model_subheader = Container::new(render_custom_size_header(
            appearance,
            "Models",
            14.0,
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        ))
        .with_margin_bottom(8.0)
        .finish();

        let base_model_setting =
            Container::new(self.render_base_model_setting(view, ai_settings, appearance, app))
                .with_margin_bottom(8.0)
                .finish();

        let mut children = vec![model_subheader, base_model_setting];
        if let Some(context_window_setting) =
            self.render_context_window_setting(view, ai_settings, appearance, app)
        {
            children.push(
                Container::new(context_window_setting)
                    .with_margin_bottom(8.0)
                    .finish(),
            );
        }

        Flex::column().with_children(children).finish()
    }

    /// Renders the context window slider + numeric input row shown below the
    /// base model dropdown. Returns `None` if the active base model does not
    /// advertise a configurable context window or global AI is disabled.
    fn render_context_window_setting(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        if !ai_settings.is_any_ai_enabled(app) {
            return None;
        }
        let cw = AISettingsPageView::configurable_context_window(app)?;
        let min = cw.min;
        let max = cw.max;

        let label = Container::new(render_body_item_label::<AISettingsPageAction>(
            "Context window (tokens)".to_string(),
            None,
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        ))
        .with_margin_bottom(4.0)
        .finish();

        let min_label = appearance
            .ui_builder()
            .span(format!("{min}"))
            .with_style(UiComponentStyles {
                font_size: Some(CONTENT_FONT_SIZE),
                ..Default::default()
            })
            .build()
            .finish();

        let max_label = appearance
            .ui_builder()
            .span(format!("{max}"))
            .with_style(UiComponentStyles {
                font_size: Some(CONTENT_FONT_SIZE),
                ..Default::default()
            })
            .build()
            .finish();

        let current_value = AISettingsPageView::current_context_window_display_value(app)
            .unwrap_or(cw.default_max)
            .clamp(min, max);
        let slider = appearance
            .ui_builder()
            .slider(view.context_window_slider_state.clone())
            .with_range(min as f32..max as f32)
            .with_default_value(current_value as f32)
            .with_style(UiComponentStyles {
                width: Some(CONTEXT_WINDOW_SLIDER_WIDTH),
                margin: Some(Coords::default().left(8.).right(8.)),
                ..Default::default()
            })
            .on_drag(|ctx, _, val| {
                ctx.dispatch_typed_action(AISettingsPageAction::ContextWindowSliderDragged(
                    val.round() as u32,
                ));
            })
            .on_change(|ctx, _, val| {
                ctx.dispatch_typed_action(AISettingsPageAction::SetContextWindowSize(
                    val.round() as u32
                ));
            })
            .build()
            .finish();

        let context_window_editor = view.context_window_editor.clone();
        let input_box = Dismiss::new(
            appearance
                .ui_builder()
                .text_input(view.context_window_editor.clone())
                .with_style(UiComponentStyles {
                    width: Some(CONTEXT_WINDOW_INPUT_BOX_WIDTH),
                    padding: Some(Coords {
                        top: 6.,
                        bottom: 6.,
                        left: 10.,
                        right: 10.,
                    }),
                    margin: Some(Coords::default().left(12.)),
                    background: Some(appearance.theme().surface_2().into()),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .on_dismiss(move |ctx, app| {
            let buffer_text = context_window_editor.as_ref(app).buffer_text(app);
            let cleaned: String = buffer_text
                .chars()
                .filter(|c| !c.is_whitespace() && *c != ',')
                .collect();
            if let Ok(parsed) = cleaned.parse::<u32>() {
                ctx.dispatch_typed_action(AISettingsPageAction::SetContextWindowSize(parsed));
            }
        })
        .finish();

        let row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(min_label)
            .with_child(Shrinkable::new(1., slider).finish())
            .with_child(max_label)
            .with_child(input_box)
            .finish();

        let mut column = Flex::column().with_child(label).with_child(row);
        if AISettingsPageView::active_profile_data(app)
            .should_show_long_context_pricing_warning(view.dragged_context_window_value, app)
        {
            column.add_child(render_warning_box(
                WarningBoxConfig::formatted_title(long_context_pricing_warning_title()),
                appearance,
            ));
        }

        Some(column.finish())
    }

    fn render_permissions_section(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);
        let permissions_subheader = Container::new(render_custom_size_header(
            appearance,
            "Permissions",
            14.0,
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        ))
        .with_margin_bottom(4.0)
        .finish();

        let code_diff_setting =
            BlocklistAIPermissions::as_ref(app).get_apply_code_diffs_setting(app, None);
        let code_diffs = self.render_execution_profile_dropdown(
            "Apply code diffs",
            Icon::Code2,
            code_diff_setting.description(),
            &view.apply_code_diffs_dropdown_menu,
            ai_settings,
            appearance,
            app,
        );

        let read_files_setting =
            BlocklistAIPermissions::as_ref(app).get_read_files_setting(app, None);
        let mut read_files_flex = Flex::column().with_main_axis_size(MainAxisSize::Min);
        read_files_flex.add_child(self.render_execution_profile_dropdown(
            "Read files",
            Icon::Notebook,
            read_files_setting.description(),
            &view.read_files_dropdown_menu,
            ai_settings,
            appearance,
            app,
        ));

        if read_files_setting == ActionPermission::AlwaysAsk {
            let directory_allowlist =
                BlocklistAIPermissions::as_ref(app).get_read_files_allowlist(app, None);
            read_files_flex.add_child(
                Container::new(Self::render_directory_allowlist(
                    directory_allowlist,
                    view,
                    ai_settings,
                    appearance,
                    app,
                ))
                .with_margin_bottom(HEADER_PADDING)
                .finish(),
            );
        }
        let read_files = read_files_flex.finish();

        let execute_commands_setting =
            BlocklistAIPermissions::as_ref(app).get_execute_commands_setting(app, None);
        let mut execute_commands_flex = Flex::column().with_main_axis_size(MainAxisSize::Min);
        execute_commands_flex.add_child(self.render_execution_profile_dropdown(
            "Execute commands",
            Icon::Terminal,
            execute_commands_setting.description(),
            &view.execute_commands_dropdown_menu,
            ai_settings,
            appearance,
            app,
        ));

        if execute_commands_setting == ActionPermission::AlwaysAsk
            || execute_commands_setting == ActionPermission::AgentDecides
        {
            let command_allowlist =
                BlocklistAIPermissions::as_ref(app).get_execute_commands_allowlist(app, None);
            execute_commands_flex.add_child(
                Container::new(Self::render_command_allowlist(
                    command_allowlist,
                    view,
                    ai_settings,
                    appearance,
                    app,
                ))
                .with_margin_bottom(8.)
                .finish(),
            );
        }

        if execute_commands_setting != ActionPermission::AlwaysAsk {
            let command_denylist = Container::new(Self::render_command_denylist(
                BlocklistAIPermissions::as_ref(app).get_execute_commands_denylist(app, None),
                view,
                ai_settings,
                appearance,
                app,
            ))
            .with_margin_bottom(8.)
            .finish();
            execute_commands_flex.add_child(command_denylist);
        }
        let execute_commands = execute_commands_flex.finish();

        let mut widget_children = vec![permissions_subheader];

        if UserWorkspaces::as_ref(app)
            .ai_autonomy_settings()
            .has_any_overrides()
        {
            widget_children.push(
                Container::new(render_settings_info_banner(
                    "Some of your permissions are managed by your workspace.",
                    None,
                    appearance,
                ))
                .with_margin_bottom(12.0)
                .finish(),
            );
        }

        widget_children.extend([code_diffs, read_files, execute_commands]);

        let write_to_pty_setting =
            BlocklistAIPermissions::as_ref(app).get_write_to_pty_setting(app, None);
        let write_to_pty = self.render_execution_profile_dropdown(
            "Interact with running commands",
            Icon::Workflow,
            write_to_pty_setting.description(),
            &view.write_to_pty_autonomy_dropdown_menu,
            ai_settings,
            appearance,
            app,
        );
        widget_children.push(write_to_pty);

        if MCPServersWidget::should_show_mcp() {
            let mcp_permissions = self.render_mcp_permissions(view, ai_settings, appearance, app);
            widget_children.push(mcp_permissions);
        }

        if !FeatureFlag::FullSourceCodeEmbedding.is_enabled() {
            let codebase_context = Self::render_codebase_context_outline_generation_setting(
                self.codebase_context_toggle.clone(),
                self.codebase_context_link_index.clone(),
                view,
                ai_settings,
                appearance,
                app,
            );
            widget_children.push(codebase_context);
        }

        Flex::column().with_children(widget_children).finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_execution_profile_dropdown(
        &self,
        header_text: &str,
        header_icon: Icon,
        permission_description: &'static str,
        dropdown_menu: &ViewHandle<Dropdown<AISettingsPageAction>>,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &galaxyui::AppContext,
    ) -> Box<dyn Element> {
        let header = Container::new(render_body_item_label_with_icon::<AISettingsPageAction>(
            header_text.into(),
            header_icon,
            Some(styles::header_font_color(
                ai_settings.is_any_ai_enabled(app),
                app,
            )),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        ))
        .finish();

        let description_color = appearance.theme().disabled_ui_text_color();
        let alert_icon = Container::new(
            ConstrainedBox::new(
                Icon::AlertCircle
                    .to_galaxyui_icon(
                        appearance
                            .theme()
                            .sub_text_color(appearance.theme().surface_2()),
                    )
                    .finish(),
            )
            .with_width(14.)
            .with_height(14.)
            .finish(),
        )
        .with_margin_right(4.)
        .finish();
        let text = Text::new(
            permission_description,
            appearance.ui_font_family(),
            appearance.ui_font_size(),
        )
        .with_color(description_color.into())
        .finish();
        let description = Flex::row()
            .with_children([alert_icon, Shrinkable::new(1.0, text).finish()])
            .finish();

        Container::new(
            Flex::column()
                .with_child(header)
                .with_child(ChildView::new(dropdown_menu).finish())
                .with_child(description)
                .finish(),
        )
        .with_margin_bottom(12.)
        .finish()
    }

    fn render_command_denylist(
        command_denylist: Vec<AgentModeCommandExecutionPredicate>,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_disabled = !ai_settings.is_any_ai_enabled(app);
        let org_denylist = BlocklistAIPermissions::get_org_execute_commands_denylist(app);
        let mut tooltip_idx = 0usize;
        let list = render_input_list(
            None,
            command_denylist
                .into_iter()
                .zip(view.command_denylist_mouse_state_handles.clone())
                .rev()
                .map(|(cmd, mouse_state_handle)| {
                    let is_org = org_denylist.contains(&cmd);
                    let tooltip_mouse_state = if is_org {
                        let handle = view
                            .command_denylist_tooltip_mouse_state_handles
                            .get(tooltip_idx)
                            .cloned();
                        tooltip_idx += 1;
                        handle
                    } else {
                        None
                    };
                    InputListItem {
                        item: cmd.to_string(),
                        mouse_state_handle,
                        on_remove_action: AISettingsPageAction::RemoveFromProfileCommandDenylist(
                            cmd,
                        ),
                        is_disabled: is_org || ai_disabled,
                        tooltip_mouse_state,
                    }
                }),
            Some(&view.command_denylist_editor),
            appearance,
        );
        render_ai_list(
            "Command denylist",
            "Regular expressions to match commands that the Galaxy Agent should always ask permission to execute.",
            list,
            view,
            ai_settings,
            app,
        )
    }

    fn render_command_allowlist(
        command_allowlist: Vec<AgentModeCommandExecutionPredicate>,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let disabled = !ai_settings.is_command_allowlist_editable(app);
        let list = render_input_list(
            None,
            command_allowlist
                .into_iter()
                .zip(view.command_allowlist_mouse_state_handles.clone())
                .rev()
                .map(move |(cmd, mouse_state_handle)| InputListItem {
                    item: cmd.to_string(),
                    mouse_state_handle,
                    on_remove_action: AISettingsPageAction::RemoveFromProfileCommandAllowlist(cmd),
                    is_disabled: disabled,
                    tooltip_mouse_state: None,
                }),
            Some(&view.command_allowlist_editor),
            appearance,
        );

        render_ai_list(
            "Command allowlist",
            "Regular expressions to match commands that can be automatically executed by the Galaxy Agent.",
            list,
            view,
            ai_settings,
            app,
        )
    }

    fn render_directory_allowlist(
        directory_allowlist: Vec<PathBuf>,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let disabled = !ai_settings.is_directory_allowlist_editable(app);
        let list = render_input_list(
            None,
            directory_allowlist
                .clone()
                .into_iter()
                .zip(view.directory_allowlist_mouse_state_handles.clone())
                .rev()
                .map(move |(path, mouse_state_handle)| InputListItem {
                    item: path.display().to_string(),
                    mouse_state_handle,
                    on_remove_action: AISettingsPageAction::RemoveFromProfileDirectoryAllowlist(
                        path,
                    ),
                    is_disabled: disabled,
                    tooltip_mouse_state: None,
                }),
            Some(&view.directory_allowlist_editor),
            appearance,
        );

        render_ai_list(
            "Directory allowlist",
            "Give the agent file access to certain directories.",
            list,
            view,
            ai_settings,
            app,
        )
    }

    fn render_base_model_setting(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let show_in_prompt_checkbox = {
            let is_checked = *SessionSettings::as_ref(app).show_model_selectors_in_prompt;

            let mut checkbox = appearance
                .ui_builder()
                .checkbox(self.show_in_prompt_checkbox.clone(), None)
                .check(is_checked);

            if !ai_settings.is_any_ai_enabled(app) {
                checkbox = checkbox.disabled();
            }

            Container::new(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_children([
                        checkbox
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(
                                    AISettingsPageAction::ToggleShowBaseModelPickerInPrompt,
                                );
                            })
                            .finish(),
                        appearance
                            .ui_builder()
                            .span("Show model picker in prompt".to_string())
                            .with_style(UiComponentStyles {
                                font_color: Some(
                                    theme.sub_text_color(theme.surface_2()).into_solid(),
                                ),
                                font_size: Some(12.0),
                                ..Default::default()
                            })
                            .build()
                            .finish(),
                    ])
                    .finish(),
            )
            .with_margin_top(-6.0)
            .with_margin_left(-4.0)
            .finish()
        };

        render_dropdown_item(
            appearance,
            "Base model",
            Some(
                "This model serves as the primary engine behind the Galaxy Agent. It powers most interactions and invokes other models for tasks like planning or code generation when necessary. Galaxy may automatically switch to alternate models based on model availability or for auxiliary tasks such as conversation summarization.",
            ),
            Some(show_in_prompt_checkbox),
            LocalOnlyIconState::Hidden,
            (!ai_settings.is_any_ai_enabled(app))
                .then(|| appearance.theme().disabled_ui_text_color()),
            &view.base_model_dropdown,
        )
    }

    fn render_codebase_context_outline_generation_setting(
        codebase_context_toggle: SwitchStateHandle,
        codebase_context_link_index: HighlightedHyperlink,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &galaxyui::AppContext,
    ) -> Box<dyn Element> {
        let code_settings = CodeSettings::as_ref(app);
        let toggle = render_ai_setting_toggle::<CodebaseContextEnabled>(
            "Codebase Context",
            AISettingsPageAction::ToggleCodebaseContext,
            *code_settings.codebase_context_enabled,
            ai_settings.is_any_ai_enabled(app),
            codebase_context_toggle,
            &view.local_only_icon_tooltip_states,
            app,
        );

        let codebase_context_description = vec![
            FormattedTextFragment::plain_text(
                "Allow the Galaxy Agent to generate an outline of your codebase that can be used for context. No code is ever stored on our servers. ",
            ),
            FormattedTextFragment::hyperlink(
                "Learn more",
                "https://docs.warp.dev/agent-platform/capabilities/codebase-context",
            ),
        ];
        let description = Container::new(
            FormattedTextElement::new(
                FormattedText::new([FormattedTextLine::Line(codebase_context_description)]),
                CONTENT_FONT_SIZE,
                appearance.ui_font_family(),
                appearance.ui_font_family(),
                styles::description_font_color(ai_settings.is_any_ai_enabled(app), app).into(),
                codebase_context_link_index,
            )
            .with_hyperlink_font_color(appearance.theme().accent().into_solid())
            .register_default_click_handlers(|url, ctx, _| {
                ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
            })
            .finish(),
        )
        .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
        .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
        .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
        .finish();

        Flex::column()
            .with_child(toggle)
            .with_child(description)
            .finish()
    }

    fn render_mcp_permissions(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let all_runnable_mcp_servers =
            TemplatableMCPServerManager::get_all_cloud_synced_mcp_servers(app);
        if all_runnable_mcp_servers.is_empty() {
            self.render_mcp_permissions_zero_state(ai_settings, appearance, app)
        } else {
            self.render_mcp_permissions_with_servers(view, ai_settings, appearance, app)
        }
    }

    fn render_mcp_permissions_zero_state(
        &self,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let header = Container::new(render_body_item_label_with_icon::<AISettingsPageAction>(
            "Call MCP servers".into(),
            Icon::Dataflow,
            Some(styles::header_font_color(
                ai_settings.is_any_ai_enabled(app),
                app,
            )),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        ))
        .with_margin_bottom(4.)
        .finish();

        let subtext = {
            let subtext_fragments = vec![
                FormattedTextFragment::plain_text(
                    "You haven't added any MCP servers yet. Once you do, you'll be able to control how much autonomy the Galaxy Agent has when interacting with them. ",
                ),
                FormattedTextFragment::hyperlink_action(
                    "Add a server",
                    AISettingsPageAction::OpenMCPServerCollection,
                ),
                FormattedTextFragment::plain_text(" or "),
                FormattedTextFragment::hyperlink(
                    "learn more about MCPs.",
                    "https://docs.warp.dev/agent-platform/capabilities/mcp",
                ),
            ];

            Container::new(
                FormattedTextElement::new(
                    FormattedText::new([FormattedTextLine::Line(subtext_fragments)]),
                    CONTENT_FONT_SIZE,
                    appearance.ui_font_family(),
                    appearance.ui_font_family(),
                    styles::description_font_color(ai_settings.is_any_ai_enabled(app), app).into(),
                    HighlightedHyperlink::default(),
                )
                .with_hyperlink_font_color(appearance.theme().accent().into_solid())
                .register_default_click_handlers_with_action_support(|hyperlink_lens, ctx, _app| {
                    match hyperlink_lens {
                        HyperlinkLens::Url(url) => {
                            ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(
                                HyperlinkUrl {
                                    url: url.to_owned(),
                                },
                            ));
                        }
                        HyperlinkLens::Action(action_ref) => {
                            if let Some(action) =
                                action_ref.as_any().downcast_ref::<AISettingsPageAction>()
                            {
                                ctx.dispatch_typed_action(action.clone());
                            }
                        }
                    }
                })
                .finish(),
            )
            .with_margin_bottom(4.0)
            .finish()
        };

        Flex::column()
            .with_child(header)
            .with_child(subtext)
            .finish()
    }

    fn render_mcp_permissions_with_servers(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column();

        let current_mcp_setting =
            BlocklistAIPermissions::as_ref(app).get_mcp_permissions_setting(app, None);

        let permission_setting = self.render_execution_profile_dropdown(
            "Call MCP servers",
            Icon::Dataflow,
            current_mcp_setting.description(),
            &view.mcp_permissions_dropdown_menu,
            ai_settings,
            appearance,
            app,
        );
        column.add_child(permission_setting);

        if current_mcp_setting == ActionPermission::AlwaysAsk
            || current_mcp_setting == ActionPermission::AgentDecides
        {
            let allowlist = self.render_mcp_list(
                "MCP allowlist",
                "Allow the Galaxy Agent to call these MCP servers.",
                &view.mcp_allowlist_dropdown,
                BlocklistAIPermissions::as_ref(app).get_mcp_allowlist(app, None),
                view.mcp_allowlist_mouse_state_handles.clone(),
                AISettingsPageAction::RemoveFromMCPAllowlist,
                ai_settings,
                appearance,
                app,
            );
            column.add_child(allowlist);
        }

        if current_mcp_setting == ActionPermission::AlwaysAllow
            || current_mcp_setting == ActionPermission::AgentDecides
        {
            let denylist = self.render_mcp_list(
                "MCP denylist",
                "The Galaxy Agent will always ask for permission before calling any MCP servers on this list.",
                &view.mcp_denylist_dropdown,
                BlocklistAIPermissions::as_ref(app).get_mcp_denylist(app, None),
                view.mcp_denylist_mouse_state_handles.clone(),
                AISettingsPageAction::RemoveFromMCPDenylist,
                ai_settings,
                appearance,
                app,
            );
            column.add_child(denylist);
        }

        column.finish()
    }

    // Helper function to render the allow and denylists for mcp servers
    #[allow(clippy::too_many_arguments)]
    fn render_mcp_list(
        &self,
        title: &str,
        description: &str,
        dropdown: &ViewHandle<FilterableDropdown<AISettingsPageAction>>,
        items: Vec<uuid::Uuid>,
        mouse_state_handles: Vec<MouseStateHandle>,
        action: impl Fn(uuid::Uuid) -> AISettingsPageAction,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let selector = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_children(vec![
                    Shrinkable::new(
                        1.0,
                        Container::new(render_dropdown_item_label(
                            title.to_string(),
                            Some(description.to_string()),
                            LocalOnlyIconState::Hidden,
                            (!ai_settings.is_any_ai_enabled(app))
                                .then(|| appearance.theme().disabled_ui_text_color()),
                            appearance,
                        ))
                        .with_margin_right(4.)
                        .finish(),
                    )
                    .finish(),
                    ChildView::new(dropdown).finish(),
                ])
                .finish(),
        )
        .with_margin_bottom(2.)
        .finish();

        let disabled = !ai_settings.is_any_ai_enabled(app);
        let items = render_input_list(
            None,
            items
                .into_iter()
                .rev()
                .zip(mouse_state_handles.clone())
                .filter_map(move |(uuid, mouse_state_handle)| {
                    let server_name = TemplatableMCPServerManager::get_mcp_name(&uuid, app);
                    server_name.map(|server_name| InputListItem {
                        item: server_name,
                        mouse_state_handle,
                        on_remove_action: action(uuid),
                        is_disabled: disabled,
                        tooltip_mouse_state: None,
                    })
                }),
            None,
            appearance,
        );

        Container::new(Flex::column().with_children(vec![selector, items]).finish())
            .with_margin_bottom(8.)
            .finish()
    }
}

#[derive(Default)]
struct AIInputWidget {
    incorrect_autodetection_highlight_index: HighlightedHyperlink,
    autodetection_toggle: SwitchStateHandle,
    nld_in_terminal_toggle: SwitchStateHandle,
    show_input_hint_toggle: SwitchStateHandle,
    show_agent_tips_toggle: SwitchStateHandle,
    include_agent_commands_in_history_toggle: SwitchStateHandle,
}

impl SettingsWidget for AIInputWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "oz agent ai input natural language detection autodetection prompt terminal command commands history shell executed execution queue interrupt submission submit auto-queue response while responding default long-running long running lrc"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let input_header = build_sub_header(
            appearance,
            "Input",
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        )
        .with_padding_bottom(HEADER_PADDING)
        .finish();

        let natural_language_detection_section = Self::render_natural_language_detection_section(
            self.incorrect_autodetection_highlight_index.clone(),
            self.autodetection_toggle.clone(),
            self.nld_in_terminal_toggle.clone(),
            view,
            ai_settings,
            appearance,
            app,
        );

        let show_input_hint_text = render_ai_setting_toggle::<ShowHintText>(
            "Show input hint text",
            AISettingsPageAction::ToggleShowInputHintText,
            *InputSettings::as_ref(app).show_hint_text,
            is_any_ai_enabled,
            self.show_input_hint_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let mut widget_children = vec![
            render_separator(appearance),
            input_header,
            natural_language_detection_section,
            show_input_hint_text,
        ];

        if FeatureFlag::AgentTips.is_enabled() {
            let agent_tips_toggle = render_ai_setting_toggle::<ShowAgentTips>(
                "Show agent tips",
                AISettingsPageAction::ToggleShowAgentTips,
                *InputSettings::as_ref(app).show_agent_tips,
                is_any_ai_enabled,
                self.show_agent_tips_toggle.clone(),
                &view.local_only_icon_tooltip_states,
                app,
            );
            widget_children.push(agent_tips_toggle);
        }

        widget_children.push(render_ai_setting_toggle::<IncludeAgentCommandsInHistory>(
            "Include agent-executed commands in history",
            AISettingsPageAction::ToggleIncludeAgentCommandsInHistory,
            *ai_settings.include_agent_commands_in_history,
            is_any_ai_enabled,
            self.include_agent_commands_in_history_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        ));

        if FeatureFlag::QueueSlashCommand.is_enabled() {
            widget_children.push(render_dropdown_item(
                appearance,
                "Default prompt submission mode",
                Some(
                    "What happens when you submit a new prompt while the agent is still \
                     responding. You can override this per conversation using the auto-queue \
                     toggle.",
                ),
                None,
                LocalOnlyIconState::for_setting(
                    PromptSubmissionMode::storage_key(),
                    PromptSubmissionMode::sync_to_cloud(),
                    &mut view.local_only_icon_tooltip_states.borrow_mut(),
                    app,
                ),
                (!is_any_ai_enabled).then(|| appearance.theme().disabled_ui_text_color()),
                &view.default_prompt_submission_mode_dropdown,
            ));

            // Only meaningful in Interrupt mode: with Queue selected, prompts already
            // queue until the end of the full response, so the LRC mode is hidden.
            if ai_settings.default_prompt_submission_mode == PromptSubmissionMode::Interrupt {
                widget_children.push(
                    Container::new(render_dropdown_item(
                        appearance,
                        "Default long-running command submission mode",
                        Some(
                            "What happens when you submit a prompt while an agent is driving an \
                             agent-requested long-running command. Queued prompts are sent to the \
                             agent when the command finishes.",
                        ),
                        None,
                        LocalOnlyIconState::for_setting(
                            LongRunningCommandSubmissionMode::storage_key(),
                            LongRunningCommandSubmissionMode::sync_to_cloud(),
                            &mut view.local_only_icon_tooltip_states.borrow_mut(),
                            app,
                        ),
                        (!is_any_ai_enabled).then(|| appearance.theme().disabled_ui_text_color()),
                        &view.lrc_submission_mode_dropdown,
                    ))
                    .with_margin_top(styles::DESCRIPTION_MARGIN_BOTTOM)
                    .finish(),
                );
            }
        }

        Flex::column().with_children(widget_children).finish()
    }
}

impl AIInputWidget {
    fn render_natural_language_detection_section(
        incorrect_autodetection_highlight_index: HighlightedHyperlink,
        autodetection_toggle: SwitchStateHandle,
        nld_in_terminal_toggle: SwitchStateHandle,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let is_toggleable = ai_settings.is_any_ai_enabled(app);
        let is_nld_enabled = *ai_settings.ai_autodetection_enabled_internal.value();

        let autodetection_denylist_input_field = appearance
            .ui_builder()
            .text_input(view.autodetection_denylist_editor.clone())
            .with_style(UiComponentStyles {
                width: Some(280.),
                padding: Some(Coords {
                    top: 4.,
                    bottom: 4.,
                    left: 6.,
                    right: 6.,
                }),
                background: Some(appearance.theme().surface_2().into()),
                ..Default::default()
            })
            .build()
            .finish();

        let mut section = Flex::column();

        if FeatureFlag::AgentView.is_enabled() {
            static AUTODETECTION_DESCRIPTION_FRAGMENTS: LazyLock<Vec<FormattedTextFragment>> =
                LazyLock::new(|| {
                    vec![
                        FormattedTextFragment::plain_text("Encountered an incorrect detection? "),
                        FormattedTextFragment::hyperlink(
                            "Let us know",
                            "https://warpdotdev.typeform.com/to/offrTIpq",
                        ),
                    ]
                });

            section.add_children([
                render_ai_setting_toggle::<NLDInTerminalEnabled>(
                    "Autodetect agent prompts in terminal input",
                    AISettingsPageAction::ToggleNLDInTerminal,
                    ai_settings.is_nld_in_terminal_enabled(app),
                    is_toggleable,
                    nld_in_terminal_toggle,
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
                render_ai_setting_toggle::<AIAutoDetectionEnabled>(
                    "Autodetect terminal commands in agent input",
                    AISettingsPageAction::ToggleAIInputAutoDetection,
                    is_nld_enabled,
                    is_toggleable,
                    autodetection_toggle,
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
                Container::new(
                    FormattedTextElement::new(
                        FormattedText::new([FormattedTextLine::Line(
                            (*AUTODETECTION_DESCRIPTION_FRAGMENTS).clone(),
                        )]),
                        CONTENT_FONT_SIZE,
                        appearance.ui_font_family(),
                        appearance.ui_font_family(),
                        styles::description_font_color(is_toggleable, app).into(),
                        incorrect_autodetection_highlight_index,
                    )
                    .with_hyperlink_font_color(appearance.theme().accent().into_solid())
                    .register_default_click_handlers(|url, ctx, _| {
                        ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
                    })
                    .finish(),
                )
                .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                .finish(),
            ])
        } else {
            static NATURAL_LANGUAGE_DETECTION_DESCRIPTION_FRAGMENTS: LazyLock<
                Vec<FormattedTextFragment>,
            > = LazyLock::new(|| {
                vec![
                    FormattedTextFragment::plain_text(
                        "Enabling natural language detection will detect when natural language is written in the terminal input, and then automatically switch to Agent Mode for AI queries.",
                    ),
                    FormattedTextFragment::plain_text(
                        " Encountered an incorrect input detection? ",
                    ),
                    FormattedTextFragment::hyperlink(
                        "Let us know",
                        "https://warpdotdev.typeform.com/to/offrTIpq",
                    ),
                ]
            });

            section.add_children([
                render_ai_setting_toggle::<AIAutoDetectionEnabled>(
                    "Natural language detection",
                    AISettingsPageAction::ToggleAIInputAutoDetection,
                    is_nld_enabled,
                    is_toggleable,
                    autodetection_toggle,
                    &view.local_only_icon_tooltip_states,
                    app,
                ),
                Container::new(
                    FormattedTextElement::new(
                        FormattedText::new([FormattedTextLine::Line(
                            (*NATURAL_LANGUAGE_DETECTION_DESCRIPTION_FRAGMENTS).clone(),
                        )]),
                        CONTENT_FONT_SIZE,
                        appearance.ui_font_family(),
                        appearance.ui_font_family(),
                        styles::description_font_color(is_toggleable, app).into(),
                        incorrect_autodetection_highlight_index,
                    )
                    .with_hyperlink_font_color(appearance.theme().accent().into_solid())
                    .register_default_click_handlers(|url, ctx, _| {
                        ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
                    })
                    .finish(),
                )
                .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                .finish(),
            ]);
        }

        section
            .with_child(render_ai_setting_label::<AICommandDenylist>(
                "Natural language denylist".to_owned(),
                is_toggleable,
                &view.local_only_icon_tooltip_states,
                app,
            ))
            .with_child(render_ai_setting_description(
                "Commands listed here will never trigger natural language detection.",
                is_toggleable,
                app,
            ))
            .with_child(
                Container::new(autodetection_denylist_input_field)
                    .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                    .finish(),
            )
            .finish()
    }
}

#[derive(Default)]
struct MCPServersWidget {
    manage_mcp_servers_button: MouseStateHandle,
    mcp_docs_link_index: HighlightedHyperlink,
    file_based_mcp_toggle: SwitchStateHandle,
    file_based_mcp_docs_link_index: HighlightedHyperlink,
}

impl MCPServersWidget {
    fn should_show_mcp() -> bool {
        FeatureFlag::McpServer.is_enabled() && ContextFlag::ShowMCPServers.is_enabled()
    }
}

impl SettingsWidget for MCPServersWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "oz agent mcp server servers model context protocol file-based file based project claude .mcp.json .claude/.mcp.json .codex config.toml .codex/config.toml"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        MCPServersWidget::should_show_mcp()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_any_ai_enabled = AISettings::as_ref(app).is_any_ai_enabled(app);
        let ai_settings = AISettings::as_ref(app);

        let header = build_sub_header(
            appearance,
            "MCP Servers",
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        )
        .with_padding_bottom(HEADER_PADDING)
        .finish();

        let mcp_description = vec![
            FormattedTextFragment::plain_text(
                "Add MCP servers to extend the Galaxy Agent's capabilities. \
            MCP servers expose data sources or tools to agents through a standardized interface, essentially acting like plugins. ",
            ),
            FormattedTextFragment::hyperlink(
                "Learn more",
                "https://docs.warp.dev/agent-platform/capabilities/mcp",
            ),
        ];

        let description = Container::new(
            FormattedTextElement::new(
                FormattedText::new([FormattedTextLine::Line(mcp_description)]),
                CONTENT_FONT_SIZE,
                appearance.ui_font_family(),
                appearance.ui_font_family(),
                styles::description_font_color(is_any_ai_enabled, app).into(),
                self.mcp_docs_link_index.clone(),
            )
            .with_hyperlink_font_color(appearance.theme().accent().into_solid())
            .register_default_click_handlers(|url, ctx, _| {
                ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
            })
            .finish(),
        )
        .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
        .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
        .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
        .finish();

        let file_based_mcp_toggle = if FeatureFlag::FileBasedMcp.is_enabled() {
            Some(
                Flex::column()
                    .with_child(render_ai_setting_toggle::<FileBasedMcpEnabled>(
                        "Auto-spawn servers from third-party agents",
                        AISettingsPageAction::ToggleFileBasedMcp,
                        *ai_settings.file_based_mcp_enabled,
                        is_any_ai_enabled,
                        self.file_based_mcp_toggle.clone(),
                        &view.local_only_icon_tooltip_states,
                        app,
                    ))
                    .with_child({
                        static FILE_BASED_MCP_DESCRIPTION_FRAGMENTS: LazyLock<
                            Vec<FormattedTextFragment>,
                        > = LazyLock::new(|| {
                            vec![
                                FormattedTextFragment::plain_text(
                                    "Automatically detect and spawn MCP servers from globally-scoped third-party AI agent configuration files (e.g. in your home directory). Servers detected inside a repository are never spawned automatically and must be enabled individually from the MCP settings page. ",
                                ),
                                FormattedTextFragment::hyperlink(
                                    "See supported providers.",
                                    "https://docs.warp.dev/agent-platform/capabilities/mcp#file-based-mcp-servers",
                                ),
                            ]
                        });
                        Container::new(
                            FormattedTextElement::new(
                                FormattedText::new([FormattedTextLine::Line(
                                    (*FILE_BASED_MCP_DESCRIPTION_FRAGMENTS).clone(),
                                )]),
                                CONTENT_FONT_SIZE,
                                appearance.ui_font_family(),
                                appearance.ui_font_family(),
                                styles::description_font_color(is_any_ai_enabled, app).into(),
                                self.file_based_mcp_docs_link_index.clone(),
                            )
                            .with_hyperlink_font_color(appearance.theme().accent().into_solid())
                            .register_default_click_handlers(|url, ctx, _| {
                                ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
                            })
                            .finish(),
                        )
                        .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                        .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                        .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                        .finish()
                    })
                    .finish(),
            )
        } else {
            None
        };

        let button = render_full_pane_width_ai_button(
            "Manage MCP servers",
            is_any_ai_enabled,
            self.manage_mcp_servers_button.clone(),
            AISettingsPageAction::OpenMCPServerCollection,
            appearance,
        );

        let mut column = Flex::column()
            .with_child(header)
            .with_child(description)
            .with_child(button);

        if let Some(toggle) = file_based_mcp_toggle {
            column = column.with_child(toggle);
        }
        column.finish()
    }
}

#[derive(Default)]
struct AIFactWidget {
    rules_toggle: SwitchStateHandle,
    rules_link_index: HighlightedHyperlink,
    manage_rules_button: MouseStateHandle,
    rule_suggestions_toggle: SwitchStateHandle,
    warp_drive_context_toggle: SwitchStateHandle,
}

impl AIFactWidget {
    fn render_rules_toggle(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        appearance: &Appearance,
        app: &galaxyui::AppContext,
    ) -> Box<dyn Element> {
        let toggle = render_ai_setting_toggle::<MemoryEnabled>(
            "Rules",
            AISettingsPageAction::ToggleRules,
            *ai_settings.memory_enabled,
            ai_settings.is_any_ai_enabled(app),
            self.rules_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let rules_description = vec![
            FormattedTextFragment::plain_text(
                "Rules help the Galaxy Agent follow your conventions, whether for codebases or specific workflows. "
            ),
            FormattedTextFragment::hyperlink(
                "Learn more",
                "https://docs.warp.dev/agent-platform/capabilities/rules",
            ),
        ];
        let description = Container::new(
            FormattedTextElement::new(
                FormattedText::new([FormattedTextLine::Line(rules_description)]),
                CONTENT_FONT_SIZE,
                appearance.ui_font_family(),
                appearance.ui_font_family(),
                styles::description_font_color(ai_settings.is_any_ai_enabled(app), app).into(),
                self.rules_link_index.clone(),
            )
            .with_hyperlink_font_color(appearance.theme().accent().into_solid())
            .register_default_click_handlers(|url, ctx, _| {
                ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
            })
            .finish(),
        )
        .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
        .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
        .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
        .finish();

        Flex::column()
            .with_child(toggle)
            .with_child(description)
            .finish()
    }

    fn render_rule_suggestions_toggle(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        app: &galaxyui::AppContext,
    ) -> Box<dyn Element> {
        let toggle = render_ai_setting_toggle::<RuleSuggestionsEnabled>(
            "Suggested Rules",
            AISettingsPageAction::ToggleRuleSuggestions,
            *ai_settings.rule_suggestions_enabled_internal,
            ai_settings.is_any_ai_enabled(app),
            self.rule_suggestions_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let description = render_ai_setting_description(
            "Let AI suggest rules to save based on your interactions.",
            ai_settings.is_any_ai_enabled(app),
            app,
        );

        Flex::column()
            .with_child(toggle)
            .with_child(description)
            .finish()
    }

    fn render_warp_drive_context_toggle(
        &self,
        view: &AISettingsPageView,
        ai_settings: &AISettings,
        app: &galaxyui::AppContext,
    ) -> Box<dyn Element> {
        let toggle = render_ai_setting_toggle::<WarpDriveContextEnabled>(
            "Galaxy Drive as agent context",
            AISettingsPageAction::ToggleGalaxyDriveContext,
            *ai_settings.warp_drive_context_enabled,
            ai_settings.is_any_ai_enabled(app),
            self.warp_drive_context_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let description = render_ai_setting_description(
            "The Galaxy Agent can leverage your Galaxy Drive Contents to tailor responses to your personal and team developer workflows and environments. This includes any Workflows, Notebooks, and Environment Variables.",
            ai_settings.is_any_ai_enabled(app),
            app,
        );

        Flex::column()
            .with_child(toggle)
            .with_child(description)
            .finish()
    }
}

impl SettingsWidget for AIFactWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "agent oz ai a.i. knowledge fact memory memories rules warp drive context workflows notebooks environment variables"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        FeatureFlag::AIRules.is_enabled()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let header = build_sub_header(
            appearance,
            "Knowledge",
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        )
        .with_margin_bottom(HEADER_PADDING)
        .finish();

        let button = render_full_pane_width_ai_button(
            "Manage rules",
            is_any_ai_enabled,
            self.manage_rules_button.clone(),
            AISettingsPageAction::OpenAIFactCollection,
            appearance,
        );

        let mut column = Flex::column()
            .with_child(header)
            .with_child(self.render_rules_toggle(view, ai_settings, appearance, app));

        if FeatureFlag::SuggestedRules.is_enabled() {
            column.add_child(self.render_rule_suggestions_toggle(view, ai_settings, app));
        }

        column
            .with_child(button)
            .with_child(self.render_warp_drive_context_toggle(view, ai_settings, app))
            .finish()
    }
}

#[derive(Default)]
struct VoiceWidget {
    voice_input_toggle: SwitchStateHandle,
    wispr_highlight_index: HighlightedHyperlink,
}

impl VoiceWidget {
    fn render_voice_section(
        &self,
        view: &AISettingsPageView,
        appearance: &Appearance,
        app: &galaxyui::AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_toggleable = ai_settings.is_any_ai_enabled(app);
        let mut column = Flex::column().with_child(render_ai_setting_toggle::<VoiceInputEnabled>(
            "Voice Input",
            AISettingsPageAction::ToggleVoiceInput,
            *ai_settings.voice_input_enabled_internal,
            is_toggleable,
            self.voice_input_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        ));

        let voice_input_description_text_fragments = vec![
            FormattedTextFragment::plain_text(
                "Voice input allows you to control Warp by speaking directly to your terminal (powered by ",
            ),
            FormattedTextFragment::hyperlink("Wispr Flow", WISPR_FLOW_URL),
            FormattedTextFragment::plain_text(")."),
        ];

        let voice_input_description = FormattedTextElement::new(
            FormattedText::new([FormattedTextLine::Line(
                voice_input_description_text_fragments,
            )]),
            appearance.ui_font_size(),
            appearance.ui_font_family(),
            appearance.ui_font_family(),
            styles::description_font_color(is_toggleable, app).into(),
            self.wispr_highlight_index.clone(),
        )
        .with_hyperlink_font_color(appearance.theme().accent().into_solid())
        .register_default_click_handlers(|url, ctx, _| {
            ctx.dispatch_typed_action(AISettingsPageAction::HyperlinkClick(url));
        });

        column.add_child(
            Container::new(voice_input_description.finish())
                .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                .finish(),
        );

        if ai_settings.is_voice_input_enabled(app) {
            column.add_child(render_dropdown_item(
                appearance,
                "Key for Activating Voice Input",
                Some("Press and hold to activate."),
                None,
                LocalOnlyIconState::for_setting(
                    VoiceInputToggleKey::storage_key(),
                    VoiceInputToggleKey::sync_to_cloud(),
                    &mut view.local_only_icon_tooltip_states.borrow_mut(),
                    app,
                ),
                None,
                &view.voice_input_toggle_key_dropdown,
            ));
        }

        column.finish()
    }
}

impl SettingsWidget for VoiceWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "voice agent oz ai a.i. speech input natural language talk english"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        cfg!(feature = "voice_input") && UserWorkspaces::as_ref(app).is_voice_enabled()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);
        Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                build_sub_header(
                    appearance,
                    "Voice",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            )
            .with_child(self.render_voice_section(view, appearance, app))
            .finish()
    }
}
#[derive(Default)]
struct OtherAIWidget {
    use_agent_footer_toggle: SwitchStateHandle,
    show_conversation_history_toggle: SwitchStateHandle,
}

impl OtherAIWidget {
    fn create_thinking_display_mode_dropdown(
        ctx: &mut ViewContext<AISettingsPageView>,
    ) -> ViewHandle<Dropdown<AISettingsPageAction>> {
        let items: Vec<DropdownItem<AISettingsPageAction>> = ThinkingDisplayMode::iter()
            .map(|mode| {
                DropdownItem::new(
                    mode.display_name(),
                    AISettingsPageAction::SetThinkingDisplayMode(mode),
                )
            })
            .collect();

        ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);
            dropdown.add_items(items, ctx);
            dropdown
        })
    }

    fn create_default_prompt_submission_mode_dropdown(
        ctx: &mut ViewContext<AISettingsPageView>,
    ) -> ViewHandle<Dropdown<AISettingsPageAction>> {
        let items: Vec<DropdownItem<AISettingsPageAction>> = PromptSubmissionMode::iter()
            .map(|mode| {
                DropdownItem::new(
                    mode.display_name(),
                    AISettingsPageAction::SetPromptSubmissionMode(mode),
                )
            })
            .collect();

        ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);
            dropdown.add_items(items, ctx);
            dropdown
        })
    }

    fn create_lrc_submission_mode_dropdown(
        ctx: &mut ViewContext<AISettingsPageView>,
    ) -> ViewHandle<Dropdown<AISettingsPageAction>> {
        let items: Vec<DropdownItem<AISettingsPageAction>> =
            LongRunningCommandSubmissionMode::iter()
                .map(|mode| {
                    DropdownItem::new(
                        mode.display_name(),
                        AISettingsPageAction::SetLongRunningCommandSubmissionMode(mode),
                    )
                })
                .collect();

        ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);
            dropdown.add_items(items, ctx);
            dropdown
        })
    }

    fn create_orchestration_message_display_mode_dropdown(
        ctx: &mut ViewContext<AISettingsPageView>,
    ) -> ViewHandle<Dropdown<AISettingsPageAction>> {
        let items: Vec<DropdownItem<AISettingsPageAction>> =
            OrchestrationMessageDisplayMode::iter()
                .map(|mode| {
                    DropdownItem::new(
                        mode.display_name(),
                        AISettingsPageAction::SetOrchestrationMessageDisplayMode(mode),
                    )
                })
                .collect();

        ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(AI_SETTINGS_DROPDOWN_WIDTH);
            dropdown.set_menu_width(AI_SETTINGS_DROPDOWN_WIDTH, ctx);
            dropdown.set_menu_max_height(AI_SETTINGS_DROPDOWN_MAX_HEIGHT, ctx);
            dropdown.add_items(items, ctx);
            dropdown
        })
    }
}

impl SettingsWidget for OtherAIWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "other oz updates zero state empty changelog new conversation agent what's new use agent footer toolbar layout chip chips rearrange re-arrange thinking expanded reasoning collapse never show orchestration messages child agents collapse expand hide conversation history"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);
        let is_toggleable = is_any_ai_enabled;

        let mut column = Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                build_sub_header(
                    appearance,
                    "Other",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            );

        if FeatureFlag::AgentView.is_enabled() {
            let mut agent_view_column = Flex::column()
                .with_child(render_ai_setting_toggle::<ShouldRenderUseAgentToolbarForUserCommands>(
                    "Show \"Use Agent\" footer",
                    AISettingsPageAction::ToggleUseAgentToolbar,
                    *ai_settings.should_render_use_agent_footer_for_user_commands,
                    is_toggleable,
                    self.use_agent_footer_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ))
                .with_child(render_ai_setting_description(
                    "Shows hint to use the \"Full Terminal Use\"-enabled agent in long running commands.",
                    is_toggleable,
                    app,
                ));

            if is_toggleable && FeatureFlag::AgentToolbarEditor.is_enabled() {
                agent_view_column.add_child(render_toolbar_layout_editor(
                    &view.agent_toolbar_inline_editor,
                    appearance,
                ));
            }

            column.add_child(agent_view_column.finish());
        }

        column.add_child(render_ai_setting_toggle::<ShowConversationHistory>(
            "Show conversation history in tools panel",
            AISettingsPageAction::ToggleShowConversationHistory,
            *ai_settings.show_conversation_history,
            is_toggleable,
            self.show_conversation_history_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        ));

        column.add_child(render_dropdown_item(
            appearance,
            "Agent thinking display",
            Some("Controls how reasoning/thinking traces are displayed."),
            None,
            LocalOnlyIconState::for_setting(
                ThinkingDisplayMode::storage_key(),
                ThinkingDisplayMode::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            (!is_any_ai_enabled).then(|| appearance.theme().disabled_ui_text_color()),
            &view.thinking_display_mode_dropdown,
        ));

        column.add_child(render_dropdown_item(
            appearance,
            "Orchestration message display",
            Some("Controls whether orchestration messages stay expanded."),
            None,
            LocalOnlyIconState::for_setting(
                OrchestrationMessageDisplayMode::storage_key(),
                OrchestrationMessageDisplayMode::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            (!is_any_ai_enabled).then(|| appearance.theme().disabled_ui_text_color()),
            &view.orchestration_message_display_mode_dropdown,
        ));

        // TODO: OpenConversationLayoutPreference should not depend on local_fs, but it lives under the external editor settings
        // which does require local_fs. It was a mistake to put it there, but now we keep it there for backward compatibility.
        #[cfg(feature = "local_fs")]
        if FeatureFlag::OpenWarpNewSettingsModes.is_enabled() {
            use crate::util::file::external_editor::settings::OpenConversationLayoutPreference;

            column.add_child(render_dropdown_item(
                appearance,
                "Preferred layout when opening existing agent conversations",
                None,
                None,
                LocalOnlyIconState::for_setting(
                    OpenConversationLayoutPreference::storage_key(),
                    OpenConversationLayoutPreference::sync_to_cloud(),
                    &mut view.local_only_icon_tooltip_states.borrow_mut(),
                    app,
                ),
                (!is_any_ai_enabled).then(|| appearance.theme().disabled_ui_text_color()),
                &view.conversation_layout_dropdown,
            ));
        }

        column.finish()
    }
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn cli_agent_settings_widget_id() -> &'static str {
    CLIAgentWidget::static_widget_id()
}

#[derive(Default)]
struct CLIAgentWidget {
    cli_agent_footer_toggle: SwitchStateHandle,
    auto_toggle_rich_input_toggle: SwitchStateHandle,
    auto_toggle_rich_input_info_tooltip: MouseStateHandle,
    auto_open_rich_input_on_cli_agent_start_toggle: SwitchStateHandle,
    auto_dismiss_rich_input_toggle: SwitchStateHandle,
    submit_on_ctrl_enter_toggle: SwitchStateHandle,
}

impl SettingsWidget for CLIAgentWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent claude codex gemini toolbar footer layout chip chips rearrange re-arrange bar command regex auto show rich input dismiss ctrl enter submit newline"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);

        // The Coding Agents section is always enabled, independent of the
        // global AI toggle, because these settings control third-party coding
        // agents (Claude Code, Codex, Gemini CLI) rather than Warp's own AI.
        let cli_agent_footer_toggle = render_ai_setting_toggle::<ShouldRenderCLIAgentToolbar>(
            "Show coding agent toolbar",
            AISettingsPageAction::ToggleCLIAgentToolbar,
            *ai_settings.should_render_cli_agent_footer,
            true,
            self.cli_agent_footer_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let description_fragments = vec![
            FormattedTextFragment::plain_text(
                "Show a toolbar with quick actions when running coding agents like ",
            ),
            FormattedTextFragment::inline_code("claude"),
            FormattedTextFragment::plain_text(", "),
            FormattedTextFragment::inline_code("codex"),
            FormattedTextFragment::plain_text(", or "),
            FormattedTextFragment::inline_code("gemini"),
            FormattedTextFragment::plain_text("."),
        ];

        let description = FormattedTextElement::new(
            FormattedText::new([FormattedTextLine::Line(description_fragments)]),
            appearance.ui_font_size(),
            appearance.ui_font_family(),
            appearance.monospace_font_family(),
            styles::description_font_color(true, app).into(),
            HighlightedHyperlink::default(),
        );

        let is_footer_enabled = *ai_settings.should_render_cli_agent_footer;

        let mut column = Flex::column()
            .with_child(
                build_sub_header(
                    appearance,
                    "Third party CLI agents",
                    Some(styles::header_font_color(true, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            )
            .with_child(cli_agent_footer_toggle)
            .with_child(
                Container::new(description.finish())
                    .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                    .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                    .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                    .finish(),
            );

        if is_footer_enabled {
            use super::settings_page::AdditionalInfo;
            use crate::settings::{
                AutoDismissRichInputAfterSubmit, AutoOpenRichInputOnCLIAgentStart,
                AutoToggleRichInput, SubmitRichInputOnCtrlEnter,
            };

            if FeatureFlag::CLIAgentRichInput.is_enabled() {
                // Setting 1: Auto show/hide rich input based on agent status
                let auto_show_toggle_label = render_body_item_label::<AISettingsPageAction>(
                    "Auto show/hide Rich Input based on agent status".into(),
                    Some(styles::header_font_color(true, app)),
                    Some(AdditionalInfo {
                        mouse_state: self.auto_toggle_rich_input_info_tooltip.clone(),
                        on_click_action: None,
                        secondary_text: None,
                        tooltip_override_text: Some(
                            "Requires the Warp plugin for your coding agent".to_owned(),
                        ),
                    }),
                    LocalOnlyIconState::for_setting(
                        AutoToggleRichInput::storage_key(),
                        AutoToggleRichInput::sync_to_cloud(),
                        &mut view.local_only_icon_tooltip_states.borrow_mut(),
                        app,
                    ),
                    ToggleState::Enabled,
                    appearance,
                );
                column.add_child(build_toggle_element(
                    auto_show_toggle_label,
                    render_ai_feature_switch(
                        self.auto_toggle_rich_input_toggle.clone(),
                        *ai_settings.auto_toggle_rich_input,
                        true,
                        AISettingsPageAction::ToggleAutoToggleRichInput,
                        app,
                    ),
                    appearance,
                    None,
                ));

                column.add_child(
                    render_ai_setting_toggle::<AutoOpenRichInputOnCLIAgentStart>(
                        "Auto open Rich Input when a coding agent session starts",
                        AISettingsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart,
                        *ai_settings.auto_open_rich_input_on_cli_agent_start,
                        true,
                        self.auto_open_rich_input_on_cli_agent_start_toggle.clone(),
                        &view.local_only_icon_tooltip_states,
                        app,
                    ),
                );

                // Setting 2: Auto dismiss rich input after prompt submission
                column.add_child(render_ai_setting_toggle::<AutoDismissRichInputAfterSubmit>(
                    "Auto dismiss Rich Input after prompt submission",
                    AISettingsPageAction::ToggleAutoDismissRichInputAfterSubmit,
                    *ai_settings.auto_dismiss_rich_input_after_submit,
                    true,
                    self.auto_dismiss_rich_input_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ));

                // Setting 3: Submit Rich Input with Ctrl+Enter
                column.add_child(render_ai_setting_toggle::<SubmitRichInputOnCtrlEnter>(
                    "Submit Rich Input with Ctrl+Enter",
                    AISettingsPageAction::ToggleSubmitRichInputOnCtrlEnter,
                    *ai_settings.submit_on_ctrl_enter,
                    true,
                    self.submit_on_ctrl_enter_toggle.clone(),
                    &view.local_only_icon_tooltip_states,
                    app,
                ));
            }

            let command_list = {
                let mut list_column = Flex::column();

                list_column.add_child(
                    appearance
                        .ui_builder()
                        .span("Commands that enable the toolbar".to_string())
                        .with_style(UiComponentStyles {
                            font_size: Some(CONTENT_FONT_SIZE),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                );

                list_column
                    .add_child(ChildView::new(&view.cli_agent_footer_command_editor).finish());

                let background = appearance.theme().surface_1();
                let font_color = appearance.theme().foreground();
                let items: Vec<_> = ai_settings
                    .cli_agent_footer_enabled_commands
                    .value()
                    .keys()
                    .cloned()
                    .collect();
                let len = items.len();
                for (rev_i, pattern) in items.iter().rev().enumerate() {
                    let original_i = len - 1 - rev_i;
                    let remove_action =
                        AISettingsPageAction::RemoveCLIAgentToolbarEnabledCommand(pattern.clone());
                    let mouse_state = view
                        .cli_agent_footer_command_mouse_state_handles
                        .get(original_i)
                        .cloned()
                        .unwrap_or_default();

                    let remove_button = appearance
                        .ui_builder()
                        .close_button(16., mouse_state)
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(remove_action.clone());
                        })
                        .finish();

                    let label = appearance
                        .ui_builder()
                        .wrappable_text(pattern.clone(), true)
                        .with_style(UiComponentStyles {
                            font_color: Some(font_color.into_solid()),
                            font_family_id: Some(appearance.monospace_font_family()),
                            font_size: Some(appearance.ui_font_size()),
                            ..Default::default()
                        })
                        .build()
                        .finish();

                    let mut right_side =
                        Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
                    if let Some(dropdown_handle) = view
                        .cli_agent_footer_command_agent_dropdowns
                        .get(original_i)
                    {
                        right_side.add_child(
                            Container::new(ChildView::new(dropdown_handle).finish())
                                .with_margin_right(8.)
                                .finish(),
                        );
                    }
                    right_side.add_child(remove_button);

                    let row = Container::new(
                        Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_main_axis_size(MainAxisSize::Max)
                            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                            .with_children([
                                Shrinkable::new(1., label).finish(),
                                right_side.finish(),
                            ])
                            .finish(),
                    )
                    .with_background(background)
                    .with_horizontal_padding(8.)
                    .with_vertical_padding(4.)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                    .with_margin_bottom(4.)
                    .finish();

                    list_column.add_child(row);
                }

                list_column.finish()
            };
            let command_list_description = appearance
                .ui_builder()
                .paragraph(
                    "Add regex patterns to show the coding agent toolbar for matching commands.",
                )
                .with_style(UiComponentStyles {
                    font_size: Some(appearance.ui_font_size()),
                    font_color: Some(styles::description_font_color(true, app).into()),
                    margin: Some(
                        Coords::default()
                            .top(4.)
                            .bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                            .right(styles::TOGGLE_WIDTH_MARGIN),
                    ),
                    ..Default::default()
                })
                .build()
                .finish();

            column.add_child(command_list);
            column.add_child(command_list_description);

            if FeatureFlag::AgentToolbarEditor.is_enabled() {
                column.add_child(render_toolbar_layout_editor(
                    &view.cli_agent_toolbar_inline_editor,
                    appearance,
                ));
            }
        }

        column.finish()
    }
}

/// The presentation state of the agent attribution toggle, derived from the
/// org-level [`AdminEnablementSetting`], the user's stored preference, and
/// whether AI is globally enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentAttributionToggleState {
    /// Whether the toggle is rendered in the checked state.
    pub(crate) is_enabled: bool,
    /// Whether the org has forced the value (locking the toggle with a tooltip).
    pub(crate) is_forced_by_org: bool,
    /// Whether the toggle should be rendered as non-interactive overall
    /// (forced by the org, or AI globally disabled).
    pub(crate) is_disabled: bool,
}

/// Derive the toggle state from its three inputs.
pub(crate) fn derive_agent_attribution_toggle_state(
    org_setting: &AdminEnablementSetting,
    user_pref: bool,
    is_any_ai_enabled: bool,
) -> AgentAttributionToggleState {
    let is_forced_by_org = match org_setting {
        AdminEnablementSetting::Enable | AdminEnablementSetting::Disable => true,
        AdminEnablementSetting::RespectUserSetting => false,
    };
    let is_enabled = match org_setting {
        AdminEnablementSetting::Enable => true,
        AdminEnablementSetting::Disable => false,
        AdminEnablementSetting::RespectUserSetting => user_pref,
    };
    AgentAttributionToggleState {
        is_enabled,
        is_forced_by_org,
        is_disabled: is_forced_by_org || !is_any_ai_enabled,
    }
}

#[derive(Default)]
struct AgentAttributionWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for AgentAttributionWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "agent attribution commit pull request co-author author credit oz warp"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let org_setting = UserWorkspaces::as_ref(app).get_agent_attribution_setting();
        let state = derive_agent_attribution_toggle_state(
            &org_setting,
            *ai_settings.agent_attribution_enabled,
            is_any_ai_enabled,
        );

        let ui_builder = appearance.ui_builder();
        let toggle = if state.is_forced_by_org {
            ui_builder
                .switch(self.toggle.clone())
                .check(state.is_enabled)
                .with_tooltip(TooltipConfig {
                    text: "This option is enforced by your organization's settings and cannot be customized.".to_string(),
                    styles: ui_builder.default_tool_tip_styles(),
                })
                .disable()
                .build()
                .finish()
        } else if !is_any_ai_enabled {
            ui_builder
                .switch(self.toggle.clone())
                .check(state.is_enabled)
                .with_disabled(true)
                .build()
                .finish()
        } else {
            ui_builder
                .switch(self.toggle.clone())
                .check(state.is_enabled)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(AISettingsPageAction::ToggleAgentAttribution);
                })
                .finish()
        };

        let toggle_row = build_toggle_element(
            render_body_item_label::<AISettingsPageAction>(
                "Enable agent attribution".to_string(),
                Some(styles::header_font_color(!state.is_disabled, app)),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            ),
            toggle,
            appearance,
            None,
        );

        Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                build_sub_header(
                    appearance,
                    "Agent Attribution",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            )
            .with_child(toggle_row)
            .with_child(render_ai_setting_description(
                "Galaxy can add attribution to commit messages and pull requests it creates",
                !state.is_disabled,
                app,
            ))
            .finish()
    }
}

#[cfg(test)]
#[path = "ai_page_tests.rs"]
mod tests;

#[derive(Default)]
#[allow(dead_code)]
struct CloudAgentComputerUseWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for CloudAgentComputerUseWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "oz cloud agent computer use orchestration multi-agent"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        use crate::ai::execution_profiles::{
            resolve_cloud_agent_computer_use_state, CloudAgentComputerUseState,
        };

        let is_any_ai_enabled = AISettings::as_ref(app).is_any_ai_enabled(app);

        // Determine toggle state based on workspace autonomy setting and user preference
        let CloudAgentComputerUseState {
            enabled: is_checked,
            is_forced_by_org,
        } = resolve_cloud_agent_computer_use_state(app);

        // Toggle is disabled if forced by org settings OR if AI is globally disabled
        let is_disabled = is_forced_by_org || !is_any_ai_enabled;

        let ui_builder = appearance.ui_builder();
        let toggle = if is_forced_by_org {
            // Disabled by organization setting - show tooltip on hover
            ui_builder
                .switch(self.toggle.clone())
                .check(is_checked)
                .with_tooltip(TooltipConfig {
                    text: "This option is enforced by your organization's settings and cannot be customized.".to_string(),
                    styles: ui_builder.default_tool_tip_styles(),
                })
                .disable()
                .build()
                .finish()
        } else if !is_any_ai_enabled {
            // Disabled because AI is off globally - no tooltip needed
            ui_builder
                .switch(self.toggle.clone())
                .check(is_checked)
                .with_disabled(true)
                .build()
                .finish()
        } else {
            // Enabled - allow toggling
            ui_builder
                .switch(self.toggle.clone())
                .check(is_checked)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(AISettingsPageAction::ToggleCloudAgentComputerUse);
                })
                .finish()
        };

        let toggle_row = build_toggle_element(
            render_body_item_label::<AISettingsPageAction>(
                "Computer use in Cloud Agents".to_string(),
                Some(styles::header_font_color(!is_disabled, app)),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            ),
            toggle,
            appearance,
            None,
        );

        Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                build_sub_header(
                    appearance,
                    "Experimental",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            )
            .with_child(toggle_row)
            .with_child(render_ai_setting_description(
                "Enable computer use in cloud agent conversations started from the Warp app.",
                !is_disabled,
                app,
            ))
            .finish()
    }
}

#[derive(Default)]
struct CloudHandoffWidget {
    handoff_toggle: SwitchStateHandle,
    auto_handoff_on_sleep_toggle: SwitchStateHandle,
    ampersand_toggle: SwitchStateHandle,
}

impl SettingsWidget for CloudHandoffWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "cloud handoff auto sleep ampersand & move to cloud local"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        FeatureFlag::OzHandoff.is_enabled() && FeatureFlag::HandoffLocalCloud.is_enabled()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        use crate::settings::PrivacySettings;

        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let privacy = PrivacySettings::as_ref(app);
        let cloud_convos_off = !privacy.is_cloud_conversation_storage_enabled
            || matches!(
                UserWorkspaces::as_ref(app).get_cloud_conversation_storage_enablement_setting(),
                AdminEnablementSetting::Disable
            );
        let is_force_disabled = !is_any_ai_enabled || cloud_convos_off;

        let tooltip_text = if cloud_convos_off {
            "Cloud handoff requires cloud conversations to be enabled."
        } else {
            ""
        };

        let ui_builder = appearance.ui_builder();

        let handoff_toggle = if is_force_disabled {
            let mut builder = ui_builder.switch(self.handoff_toggle.clone()).check(false);
            if !tooltip_text.is_empty() {
                builder = builder.with_tooltip(TooltipConfig {
                    text: tooltip_text.to_string(),
                    styles: ui_builder.default_tool_tip_styles(),
                });
            }
            builder.disable().build().finish()
        } else {
            ui_builder
                .switch(self.handoff_toggle.clone())
                .check(!*ai_settings.should_force_disable_cloud_handoff)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(AISettingsPageAction::ToggleCloudHandoff);
                })
                .finish()
        };

        let handoff_row = build_toggle_element(
            render_body_item_label::<AISettingsPageAction>(
                "Cloud handoff".to_string(),
                Some(styles::header_font_color(!is_force_disabled, app)),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            ),
            handoff_toggle,
            appearance,
            None,
        );

        let mut column = Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                build_sub_header(
                    appearance,
                    "Cloud Handoff",
                    Some(styles::header_font_color(is_any_ai_enabled, app)),
                )
                .with_padding_bottom(HEADER_PADDING)
                .finish(),
            )
            .with_child(handoff_row)
            .with_child(render_ai_setting_description(
                "Hand off local agent conversations to a cloud agent.",
                !is_force_disabled,
                app,
            ));

        if ai_settings.is_cloud_handoff_enabled(app) {
            if ai_settings
                .auto_handoff_on_sleep_enabled
                .is_supported_on_current_platform()
            {
                let auto_handoff_on_sleep_toggle = ui_builder
                    .switch(self.auto_handoff_on_sleep_toggle.clone())
                    .check(*ai_settings.auto_handoff_on_sleep_enabled)
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(AISettingsPageAction::ToggleAutoHandoffOnSleep);
                    })
                    .finish();
                let auto_handoff_on_sleep_row = build_toggle_element(
                    render_body_item_label::<AISettingsPageAction>(
                        "Auto-handoff before sleep".to_string(),
                        Some(styles::header_font_color(true, app)),
                        None,
                        LocalOnlyIconState::Hidden,
                        ToggleState::Enabled,
                        appearance,
                    ),
                    auto_handoff_on_sleep_toggle,
                    appearance,
                    None,
                );
                column.add_child(auto_handoff_on_sleep_row);
                column.add_child(render_ai_setting_description(
                    "When macOS is about to sleep, automatically moves the most recently focused running local Galaxy Agent conversation to Cloud Mode so it can keep working.",
                    true,
                    app,
                ));
            }
            let ampersand_toggle = ui_builder
                .switch(self.ampersand_toggle.clone())
                .check(!*ai_settings.should_force_disable_ampersand_handoff)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(AISettingsPageAction::ToggleAmpersandHandoff);
                })
                .finish();

            let ampersand_row = build_toggle_element(
                render_body_item_label::<AISettingsPageAction>(
                    "Use & to trigger handoff".to_string(),
                    Some(styles::header_font_color(true, app)),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                ),
                ampersand_toggle,
                appearance,
                None,
            );

            column.add_child(ampersand_row);
            column.add_child(render_ai_setting_description(
                "Type & as the first character to enter cloud handoff compose mode.",
                true,
                app,
            ));
        }

        column.finish()
    }
}

struct ModelsOverviewWidget;

impl SettingsWidget for ModelsOverviewWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "models providers runtimes rig litellm openai compatible ollama lm studio bedrock acp agent client protocol"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = AISettings::as_ref(app);
        let openai_count = settings
            .openai_providers
            .value()
            .iter()
            .filter(|provider| {
                ModelProviderSection::for_provider(provider) == ModelProviderSection::OpenAI
            })
            .count();
        let litellm_count = settings
            .openai_providers
            .value()
            .iter()
            .filter(|provider| {
                ModelProviderSection::for_provider(provider) == ModelProviderSection::LiteLLM
            })
            .count();
        let chatgpt_count = settings
            .openai_providers
            .value()
            .iter()
            .filter(|provider| {
                ModelProviderSection::for_provider(provider)
                    == ModelProviderSection::ChatGPTSubscription
            })
            .count();
        let endpoint_model_count = settings
            .openai_providers
            .value()
            .iter()
            .map(|provider| provider.models.len())
            .sum::<usize>();
        let bedrock_model_count = settings.bedrock_models.value().len();
        let agent_runtime_count = settings.enabled_acp_providers().len();

        Flex::column()
            .with_spacing(8.)
            .with_child(build_sub_header(appearance, "Models", None).finish())
            .with_child(render_ai_setting_description(
                "Configure Galaxy's direct model providers and agent runtimes in one place. OpenAI, LiteLLM, ChatGPT subscription, and Bedrock connections are configured here. ACP coding agents are shown read-only because the agent runtime controls their model, login, session, and tool loop.",
                true,
                app,
            ))
            .with_child(render_ai_setting_description(
                format!(
                    "{openai_count} OpenAI provider(s); {litellm_count} LiteLLM provider(s); {chatgpt_count} ChatGPT subscription provider(s); {endpoint_model_count} direct model(s); {bedrock_model_count} Bedrock model(s); {agent_runtime_count} enabled ACP runtime(s)."
                ),
                true,
                app,
            ))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelProviderSection {
    OpenAI,
    LiteLLM,
    ChatGPTSubscription,
}

impl ModelProviderSection {
    fn for_provider(provider: &OpenAIProviderConfig) -> Self {
        match provider.kind {
            OpenAIProviderKind::OpenAI
            | OpenAIProviderKind::Anthropic
            | OpenAIProviderKind::Gemini
            | OpenAIProviderKind::VertexAI => Self::OpenAI,
            OpenAIProviderKind::LiteLLM => Self::LiteLLM,
            OpenAIProviderKind::ChatGPTSubscription => Self::ChatGPTSubscription,
        }
    }
}

struct OpenAIProviderCardState {
    enabled_switch: SwitchStateHandle,
    edit_button: ViewHandle<ActionButton>,
    remove_button: ViewHandle<ActionButton>,
}

struct AcpProviderCardState {
    remove_button: ViewHandle<ActionButton>,
}

struct ProviderSettingsWidget {
    provider_type: ProviderSetupProviderType,
    bedrock_enabled_toggle: SwitchStateHandle,
    add_openai_provider_button: ViewHandle<ActionButton>,
    add_litellm_provider_button: ViewHandle<ActionButton>,
    add_chatgpt_provider_button: ViewHandle<ActionButton>,
    bedrock_add_button: ViewHandle<ActionButton>,
    acp_add_button: ViewHandle<ActionButton>,
    bedrock_edit_button: ViewHandle<ActionButton>,
    bedrock_remove_button: ViewHandle<ActionButton>,
    provider_cards: Vec<OpenAIProviderCardState>,
    acp_provider_cards: Vec<AcpProviderCardState>,
}

impl ProviderSettingsWidget {
    fn new(
        ctx: &mut ViewContext<<Self as SettingsWidget>::View>,
        provider_type: ProviderSetupProviderType,
    ) -> Self {
        let providers = AISettings::as_ref(ctx).openai_providers.value().clone();
        let provider_cards = providers
            .iter()
            .enumerate()
            .map(|(index, _)| OpenAIProviderCardState {
                enabled_switch: SwitchStateHandle::default(),
                edit_button: ctx.add_typed_action_view(move |_| {
                    ActionButton::new("Edit", SecondaryTheme).on_click(move |ctx| {
                        ctx.dispatch_typed_action(AISettingsPageAction::EditOpenAIProvider(index));
                    })
                }),
                remove_button: ctx.add_typed_action_view(move |_| {
                    ActionButton::new("Delete", DangerSecondaryTheme).on_click(move |ctx| {
                        ctx.dispatch_typed_action(AISettingsPageAction::RemoveOpenAIProvider(
                            index,
                        ));
                    })
                }),
            })
            .collect();
        let acp_providers = AISettings::as_ref(ctx).configured_acp_providers();
        let acp_provider_cards = acp_providers
            .iter()
            .map(|provider| {
                let provider_id = provider.id.clone();
                AcpProviderCardState {
                    remove_button: ctx.add_typed_action_view(move |_| {
                        ActionButton::new("Delete", DangerSecondaryTheme).on_click({
                            let provider_id = provider_id.clone();
                            move |ctx| {
                                ctx.dispatch_typed_action(AISettingsPageAction::RemoveAcpProvider(
                                    provider_id.clone(),
                                ));
                            }
                        })
                    }),
                }
            })
            .collect();
        let add_openai_provider_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add provider", SecondaryTheme)
                .with_icon(Icon::Plus)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::AddOpenAIProvider(
                        ProviderSetupProviderType::OpenAI,
                    ));
                })
        });
        let add_litellm_provider_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add provider", SecondaryTheme)
                .with_icon(Icon::Plus)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::AddOpenAIProvider(
                        ProviderSetupProviderType::LiteLLM,
                    ));
                })
        });
        let add_chatgpt_provider_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add provider", SecondaryTheme)
                .with_icon(Icon::Plus)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::AddOpenAIProvider(
                        ProviderSetupProviderType::ChatGPTSubscription,
                    ));
                })
        });
        let bedrock_add_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add provider", SecondaryTheme)
                .with_icon(Icon::Plus)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::EditBedrockProvider);
                })
        });
        let acp_add_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add provider", SecondaryTheme)
                .with_icon(Icon::Plus)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(AISettingsPageAction::AddOpenAIProvider(
                        ProviderSetupProviderType::Acp,
                    ));
                })
        });
        let bedrock_edit_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Edit", SecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(AISettingsPageAction::EditBedrockProvider);
            })
        });
        let bedrock_remove_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Delete", DangerSecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(AISettingsPageAction::RemoveBedrockProvider);
            })
        });
        Self {
            provider_type,
            bedrock_enabled_toggle: SwitchStateHandle::default(),
            add_openai_provider_button,
            add_litellm_provider_button,
            add_chatgpt_provider_button,
            bedrock_add_button,
            acp_add_button,
            bedrock_edit_button,
            bedrock_remove_button,
            provider_cards,
            acp_provider_cards,
        }
    }

    fn provider_status(provider: &OpenAIProviderConfig) -> String {
        if !provider.enabled {
            return "Disabled".to_string();
        }

        let enabled_models = provider.models.iter().filter(|model| model.enabled).count();
        if enabled_models == 0 {
            "No models enabled".to_string()
        } else {
            format!(
                "{enabled_models} enabled model{}",
                if enabled_models == 1 { "" } else { "s" }
            )
        }
    }

    fn provider_type(provider: &OpenAIProviderConfig) -> &'static str {
        match provider.kind {
            OpenAIProviderKind::OpenAI => "OpenAI",
            OpenAIProviderKind::LiteLLM => "LiteLLM",
            OpenAIProviderKind::ChatGPTSubscription => "ChatGPT subscription",
            OpenAIProviderKind::Anthropic => "Anthropic",
            OpenAIProviderKind::Gemini => "Google Gemini",
            OpenAIProviderKind::VertexAI => "Google Vertex AI",
        }
    }

    fn format_openai_model_details(model: &OpenAIModelConfig) -> String {
        let mut details = vec![
            if model.enabled { "Enabled" } else { "Disabled" }.to_string(),
            format!("Context: {}", model.context_size),
        ];

        if let Some(max_input_tokens) = model.max_input_tokens {
            details.push(format!("Max input: {max_input_tokens}"));
        }
        if let Some(max_output_tokens) = model.max_output_tokens {
            details.push(format!("Max output: {max_output_tokens}"));
        }
        if model.effective_vision_supported() {
            details.push("Images".to_string());
        }
        if model.use_rig {
            details.push("Rig".to_string());
        }
        if !model.reasoning_efforts.is_empty() {
            details.push(format!("Reasoning: {}", model.reasoning_efforts.join(", ")));
        }
        if !model.capability_overrides.is_empty() {
            let overrides = model
                .capability_overrides
                .iter()
                .sorted_by_key(|(key, _)| key.as_str())
                .map(|(key, value)| format!("{}: {}", key.replace('_', " "), value.label()))
                .join(", ");
            details.push(format!("Overrides: {overrides}"));
        }

        details.join(" · ")
    }

    fn render_model_row(
        display_name: String,
        model_id: String,
        details: String,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_spacing(3.)
                .with_child(
                    Text::new(display_name, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                        .with_color(appearance.theme().active_ui_text_color().into())
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .finish(),
                )
                .with_child(
                    Text::new(model_id, appearance.monospace_font_family(), 10.)
                        .with_color(appearance.theme().nonactive_ui_text_color().into())
                        .soft_wrap(true)
                        .finish(),
                )
                .with_child(
                    Text::new(details, appearance.ui_font_family(), 11.)
                        .with_color(appearance.theme().nonactive_ui_text_color().into())
                        .soft_wrap(true)
                        .finish(),
                )
                .finish(),
        )
        .with_padding(Padding::uniform(10.))
        .with_border(Border::bottom(1.).with_border_fill(appearance.theme().outline()))
        .finish()
    }

    fn render_model_catalog(
        rows: Vec<Box<dyn Element>>,
        empty_message: &'static str,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        if rows.is_empty() {
            return Text::new(
                empty_message,
                appearance.ui_font_family(),
                CONTENT_FONT_SIZE,
            )
            .with_color(appearance.theme().nonactive_ui_text_color().into())
            .soft_wrap(true)
            .finish();
        }

        Container::new(Flex::column().with_children(rows).finish())
            .with_background(appearance.theme().surface_2())
            .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .finish()
    }

    fn render_openai_model_catalog(
        provider: &OpenAIProviderConfig,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let rows = provider
            .models
            .iter()
            .map(|model| {
                Self::render_model_row(
                    model.display_name.clone(),
                    model.model_id.clone(),
                    Self::format_openai_model_details(model),
                    appearance,
                )
            })
            .collect::<Vec<_>>();
        Self::render_model_catalog(
            rows,
            "No models configured for this connection.",
            appearance,
        )
    }

    fn render_bedrock_model_catalog(
        models: &[BedrockModelConfig],
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let rows = models
            .iter()
            .map(|model| {
                let mut details = Vec::new();
                if model.vision_supported {
                    details.push("Images".to_string());
                }
                if model.use_rig {
                    details.push("Rig".to_string());
                }
                Self::render_model_row(
                    model.display_name.clone(),
                    model.model_id.clone(),
                    if details.is_empty() {
                        "Available".to_string()
                    } else {
                        details.join(" · ")
                    },
                    appearance,
                )
            })
            .collect::<Vec<_>>();
        Self::render_model_catalog(rows, "No Bedrock models discovered yet.", appearance)
    }

    fn render_acp_model_catalog(
        provider: &AcpProviderConfig,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        if provider.config_options.is_empty() {
            return Text::new(
                "No ACP model or mode catalog has been discovered yet.",
                appearance.ui_font_family(),
                CONTENT_FONT_SIZE,
            )
            .with_color(appearance.theme().nonactive_ui_text_color().into())
            .soft_wrap(true)
            .finish();
        }

        let rows = provider
            .config_options
            .iter()
            .filter(|option| {
                matches!(
                    option.category.as_deref(),
                    Some("model") | Some("thought_level") | Some("mode")
                )
            })
            .map(|option| {
                let details = if option.options.is_empty() {
                    option.current_value.to_string()
                } else {
                    option
                        .options
                        .iter()
                        .map(|value| value.name.as_str())
                        .join(", ")
                };
                Self::render_model_row(option.name.clone(), option.id.clone(), details, appearance)
            })
            .collect::<Vec<_>>();
        Self::render_model_catalog(
            rows,
            "No ACP model or mode catalog has been discovered yet.",
            appearance,
        )
    }

    fn render_provider_card(
        &self,
        provider_index: usize,
        provider: &OpenAIProviderConfig,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let Some(card_state) = self.provider_cards.get(provider_index) else {
            return Empty::new().finish();
        };
        let summary = format!(
            "{} model{} available · {}",
            provider.models.len(),
            if provider.models.len() == 1 { "" } else { "s" },
            Self::provider_status(provider),
        );

        let title = Text::new(
            provider.name.clone(),
            appearance.ui_font_family(),
            appearance.header_font_size(),
        )
        .with_color(appearance.theme().active_ui_text_color().into())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish();
        let provider_type = Text::new(
            Self::provider_type(provider),
            appearance.ui_font_family(),
            CONTENT_FONT_SIZE,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish();
        let summary = Text::new(summary, appearance.ui_font_family(), CONTENT_FONT_SIZE)
            .with_color(appearance.theme().nonactive_ui_text_color().into())
            .soft_wrap(true)
            .finish();

        let actions = Flex::row()
            .with_spacing(8.)
            .with_child(ChildView::new(&card_state.edit_button).finish())
            .with_child(ChildView::new(&card_state.remove_button).finish())
            .finish();
        let header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Flex::column()
                    .with_spacing(4.)
                    .with_child(title)
                    .with_child(provider_type)
                    .finish(),
            )
            .with_child(actions)
            .finish();
        let toggle = appearance
            .ui_builder()
            .switch(card_state.enabled_switch.clone())
            .check(provider.enabled)
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(AISettingsPageAction::ToggleOpenAIProviderEnabled(
                    provider_index,
                ));
            })
            .finish();
        let enablement = Flex::row()
            .with_spacing(8.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(toggle)
            .with_child(
                Text::new(
                    if provider.enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                    appearance.ui_font_family(),
                    CONTENT_FONT_SIZE,
                )
                .with_color(appearance.theme().active_ui_text_color().into())
                .finish(),
            )
            .finish();

        Container::new(
            Flex::column()
                .with_spacing(12.)
                .with_child(header)
                .with_child(summary)
                .with_child(enablement)
                .with_child(Self::render_openai_model_catalog(provider, appearance))
                .finish(),
        )
        .with_padding(Padding::uniform(16.))
        .with_background(appearance.theme().surface_1())
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .finish()
    }

    fn render_bedrock_provider_card(
        &self,
        title: &str,
        description: &'static str,
        enabled: bool,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let toggle = appearance
            .ui_builder()
            .switch(self.bedrock_enabled_toggle.clone())
            .check(enabled)
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(AISettingsPageAction::ToggleBedrockEnabled);
            })
            .finish();
        let header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Flex::column()
                    .with_spacing(4.)
                    .with_child(
                        Text::new(
                            title.to_string(),
                            appearance.ui_font_family(),
                            appearance.header_font_size(),
                        )
                        .with_color(appearance.theme().active_ui_text_color().into())
                        .with_style(Properties::default().weight(Weight::Bold))
                        .finish(),
                    )
                    .with_child(
                        Text::new(description, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                            .with_color(appearance.theme().nonactive_ui_text_color().into())
                            .soft_wrap(true)
                            .finish(),
                    )
                    .finish(),
            )
            .with_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_child(ChildView::new(&self.bedrock_edit_button).finish())
                    .with_child(ChildView::new(&self.bedrock_remove_button).finish())
                    .finish(),
            )
            .finish();
        Container::new(
            Flex::column()
                .with_spacing(12.)
                .with_child(header)
                .with_child(
                    Flex::row()
                        .with_spacing(8.)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(toggle)
                        .with_child(
                            Text::new(
                                if enabled { "Enabled" } else { "Disabled" },
                                appearance.ui_font_family(),
                                CONTENT_FONT_SIZE,
                            )
                            .with_color(appearance.theme().active_ui_text_color().into())
                            .finish(),
                        )
                        .finish(),
                )
                .with_child(Self::render_bedrock_model_catalog(
                    AISettings::as_ref(app).bedrock_models.value(),
                    appearance,
                ))
                .finish(),
        )
        .with_padding(Padding::uniform(16.))
        .with_background(appearance.theme().surface_1())
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .finish()
    }

    fn render_acp_provider_card(
        &self,
        provider_index: usize,
        provider: &AcpProviderConfig,
        description: &'static str,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let Some(card_state) = self.acp_provider_cards.get(provider_index) else {
            return Empty::new().finish();
        };
        let status = format!("Read-only · Agent: {}", provider.agent_id);
        let header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Flex::column()
                    .with_spacing(4.)
                    .with_child(
                        Text::new(
                            provider.display_name(),
                            appearance.ui_font_family(),
                            appearance.header_font_size(),
                        )
                        .with_color(appearance.theme().active_ui_text_color().into())
                        .with_style(Properties::default().weight(Weight::Bold))
                        .finish(),
                    )
                    .with_child(
                        Text::new(description, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                            .with_color(appearance.theme().nonactive_ui_text_color().into())
                            .soft_wrap(true)
                            .finish(),
                    )
                    .finish(),
            )
            .with_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        Text::new(status, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                            .with_color(appearance.theme().nonactive_ui_text_color().into())
                            .finish(),
                    )
                    .with_child(ChildView::new(&card_state.remove_button).finish())
                    .finish(),
            )
            .finish();

        Container::new(
            Flex::column()
                .with_spacing(12.)
                .with_child(header)
                .with_child(Self::render_acp_model_catalog(provider, appearance))
                .finish(),
        )
        .with_padding(Padding::uniform(16.))
        .with_background(appearance.theme().surface_1())
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .finish()
    }

    fn render_inline_setup(appearance: &Appearance, view: &AISettingsPageView) -> Box<dyn Element> {
        Container::new(ChildView::new(&view.provider_setup_body).finish())
            .with_padding(Padding::uniform(16.))
            .with_background(appearance.theme().surface_1())
            .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .finish()
    }

    fn model_section_for_provider_type(
        provider_type: ProviderSetupProviderType,
    ) -> Option<ModelProviderSection> {
        match provider_type {
            ProviderSetupProviderType::OpenAI => Some(ModelProviderSection::OpenAI),
            ProviderSetupProviderType::LiteLLM => Some(ModelProviderSection::LiteLLM),
            ProviderSetupProviderType::ChatGPTSubscription => {
                Some(ModelProviderSection::ChatGPTSubscription)
            }
            ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI
            | ProviderSetupProviderType::Bedrock
            | ProviderSetupProviderType::Acp => None,
        }
    }

    fn provider_page_title(provider_type: ProviderSetupProviderType) -> &'static str {
        match provider_type {
            ProviderSetupProviderType::OpenAI => "OpenAI",
            ProviderSetupProviderType::LiteLLM => "LiteLLM",
            ProviderSetupProviderType::ChatGPTSubscription => "ChatGPT Subscription",
            ProviderSetupProviderType::Bedrock => "Bedrock",
            ProviderSetupProviderType::Acp => "ACP",
            ProviderSetupProviderType::Anthropic => "Anthropic",
            ProviderSetupProviderType::Gemini => "Google Gemini",
            ProviderSetupProviderType::VertexAI => "Google Vertex AI",
        }
    }

    fn provider_page_description(provider_type: ProviderSetupProviderType) -> &'static str {
        match provider_type {
            ProviderSetupProviderType::OpenAI => {
                "Connect directly to OpenAI-compatible OpenAI API endpoints and configure the models available from each connection."
            }
            ProviderSetupProviderType::LiteLLM => {
                "Connect LiteLLM endpoints and use LiteLLM model metadata APIs during model discovery."
            }
            ProviderSetupProviderType::ChatGPTSubscription => {
                "Use your ChatGPT subscription through the supported authentication flow."
            }
            ProviderSetupProviderType::Bedrock => {
                "Use AWS Bedrock credentials and discovered Bedrock foundation models."
            }
            ProviderSetupProviderType::Acp => {
                "Add an ACP agent connection here. After it is added, Settings shows it read-only because the agent controls its model, login, session, and tool loop."
            }
            ProviderSetupProviderType::Anthropic => "Connect directly to Anthropic.",
            ProviderSetupProviderType::Gemini => "Connect directly to Google Gemini.",
            ProviderSetupProviderType::VertexAI => "Connect directly to Google Vertex AI.",
        }
    }

    fn empty_message_for_type(provider_type: ProviderSetupProviderType) -> &'static str {
        match provider_type {
            ProviderSetupProviderType::OpenAI => "No OpenAI provider configured.",
            ProviderSetupProviderType::LiteLLM => "No LiteLLM provider configured.",
            ProviderSetupProviderType::ChatGPTSubscription => {
                "No ChatGPT subscription provider configured."
            }
            ProviderSetupProviderType::Bedrock => "No Bedrock provider configured.",
            ProviderSetupProviderType::Acp => "No ACP agent connection configured.",
            ProviderSetupProviderType::Anthropic => "No Anthropic provider configured.",
            ProviderSetupProviderType::Gemini => "No Google Gemini provider configured.",
            ProviderSetupProviderType::VertexAI => "No Google Vertex AI provider configured.",
        }
    }

    fn add_button_for_type(
        &self,
        provider_type: ProviderSetupProviderType,
    ) -> Option<&ViewHandle<ActionButton>> {
        match provider_type {
            ProviderSetupProviderType::OpenAI => Some(&self.add_openai_provider_button),
            ProviderSetupProviderType::LiteLLM => Some(&self.add_litellm_provider_button),
            ProviderSetupProviderType::ChatGPTSubscription => {
                Some(&self.add_chatgpt_provider_button)
            }
            ProviderSetupProviderType::Bedrock => Some(&self.bedrock_add_button),
            ProviderSetupProviderType::Acp => Some(&self.acp_add_button),
            ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI => None,
        }
    }

    fn render_provider_section(
        title: &'static str,
        description: &'static str,
        add_button: Option<&ViewHandle<ActionButton>>,
        cards: Vec<Box<dyn Element>>,
        empty_message: &'static str,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(build_sub_header(appearance, title, None).finish());
        if let Some(button) = add_button {
            header = header.with_child(ChildView::new(button).finish());
        }

        let mut section = Flex::column()
            .with_spacing(10.)
            .with_child(header.finish())
            .with_child(
                Text::new(description, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
            );

        if cards.is_empty() {
            section = section.with_child(
                Text::new(
                    empty_message,
                    appearance.ui_font_family(),
                    CONTENT_FONT_SIZE,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
            );
        } else {
            for card in cards {
                section.add_child(card);
            }
        }

        section.finish()
    }
}

impl SettingsWidget for ProviderSettingsWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "openai chatgpt pro subscription litellm custom provider endpoint api key models acp agent client protocol"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        true
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = AISettings::as_ref(app);
        let providers = settings.openai_providers.value();

        let mut column = Flex::column().with_spacing(18.);

        let is_setup_visible = view
            .inline_provider_setup
            .is_some_and(|state| state.provider_type == self.provider_type);

        match self.provider_type {
            ProviderSetupProviderType::OpenAI
            | ProviderSetupProviderType::LiteLLM
            | ProviderSetupProviderType::ChatGPTSubscription => {
                if is_setup_visible {
                    column.add_child(Self::render_inline_setup(appearance, view));
                }

                let cards = Self::model_section_for_provider_type(self.provider_type)
                    .map(|section| {
                        providers
                            .iter()
                            .enumerate()
                            .filter(|(_, provider)| {
                                ModelProviderSection::for_provider(provider) == section
                            })
                            .map(|(index, provider)| {
                                self.render_provider_card(index, provider, appearance)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                column.add_child(Self::render_provider_section(
                    Self::provider_page_title(self.provider_type),
                    Self::provider_page_description(self.provider_type),
                    if is_setup_visible {
                        None
                    } else {
                        self.add_button_for_type(self.provider_type)
                    },
                    cards,
                    Self::empty_message_for_type(self.provider_type),
                    appearance,
                ));
            }
            ProviderSetupProviderType::Bedrock => {
                if is_setup_visible {
                    column.add_child(Self::render_inline_setup(appearance, view));
                }

                let bedrock_cards = if settings.bedrock_models.value().is_empty() {
                    Vec::new()
                } else {
                    vec![self.render_bedrock_provider_card(
                        settings.bedrock_connection_name.value().as_str(),
                        "Use AWS credentials to access Bedrock foundation models directly.",
                        *settings.bedrock_enabled.value(),
                        appearance,
                        app,
                    )]
                };
                let bedrock_add_button =
                    if !is_setup_visible && settings.bedrock_models.value().is_empty() {
                        Some(&self.bedrock_add_button)
                    } else {
                        None
                    };
                column.add_child(Self::render_provider_section(
                    "Bedrock",
                    "Use AWS Bedrock credentials and discovered Bedrock foundation models.",
                    bedrock_add_button,
                    bedrock_cards,
                    "No Bedrock provider configured.",
                    appearance,
                ));
            }
            ProviderSetupProviderType::Acp => {
                if is_setup_visible {
                    column.add_child(Self::render_inline_setup(appearance, view));
                }

                let acp_supported = cfg!(unix) && FeatureFlag::AgentClientProtocol.is_enabled();
                let acp_cards = if acp_supported {
                    settings
                        .configured_acp_providers()
                        .iter()
                        .enumerate()
                        .map(|(index, provider)| {
                            self.render_acp_provider_card(
                                index,
                                provider,
                            "Use a local session-oriented agent that owns its model, login, session, and tool loop.",
                            appearance,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let acp_add_button = if acp_supported && !is_setup_visible {
                    Some(&self.acp_add_button)
                } else {
                    None
                };
                let empty_message = if acp_supported {
                    "No ACP agent connection configured."
                } else {
                    "ACP providers are not available on this platform."
                };
                column.add_child(Self::render_provider_section(
                    "ACP",
                    "Add an ACP agent connection here. After it is added, Settings shows it read-only because the agent controls its model, login, session, and tool loop.",
                    acp_add_button,
                    acp_cards,
                    empty_message,
                    appearance,
                ));
            }
            ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI => {}
        }

        column.finish()
    }
}

/// Stable `&'static str` id for the custom model routers settings widget,
/// exposed for the `warp://settings?widget=custom_router` deeplink (see
/// `settings_widget_deeplink_target`).
pub(crate) fn custom_model_routers_widget_id() -> &'static str {
    CustomModelRoutersWidget::static_widget_id()
}

#[derive(Default)]
struct CustomModelRoutersWidget;

impl SettingsWidget for CustomModelRoutersWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "custom model router complexity prompt auto model routing"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        FeatureFlag::CustomModelRouters.is_enabled()
    }

    #[cfg_attr(not(feature = "local_fs"), allow(unused_variables))]
    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_any_ai_enabled = AISettings::as_ref(app).is_any_ai_enabled(app);
        let header_color = styles::header_font_color(is_any_ai_enabled, app);

        // Header row: "Custom Model Routers" + add button
        let header_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(build_sub_header(appearance, "Custom Routers", Some(header_color)).finish())
            .with_child({
                #[cfg(feature = "local_fs")]
                {
                    galaxyui::elements::Container::new(
                        view.add_router_button.as_ref(app).render(app),
                    )
                    .with_margin_bottom(4.)
                    .with_margin_top(-4.)
                    .finish()
                }
                #[cfg(not(feature = "local_fs"))]
                {
                    galaxyui::elements::Empty::new().finish()
                }
            })
            .finish();

        let column = Flex::column()
            .with_child(render_separator(appearance))
            .with_child(
                Container::new(header_row)
                    .with_padding_bottom(HEADER_PADDING)
                    .finish(),
            )
            .with_child(render_ai_setting_description(
                "Automatically route tasks to specific models based on task complexity or custom rules. Custom routers will appear in your model selector menu.",
                is_any_ai_enabled,
                app,
            ));

        // Error cards and router summary cards (local_fs only)
        #[cfg(feature = "local_fs")]
        let column = {
            use super::custom_router_view::render_router_error_card;
            use crate::user_config::GalaxyConfig;
            let mut c = column;
            // Error cards (files that failed to parse) — shown first
            let errors = GalaxyConfig::as_ref(app).custom_model_router_errors();
            for error in errors.iter() {
                c.add_child(
                    Container::new(render_router_error_card(
                        &error.file_name,
                        &error.error_message,
                        appearance,
                    ))
                    .with_margin_top(8.)
                    .finish(),
                );
            }
            // Router summary cards
            for view_handle in &view.router_views {
                c.add_child(
                    Container::new(galaxyui::elements::ChildView::new(view_handle).finish())
                        .with_margin_top(8.)
                        .finish(),
                );
            }
            c
        };

        // Add trailing space beneath this section (matching sibling sections
        // like AWS Bedrock) so the following section's title isn't crowded
        // against the router cards.
        Container::new(column.finish())
            .with_margin_bottom(HEADER_PADDING)
            .finish()
    }
}

// ── Experiments widget ──────────────────────────────────────────────────────

#[derive(Default)]
struct ExperimentsWidget {
    crosscheck_toggle: SwitchStateHandle,
}

impl SettingsWidget for ExperimentsWidget {
    type View = AISettingsPageView;

    fn search_terms(&self) -> &str {
        "experiments crosscheck review reviewer sub-agent feedback loop lgtm"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        FeatureFlag::CrosscheckWork.is_enabled()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);
        let is_any_ai_enabled = ai_settings.is_any_ai_enabled(app);

        let header = build_sub_header(
            appearance,
            "Experiments",
            Some(styles::header_font_color(is_any_ai_enabled, app)),
        )
        .with_margin_bottom(HEADER_PADDING)
        .finish();

        let crosscheck_toggle = render_ai_setting_toggle::<CrosscheckEnabled>(
            "Crosscheck Work",
            AISettingsPageAction::ToggleCrosscheckEnabled,
            *ai_settings.crosscheck_enabled,
            is_any_ai_enabled,
            self.crosscheck_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let crosscheck_description = render_ai_setting_description(
            "When enabled, a reviewer agent critiques the main agent's output after each turn. \
             The feedback loop continues until the reviewer responds with \"LGTM!\" or the \
             maximum iteration count is reached.",
            is_any_ai_enabled,
            app,
        );

        let model_description = render_ai_setting_description(
            "Configure the reviewer model and max iterations in settings.toml under \
             [agents.experiments].",
            is_any_ai_enabled,
            app,
        );

        Flex::column()
            .with_child(header)
            .with_child(crosscheck_toggle)
            .with_child(crosscheck_description)
            .with_child(model_description)
            .finish()
    }
}

mod styles {
    use galaxy_core::ui::appearance::Appearance;
    use galaxy_core::ui::theme::Fill;
    use galaxyui::{AppContext, SingletonEntity};

    // Apply a negative margin to the description text so it appears closer to the main
    // settings option text.
    pub const DESCRIPTION_NEGATIVE_MARGIN_OFFSET: f32 = -12.;

    /// The space between a description and the next toggle.
    pub const DESCRIPTION_MARGIN_BOTTOM: f32 = 12.;

    /// Margin to leave for switch toggle to the right of the description subtext.
    pub const TOGGLE_WIDTH_MARGIN: f32 = 48.;

    pub fn header_font_color(is_enabled_setting: bool, app: &AppContext) -> Fill {
        let appearance = Appearance::as_ref(app);
        if is_enabled_setting {
            appearance
                .theme()
                .main_text_color(appearance.theme().surface_2())
        } else {
            appearance.theme().disabled_ui_text_color()
        }
    }

    pub fn description_font_color(is_enabled_setting: bool, app: &AppContext) -> Fill {
        let appearance = Appearance::as_ref(app);
        if is_enabled_setting {
            appearance
                .theme()
                .sub_text_color(appearance.theme().surface_1())
        } else {
            appearance.theme().disabled_ui_text_color()
        }
    }
}
