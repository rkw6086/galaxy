use galaxy_cli::agent::Harness;
use galaxy_core::ui::theme::Fill;
use galaxyui::clipboard::ClipboardContent;
use galaxyui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, Flex, FormattedTextElement, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, Padding, ParentElement, Radius, ScrollbarWidth, Text,
};
use galaxyui::fonts::{Properties, Weight};
use galaxyui::text_layout::ClipConfig;
use galaxyui::ui_components::button::ButtonVariant;
use galaxyui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use galaxyui::ui_components::switch::SwitchStateHandle;
use galaxyui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};
use pathfinder_color::ColorU;

#[cfg(not(target_family = "wasm"))]
use crate::ai::chatgpt_auth::{ChatGPTAuthModel, ChatGPTAuthModelEvent, ChatGPTAuthState};
use crate::ai::harness_display;
use crate::ai::llms::{merge_discovered_provider_models, LLMPreferences};
use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::modal::{Modal, ModalViewState};
use crate::settings::ai::{
    AcpConfigOptionSettings, BedrockAuthMethod, BedrockModelConfig, ModelCapabilityOverride,
    OpenAIModelConfig, OpenAIProviderConfig, OpenAIProviderKind,
};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, NakedTheme, PrimaryTheme, SecondaryTheme,
};

const MODAL_WIDTH: f32 = 900.;
const MODAL_HEIGHT: f32 = 700.;
const BODY_HEIGHT: f32 = 630.;
const INPUT_FONT_SIZE: f32 = 12.;
const MODEL_LOGO_SIZE: f32 = 20.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderSetupStep {
    ProviderType,
    Configure,
    Discover,
    Models,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSetupProviderType {
    ChatGPTSubscription,
    OpenAICompatible,
    Anthropic,
    Gemini,
    VertexAI,
    Bedrock,
    Acp,
}

const PROVIDER_TYPE_OPTIONS: &[(ProviderSetupProviderType, &str, &str)] = &[
    (
        ProviderSetupProviderType::ChatGPTSubscription,
        "ChatGPT subscription",
        "Use your ChatGPT Plus or Pro subscription with native OAuth.",
    ),
    (
        ProviderSetupProviderType::OpenAICompatible,
        "OpenAI-compatible API",
        "Connect LiteLLM, Ollama, vLLM, or another compatible endpoint.",
    ),
    (
        ProviderSetupProviderType::Anthropic,
        "Anthropic",
        "Connect directly to Anthropic's native Messages API with an API key.",
    ),
    (
        ProviderSetupProviderType::Gemini,
        "Google Gemini",
        "Connect directly to Google's Gemini API with an API key.",
    ),
    (
        ProviderSetupProviderType::VertexAI,
        "Google Vertex AI",
        "Use Google Cloud Application Default Credentials for Vertex-hosted Gemini models.",
    ),
    (
        ProviderSetupProviderType::Bedrock,
        "AWS Bedrock",
        "Use the AWS Bedrock credentials and model configuration already managed by Galaxy.",
    ),
    (
        ProviderSetupProviderType::Acp,
        "ACP agent runtime",
        "Use a session-oriented ACP agent that owns its model and authentication.",
    ),
];

#[derive(Clone, Debug)]
pub struct BedrockProviderDraft {
    pub name: String,
    pub auth_method: BedrockAuthMethod,
    pub profile: String,
    pub region: String,
    pub cross_region_inference: bool,
    pub auto_login: bool,
    pub auth_refresh_command: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub models: Vec<BedrockModelConfig>,
}

#[derive(Clone, Debug)]
pub struct AcpProviderDraft {
    pub name: String,
    pub agent_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub config_options: Vec<AcpConfigOptionSettings>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DiscoveryState {
    Idle,
    Loading,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapabilityKey {
    Vision,
    Files,
    Audio,
    Tools,
    SystemMessages,
}

impl CapabilityKey {
    const ALL: [Self; 5] = [
        Self::Vision,
        Self::Files,
        Self::Audio,
        Self::Tools,
        Self::SystemMessages,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Vision => "Images",
            Self::Files => "Files",
            Self::Audio => "Audio",
            Self::Tools => "Tools",
            Self::SystemMessages => "System",
        }
    }

    fn setting_key(self) -> &'static str {
        match self {
            Self::Vision => "vision",
            Self::Files => "files",
            Self::Audio => "audio",
            Self::Tools => "tools",
            Self::SystemMessages => "system_messages",
        }
    }
}

pub enum ProviderSetupModalBodyEvent {
    Close,
    RequestAcpDiscovery(AcpProviderDraft),
    SaveOpenAI {
        editing_index: Option<usize>,
        provider: OpenAIProviderConfig,
    },
    SaveBedrock(BedrockProviderDraft),
    SaveAcp(AcpProviderDraft),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderSetupModalBodyAction {
    SelectProvider(ProviderSetupProviderType),
    Next,
    Back,
    Cancel,
    ToggleModel(usize),
    CycleModelCapability(usize, CapabilityKey),
    ConnectChatGPT,
    OpenChatGPTDevicePage,
    CopyChatGPTDeviceCode,
    SelectBedrockAuth(BedrockAuthMethod),
    ToggleBedrockCrossRegion,
    ToggleBedrockAutoLogin,
    SelectAcpAgent(String),
}

pub type ProviderSetupModalState = ModalViewState<Modal<ProviderSetupModalBody>>;

pub struct ProviderSetupModalBody {
    step: ProviderSetupStep,
    editing_index: Option<usize>,
    provider_type: ProviderSetupProviderType,
    draft_name: String,
    draft_base_url: String,
    draft_api_key: Option<String>,
    draft_project_id: String,
    draft_location: String,
    draft_models: Vec<OpenAIModelConfig>,
    draft_bedrock: BedrockProviderDraft,
    draft_acp: AcpProviderDraft,
    discovery_state: DiscoveryState,
    provider_type_buttons: Vec<ViewHandle<ActionButton>>,
    acp_agent_buttons: Vec<ViewHandle<ActionButton>>,
    name_editor: ViewHandle<EditorView>,
    base_url_editor: ViewHandle<EditorView>,
    api_key_editor: ViewHandle<EditorView>,
    project_id_editor: ViewHandle<EditorView>,
    location_editor: ViewHandle<EditorView>,
    bedrock_profile_editor: ViewHandle<EditorView>,
    bedrock_region_editor: ViewHandle<EditorView>,
    bedrock_refresh_command_editor: ViewHandle<EditorView>,
    bedrock_access_key_editor: ViewHandle<EditorView>,
    bedrock_secret_key_editor: ViewHandle<EditorView>,
    acp_agent_id_editor: ViewHandle<EditorView>,
    acp_command_editor: ViewHandle<EditorView>,
    acp_args_editor: ViewHandle<EditorView>,
    chatgpt_connect_mouse_state: MouseStateHandle,
    chatgpt_open_mouse_state: MouseStateHandle,
    chatgpt_copy_mouse_state: MouseStateHandle,
    bedrock_auth_buttons: Vec<ViewHandle<ActionButton>>,
    bedrock_cross_region_toggle: SwitchStateHandle,
    bedrock_auto_login_toggle: SwitchStateHandle,
    model_switches: Vec<SwitchStateHandle>,
    model_capability_switches: Vec<[SwitchStateHandle; 2]>,
    model_capability_buttons: Vec<Vec<ViewHandle<ActionButton>>>,
    model_context_editors: Vec<ViewHandle<EditorView>>,
    provider_type_scroll_state: ClippedScrollStateHandle,
    models_scroll_state: ClippedScrollStateHandle,
    back_button: ViewHandle<ActionButton>,
    cancel_button: ViewHandle<ActionButton>,
    next_button: ViewHandle<ActionButton>,
}

impl ProviderSetupModalBody {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let provider_type_buttons = PROVIDER_TYPE_OPTIONS
            .iter()
            .map(|(kind, label, _)| {
                let kind = *kind;
                let label = *label;
                ctx.add_typed_action_view(move |_| {
                    ActionButton::new(label, NakedTheme)
                        .with_full_width(true)
                        .on_click(move |ctx| {
                            ctx.dispatch_typed_action(
                                ProviderSetupModalBodyAction::SelectProvider(kind),
                            );
                        })
                })
            })
            .collect();

        let name_editor = Self::create_editor("Connection name", false, ctx);
        let base_url_editor = Self::create_editor("https://api.example.com/v1", false, ctx);
        let api_key_editor = Self::create_editor("sk-... (optional)", true, ctx);
        let project_id_editor = Self::create_editor("my-google-cloud-project", false, ctx);
        let location_editor = Self::create_editor("global", false, ctx);
        let bedrock_profile_editor = Self::create_editor("default", false, ctx);
        let bedrock_region_editor = Self::create_editor("us-east-1", false, ctx);
        let bedrock_refresh_command_editor = Self::create_editor("aws sso login", false, ctx);
        let bedrock_access_key_editor = Self::create_editor("AKIA...", false, ctx);
        let bedrock_secret_key_editor = Self::create_editor("Secret access key", true, ctx);
        let acp_agent_id_editor = Self::create_editor("codex", false, ctx);
        let acp_command_editor = Self::create_editor("Optional executable", false, ctx);
        let acp_args_editor = Self::create_editor(r#"["arg1", "arg2"]"#, false, ctx);

        let mut acp_agent_buttons = galaxy_acp::known_acp_agents()
            .iter()
            .map(|agent| {
                let id = agent.id.to_owned();
                ctx.add_typed_action_view(move |_| {
                    ActionButton::new(agent.name, NakedTheme)
                        .with_full_width(true)
                        .on_click({
                            let id = id.clone();
                            move |ctx| {
                                ctx.dispatch_typed_action(
                                    ProviderSetupModalBodyAction::SelectAcpAgent(id.clone()),
                                );
                            }
                        })
                })
            })
            .collect::<Vec<_>>();
        acp_agent_buttons.push(ctx.add_typed_action_view(|_| {
            ActionButton::new("Custom", NakedTheme)
                .with_full_width(true)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(ProviderSetupModalBodyAction::SelectAcpAgent(
                        "custom".to_owned(),
                    ));
                })
        }));

        let bedrock_auth_buttons = [
            BedrockAuthMethod::Profile,
            BedrockAuthMethod::Sso,
            BedrockAuthMethod::StaticKeys,
        ]
        .into_iter()
        .map(|method| {
            ctx.add_typed_action_view(move |_| {
                ActionButton::new(method.display_name(), NakedTheme).on_click(move |ctx| {
                    ctx.dispatch_typed_action(ProviderSetupModalBodyAction::SelectBedrockAuth(
                        method,
                    ));
                })
            })
        })
        .collect();

        ctx.subscribe_to_view(&name_editor, |me, editor, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                me.draft_name = editor.as_ref(ctx).buffer_text(ctx);
                me.update_next_button(ctx);
                ctx.notify();
            }
        });
        ctx.subscribe_to_view(&base_url_editor, |me, editor, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                me.draft_base_url = editor.as_ref(ctx).buffer_text(ctx);
                me.update_next_button(ctx);
                ctx.notify();
            }
        });
        ctx.subscribe_to_view(&api_key_editor, |me, editor, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                let value = editor.as_ref(ctx).buffer_text(ctx);
                me.draft_api_key = (!value.trim().is_empty()).then_some(value);
                ctx.notify();
            }
        });
        ctx.subscribe_to_view(&project_id_editor, |me, editor, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                me.draft_project_id = editor.as_ref(ctx).buffer_text(ctx);
                me.update_next_button(ctx);
                ctx.notify();
            }
        });
        ctx.subscribe_to_view(&location_editor, |me, editor, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                me.draft_location = editor.as_ref(ctx).buffer_text(ctx);
                ctx.notify();
            }
        });
        for (editor, update) in [
            (bedrock_profile_editor.clone(), 0),
            (bedrock_region_editor.clone(), 1),
            (bedrock_refresh_command_editor.clone(), 2),
            (bedrock_access_key_editor.clone(), 3),
            (bedrock_secret_key_editor.clone(), 4),
            (acp_agent_id_editor.clone(), 5),
            (acp_command_editor.clone(), 6),
            (acp_args_editor.clone(), 7),
        ] {
            ctx.subscribe_to_view(&editor, move |me, editor, event, ctx| {
                if matches!(event, EditorEvent::Edited(_)) {
                    let value = editor.as_ref(ctx).buffer_text(ctx);
                    match update {
                        0 => me.draft_bedrock.profile = value,
                        1 => me.draft_bedrock.region = value,
                        2 => me.draft_bedrock.auth_refresh_command = value,
                        3 => me.draft_bedrock.access_key_id = value,
                        4 => me.draft_bedrock.secret_access_key = value,
                        5 => me.draft_acp.agent_id = value,
                        6 => me.draft_acp.command = value,
                        7 => {
                            if let Ok(args) = serde_json::from_str::<Vec<String>>(&value) {
                                me.draft_acp.args = args;
                            }
                        }
                        _ => unreachable!(),
                    }
                    me.update_next_button(ctx);
                    ctx.notify();
                }
            });
        }

        #[cfg(not(target_family = "wasm"))]
        ctx.subscribe_to_model(&ChatGPTAuthModel::handle(ctx), |me, _, event, ctx| {
            if matches!(event, ChatGPTAuthModelEvent::StateChanged) {
                if me.step == ProviderSetupStep::Discover {
                    me.try_discover_chatgpt(ctx);
                }
                me.update_next_button(ctx);
                ctx.notify();
            }
        });

        let back_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Back", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(ProviderSetupModalBodyAction::Back);
            })
        });
        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(ProviderSetupModalBodyAction::Cancel);
            })
        });
        let next_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Next", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(ProviderSetupModalBodyAction::Next);
            })
        });

        Self {
            step: ProviderSetupStep::ProviderType,
            editing_index: None,
            provider_type: ProviderSetupProviderType::OpenAICompatible,
            draft_name: String::new(),
            draft_base_url: String::new(),
            draft_api_key: None,
            draft_project_id: String::new(),
            draft_location: "global".to_string(),
            draft_models: Vec::new(),
            draft_bedrock: BedrockProviderDraft {
                name: String::new(),
                auth_method: BedrockAuthMethod::default(),
                profile: "default".to_string(),
                region: String::new(),
                cross_region_inference: true,
                auto_login: true,
                auth_refresh_command: "aws sso login".to_string(),
                access_key_id: String::new(),
                secret_access_key: String::new(),
                models: Vec::new(),
            },
            draft_acp: AcpProviderDraft {
                name: String::new(),
                agent_id: "codex".to_string(),
                command: String::new(),
                args: Vec::new(),
                config_options: Vec::new(),
            },
            discovery_state: DiscoveryState::Idle,
            provider_type_buttons,
            acp_agent_buttons,
            name_editor,
            base_url_editor,
            api_key_editor,
            project_id_editor,
            location_editor,
            bedrock_profile_editor,
            bedrock_region_editor,
            bedrock_refresh_command_editor,
            bedrock_access_key_editor,
            bedrock_secret_key_editor,
            acp_agent_id_editor,
            acp_command_editor,
            acp_args_editor,
            chatgpt_connect_mouse_state: MouseStateHandle::default(),
            chatgpt_open_mouse_state: MouseStateHandle::default(),
            chatgpt_copy_mouse_state: MouseStateHandle::default(),
            bedrock_auth_buttons,
            bedrock_cross_region_toggle: SwitchStateHandle::default(),
            bedrock_auto_login_toggle: SwitchStateHandle::default(),
            model_switches: Vec::new(),
            model_capability_switches: Vec::new(),
            model_capability_buttons: Vec::new(),
            model_context_editors: Vec::new(),
            provider_type_scroll_state: ClippedScrollStateHandle::default(),
            models_scroll_state: ClippedScrollStateHandle::default(),
            back_button,
            cancel_button,
            next_button,
        }
    }

    fn create_editor(
        placeholder: &'static str,
        is_password: bool,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                is_password,
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
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor
        })
    }

    pub fn begin_create(&mut self, ctx: &mut ViewContext<Self>) {
        self.step = ProviderSetupStep::ProviderType;
        self.editing_index = None;
        self.provider_type = ProviderSetupProviderType::OpenAICompatible;
        self.draft_name.clear();
        self.draft_base_url.clear();
        self.draft_api_key = None;
        self.draft_project_id.clear();
        self.draft_location = "global".to_string();
        self.draft_models.clear();
        self.draft_bedrock = BedrockProviderDraft {
            name: String::new(),
            auth_method: BedrockAuthMethod::default(),
            profile: "default".to_string(),
            region: String::new(),
            cross_region_inference: true,
            auto_login: true,
            auth_refresh_command: "aws sso login".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            models: Vec::new(),
        };
        self.draft_acp = AcpProviderDraft {
            name: String::new(),
            agent_id: "codex".to_string(),
            command: String::new(),
            args: Vec::new(),
            config_options: Vec::new(),
        };
        self.discovery_state = DiscoveryState::Idle;
        self.sync_editors(ctx);
        self.sync_provider_type_buttons(ctx);
        self.sync_acp_agent_buttons(ctx);
        self.sync_bedrock_auth_buttons(ctx);
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    pub fn begin_edit(
        &mut self,
        editing_index: usize,
        provider: OpenAIProviderConfig,
        ctx: &mut ViewContext<Self>,
    ) {
        // Editing an existing provider is a local catalog operation. Do not
        // send the user through credentials or model discovery again.
        self.step = ProviderSetupStep::Models;
        self.editing_index = Some(editing_index);
        self.provider_type = match provider.kind {
            OpenAIProviderKind::ChatGPTSubscription => {
                ProviderSetupProviderType::ChatGPTSubscription
            }
            OpenAIProviderKind::OpenAICompatible => ProviderSetupProviderType::OpenAICompatible,
            OpenAIProviderKind::Anthropic => ProviderSetupProviderType::Anthropic,
            OpenAIProviderKind::Gemini => ProviderSetupProviderType::Gemini,
            OpenAIProviderKind::VertexAI => ProviderSetupProviderType::VertexAI,
        };
        self.draft_name = provider.name;
        self.draft_base_url = provider.base_url;
        self.draft_api_key = provider.api_key;
        self.draft_project_id = provider.project_id.unwrap_or_default();
        self.draft_location = provider.location.unwrap_or_else(|| "global".to_string());
        self.draft_models = provider.models;
        self.discovery_state = DiscoveryState::Idle;
        self.sync_editors(ctx);
        self.sync_provider_type_buttons(ctx);
        self.sync_acp_agent_buttons(ctx);
        self.sync_bedrock_auth_buttons(ctx);
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    pub fn begin_edit_bedrock(&mut self, draft: BedrockProviderDraft, ctx: &mut ViewContext<Self>) {
        self.step = ProviderSetupStep::Configure;
        self.editing_index = None;
        self.provider_type = ProviderSetupProviderType::Bedrock;
        self.draft_name = draft.name.clone();
        self.draft_bedrock = draft;
        self.draft_models.clear();
        self.discovery_state = DiscoveryState::Idle;
        self.sync_editors(ctx);
        self.sync_provider_type_buttons(ctx);
        self.sync_bedrock_auth_buttons(ctx);
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    pub fn begin_edit_acp(&mut self, draft: AcpProviderDraft, ctx: &mut ViewContext<Self>) {
        self.step = if draft.config_options.is_empty() {
            ProviderSetupStep::Configure
        } else {
            ProviderSetupStep::Models
        };
        self.editing_index = None;
        self.provider_type = ProviderSetupProviderType::Acp;
        self.draft_name = draft.name.clone();
        self.draft_acp = draft;
        self.draft_models.clear();
        self.discovery_state = DiscoveryState::Idle;
        self.sync_editors(ctx);
        self.sync_provider_type_buttons(ctx);
        self.sync_bedrock_auth_buttons(ctx);
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    pub fn finish_acp_discovery(
        &mut self,
        result: Result<(), String>,
        config_options: Vec<AcpConfigOptionSettings>,
        ctx: &mut ViewContext<Self>,
    ) {
        match result {
            Ok(()) => {
                self.draft_acp.config_options = config_options;
                self.discovery_state = DiscoveryState::Idle;
                self.step = ProviderSetupStep::Models;
                ctx.focus(&self.name_editor);
            }
            Err(error) => {
                self.discovery_state = DiscoveryState::Failed(error);
            }
        }
        self.update_next_button(ctx);
        ctx.notify();
    }

    fn sync_editors(&self, ctx: &mut ViewContext<Self>) {
        self.name_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_name, ctx);
        });
        self.base_url_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_base_url, ctx);
        });
        self.api_key_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(self.draft_api_key.as_deref().unwrap_or_default(), ctx);
        });
        self.project_id_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_project_id, ctx);
        });
        self.location_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_location, ctx);
        });
        self.bedrock_profile_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_bedrock.profile, ctx);
        });
        self.bedrock_region_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_bedrock.region, ctx);
        });
        self.bedrock_refresh_command_editor
            .update(ctx, |editor, ctx| {
                editor.system_reset_buffer_text(&self.draft_bedrock.auth_refresh_command, ctx);
            });
        self.bedrock_access_key_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_bedrock.access_key_id, ctx);
        });
        self.bedrock_secret_key_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_bedrock.secret_access_key, ctx);
        });
        self.acp_agent_id_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_acp.agent_id, ctx);
        });
        self.acp_command_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_acp.command, ctx);
        });
        self.acp_args_editor.update(ctx, |editor, ctx| {
            let args =
                serde_json::to_string(&self.draft_acp.args).unwrap_or_else(|_| "[]".to_string());
            editor.system_reset_buffer_text(&args, ctx);
        });
    }

    fn sync_provider_type_buttons(&self, ctx: &mut ViewContext<Self>) {
        for ((button_kind, _, _), button) in PROVIDER_TYPE_OPTIONS
            .iter()
            .zip(self.provider_type_buttons.iter())
        {
            button.update(ctx, |button, ctx| {
                button.set_active(*button_kind == self.provider_type, ctx);
            });
        }
    }

    fn sync_acp_agent_buttons(&self, ctx: &mut ViewContext<Self>) {
        let selected = self.draft_acp.agent_id.trim();
        for (agent, button) in galaxy_acp::known_acp_agents()
            .iter()
            .zip(self.acp_agent_buttons.iter())
        {
            button.update(ctx, |button, ctx| {
                button.set_active(agent.id.eq_ignore_ascii_case(selected), ctx);
            });
        }
        if let Some(button) = self.acp_agent_buttons.last() {
            button.update(ctx, |button, ctx| {
                button.set_active(selected.eq_ignore_ascii_case("custom"), ctx);
            });
        }
    }

    fn sync_bedrock_auth_buttons(&self, ctx: &mut ViewContext<Self>) {
        for (index, button) in self.bedrock_auth_buttons.iter().enumerate() {
            let method = match index {
                0 => BedrockAuthMethod::Profile,
                1 => BedrockAuthMethod::Sso,
                2 => BedrockAuthMethod::StaticKeys,
                _ => continue,
            };
            button.update(ctx, |button, ctx| {
                button.set_active(method == self.draft_bedrock.auth_method, ctx);
            });
        }
    }

    fn sync_model_switches(&mut self, ctx: &mut ViewContext<Self>) {
        while self.model_switches.len() < self.draft_models.len() {
            self.model_switches.push(SwitchStateHandle::default());
        }
        self.model_switches.truncate(self.draft_models.len());

        while self.model_capability_switches.len() < self.draft_models.len() {
            self.model_capability_switches
                .push([SwitchStateHandle::default(), SwitchStateHandle::default()]);
        }
        self.model_capability_switches
            .truncate(self.draft_models.len());

        while self.model_capability_buttons.len() < self.draft_models.len() {
            let index = self.model_capability_buttons.len();
            let buttons = CapabilityKey::ALL
                .into_iter()
                .map(|key| {
                    ctx.add_typed_action_view(move |_| {
                        ActionButton::new(format!("{}: Auto", key.label()), NakedTheme)
                            .with_size(ButtonSize::XSmall)
                            .on_click(move |ctx| {
                                ctx.dispatch_typed_action(
                                    ProviderSetupModalBodyAction::CycleModelCapability(index, key),
                                );
                            })
                    })
                })
                .collect();
            self.model_capability_buttons.push(buttons);
        }
        self.model_capability_buttons
            .truncate(self.draft_models.len());

        while self.model_context_editors.len() < self.draft_models.len() {
            let index = self.model_context_editors.len();
            let editor = Self::create_editor("Context window", false, ctx);
            ctx.subscribe_to_view(&editor, move |me, editor, event, ctx| {
                if matches!(event, EditorEvent::Edited(_)) {
                    if let Some(model) = me.draft_models.get_mut(index) {
                        if let Ok(context_size) = editor.as_ref(ctx).buffer_text(ctx).parse() {
                            model.context_size = context_size;
                            model.max_input_tokens = Some(context_size);
                        }
                    }
                    me.update_next_button(ctx);
                    ctx.notify();
                }
            });
            self.model_context_editors.push(editor);
        }
        self.model_context_editors.truncate(self.draft_models.len());

        for (index, model) in self.draft_models.iter().enumerate() {
            for (button, key) in self.model_capability_buttons[index]
                .iter()
                .zip(CapabilityKey::ALL)
            {
                let state = model.capability_override(key.setting_key());
                button.update(ctx, |button, ctx| {
                    button.set_label(format!("{}: {}", key.label(), state.label()), ctx);
                });
            }
            self.model_context_editors[index].update(ctx, |editor, ctx| {
                editor.system_reset_buffer_text(&model.context_size.to_string(), ctx);
            });
        }
    }

    fn update_next_button(&self, ctx: &mut ViewContext<Self>) {
        let (label, disabled) = match self.step {
            ProviderSetupStep::ProviderType => ("Next", false),
            ProviderSetupStep::Configure => {
                let disabled = match self.provider_type {
                    ProviderSetupProviderType::OpenAICompatible => {
                        self.draft_base_url.trim().is_empty()
                    }
                    ProviderSetupProviderType::Anthropic | ProviderSetupProviderType::Gemini => {
                        self.draft_api_key
                            .as_deref()
                            .is_none_or(|key| key.trim().is_empty())
                    }
                    ProviderSetupProviderType::VertexAI => self.draft_project_id.trim().is_empty(),
                    ProviderSetupProviderType::Acp => {
                        self.draft_acp.agent_id.trim().is_empty()
                            || (self.draft_acp.agent_id.eq_ignore_ascii_case("custom")
                                && self.draft_acp.command.trim().is_empty())
                    }
                    ProviderSetupProviderType::ChatGPTSubscription
                    | ProviderSetupProviderType::Bedrock => false,
                };
                ("Next", disabled)
            }
            ProviderSetupStep::Discover => (
                if matches!(self.discovery_state, DiscoveryState::Failed(_)) {
                    "Retry"
                } else {
                    "Testing..."
                },
                !matches!(self.discovery_state, DiscoveryState::Failed(_)),
            ),
            ProviderSetupStep::Models => match self.provider_type {
                ProviderSetupProviderType::OpenAICompatible
                | ProviderSetupProviderType::ChatGPTSubscription
                | ProviderSetupProviderType::Anthropic
                | ProviderSetupProviderType::Gemini
                | ProviderSetupProviderType::VertexAI => (
                    "Save",
                    self.draft_name.trim().is_empty()
                        || !self.draft_models.iter().any(|model| model.enabled),
                ),
                ProviderSetupProviderType::Bedrock => (
                    "Save",
                    self.draft_name.trim().is_empty() || self.draft_bedrock.models.is_empty(),
                ),
                ProviderSetupProviderType::Acp => ("Save", self.draft_name.trim().is_empty()),
            },
        };
        self.next_button.update(ctx, |button, ctx| {
            button.set_label(label, ctx);
            button.set_disabled(disabled, ctx);
        });
        self.back_button.update(ctx, |button, ctx| {
            button.set_disabled(self.step == ProviderSetupStep::ProviderType, ctx);
        });
    }

    fn draft_provider(&self) -> OpenAIProviderConfig {
        OpenAIProviderConfig {
            kind: match self.provider_type {
                ProviderSetupProviderType::ChatGPTSubscription => {
                    OpenAIProviderKind::ChatGPTSubscription
                }
                ProviderSetupProviderType::OpenAICompatible
                | ProviderSetupProviderType::Bedrock
                | ProviderSetupProviderType::Acp => OpenAIProviderKind::OpenAICompatible,
                ProviderSetupProviderType::Anthropic => OpenAIProviderKind::Anthropic,
                ProviderSetupProviderType::Gemini => OpenAIProviderKind::Gemini,
                ProviderSetupProviderType::VertexAI => OpenAIProviderKind::VertexAI,
            },
            enabled: true,
            name: self.draft_name.trim().to_string(),
            base_url: if matches!(
                self.provider_type,
                ProviderSetupProviderType::ChatGPTSubscription
                    | ProviderSetupProviderType::Anthropic
                    | ProviderSetupProviderType::Gemini
                    | ProviderSetupProviderType::VertexAI
            ) {
                String::new()
            } else {
                self.draft_base_url.trim().trim_end_matches('/').to_string()
            },
            api_key: matches!(
                self.provider_type,
                ProviderSetupProviderType::OpenAICompatible
                    | ProviderSetupProviderType::Anthropic
                    | ProviderSetupProviderType::Gemini
            )
            .then(|| {
                self.draft_api_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty())
                    .map(str::to_string)
            })
            .flatten(),
            project_id: matches!(self.provider_type, ProviderSetupProviderType::VertexAI)
                .then(|| self.draft_project_id.trim().to_string()),
            location: matches!(self.provider_type, ProviderSetupProviderType::VertexAI).then(
                || {
                    let location = self.draft_location.trim();
                    if location.is_empty() {
                        "global".to_string()
                    } else {
                        location.to_string()
                    }
                },
            ),
            models: self.draft_models.clone(),
        }
    }

    fn begin_discovery(&mut self, ctx: &mut ViewContext<Self>) {
        self.step = ProviderSetupStep::Discover;
        self.discovery_state = DiscoveryState::Loading;
        self.update_next_button(ctx);
        ctx.notify();

        match self.provider_type {
            ProviderSetupProviderType::ChatGPTSubscription => {
                self.try_discover_chatgpt(ctx);
                return;
            }
            ProviderSetupProviderType::Bedrock => {
                let config = crate::ai::bedrock::client::BedrockClientConfig {
                    auth_method: self.draft_bedrock.auth_method,
                    profile: self.draft_bedrock.profile.clone(),
                    region: self.draft_bedrock.region.clone(),
                    access_key_id: self.draft_bedrock.access_key_id.clone(),
                    secret_access_key: self.draft_bedrock.secret_access_key.clone(),
                    session_token: None,
                    cross_region_inference: self.draft_bedrock.cross_region_inference,
                    use_rig: false,
                };
                ctx.spawn(
                    async move {
                        crate::ai::bedrock::discovery::discover_available_models(config).await
                    },
                    move |me, result, ctx| match result {
                        Ok(models) => {
                            me.draft_bedrock.models = models;
                            me.discovery_state = DiscoveryState::Idle;
                            me.step = ProviderSetupStep::Models;
                            me.update_next_button(ctx);
                            ctx.focus(&me.name_editor);
                            ctx.notify();
                        }
                        Err(error) => {
                            me.discovery_state = DiscoveryState::Failed(error);
                            me.update_next_button(ctx);
                            ctx.notify();
                        }
                    },
                );
                return;
            }
            ProviderSetupProviderType::Acp => {
                ctx.emit(ProviderSetupModalBodyEvent::RequestAcpDiscovery(
                    self.draft_acp.clone(),
                ));
                return;
            }
            ProviderSetupProviderType::OpenAICompatible
            | ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI => {}
        }

        let provider = self.draft_provider();
        let existing_models = self.draft_models.clone();
        ctx.spawn(
            async move { LLMPreferences::discover_openai_provider_models(provider).await },
            move |me, result, ctx| match result {
                Ok(models) => {
                    me.draft_models = merge_discovered_provider_models(&existing_models, models);
                    me.discovery_state = DiscoveryState::Idle;
                    me.step = ProviderSetupStep::Models;
                    me.sync_model_switches(ctx);
                    me.update_next_button(ctx);
                    ctx.focus(&me.name_editor);
                    ctx.notify();
                }
                Err(error) => {
                    me.discovery_state = DiscoveryState::Failed(error);
                    me.update_next_button(ctx);
                    ctx.notify();
                }
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn try_discover_chatgpt(&mut self, ctx: &mut ViewContext<Self>) {
        if !matches!(
            ChatGPTAuthModel::as_ref(ctx).state(),
            ChatGPTAuthState::Connected
        ) {
            self.discovery_state = DiscoveryState::Failed(
                "Connect your ChatGPT subscription before continuing.".to_string(),
            );
            self.update_next_button(ctx);
            ctx.notify();
            return;
        }

        if self.draft_models.is_empty() {
            self.draft_models = crate::settings::ai::default_chatgpt_provider().models;
        }
        self.discovery_state = DiscoveryState::Idle;
        self.step = ProviderSetupStep::Models;
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus(&self.name_editor);
        ctx.notify();
    }

    #[cfg(target_family = "wasm")]
    fn try_discover_chatgpt(&mut self, ctx: &mut ViewContext<Self>) {
        self.discovery_state = DiscoveryState::Failed(
            "ChatGPT subscription providers are available in the desktop app.".to_string(),
        );
        self.update_next_button(ctx);
        ctx.notify();
    }

    fn render_label(appearance: &Appearance, label: &str) -> Box<dyn galaxyui::Element> {
        Text::new_inline(
            label.to_string(),
            appearance.ui_font_family(),
            INPUT_FONT_SIZE,
        )
        .with_color(appearance.theme().active_ui_text_color().into())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish()
    }

    fn render_input(
        &self,
        appearance: &Appearance,
        label: &str,
        editor: &ViewHandle<EditorView>,
    ) -> Box<dyn galaxyui::Element> {
        let input = appearance
            .ui_builder()
            .text_input(editor.clone())
            .with_style(UiComponentStyles {
                padding: Some(Coords {
                    top: 10.,
                    bottom: 10.,
                    left: 12.,
                    right: 12.,
                }),
                background: Some(appearance.theme().surface_1().into()),
                ..Default::default()
            })
            .build()
            .finish();
        Flex::column()
            .with_spacing(6.)
            .with_child(Self::render_label(appearance, label))
            .with_child(input)
            .finish()
    }

    fn render_provider_type(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let description = Text::new(
            "Choose how Galaxy should connect to this provider.",
            appearance.ui_font_family(),
            INPUT_FONT_SIZE,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish();

        let cards = PROVIDER_TYPE_OPTIONS
            .iter()
            .enumerate()
            .map(|(index, (_, _, description))| {
                let button = ChildView::new(&self.provider_type_buttons[index]).finish();
                Container::new(
                    Flex::column()
                        .with_spacing(8.)
                        .with_child(button)
                        .with_child(
                            Text::new(*description, appearance.ui_font_family(), INPUT_FONT_SIZE)
                                .with_color(appearance.theme().nonactive_ui_text_color().into())
                                .soft_wrap(true)
                                .finish(),
                        )
                        .finish(),
                )
                .with_padding(Padding::uniform(12.))
                .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .finish()
            })
            .collect::<Vec<_>>();

        let list = Flex::column()
            .with_spacing(10.)
            .with_children(cards)
            .finish();
        let scrollable = ClippedScrollable::vertical(
            self.provider_type_scroll_state.clone(),
            list,
            ScrollbarWidth::Auto,
            appearance.theme().nonactive_ui_detail().into(),
            appearance.theme().active_ui_detail().into(),
            appearance.theme().surface_1().into(),
        )
        .with_overlayed_scrollbar()
        .finish();

        Flex::column()
            .with_spacing(16.)
            .with_child(description)
            .with_child(
                ConstrainedBox::new(scrollable)
                    .with_max_height(360.)
                    .finish(),
            )
            .finish()
    }

    #[cfg(not(target_family = "wasm"))]
    fn render_chatgpt_auth(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let state = ChatGPTAuthModel::as_ref(app).state();
        let mut children = vec![Self::render_label(appearance, "ChatGPT authorization")];
        let description = match &state {
            ChatGPTAuthState::NotConnected => "Connect your ChatGPT subscription to continue.",
            ChatGPTAuthState::Connecting => "Waiting for ChatGPT authorization to start...",
            ChatGPTAuthState::AwaitingDeviceCode { .. } => {
                "Enter the device code in the ChatGPT sign-in page."
            }
            ChatGPTAuthState::Connected => "ChatGPT subscription connected.",
            ChatGPTAuthState::Failed(_) => "ChatGPT connection failed.",
        };
        children.push(
            Text::new(description, appearance.ui_font_family(), INPUT_FONT_SIZE)
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
        );
        if let ChatGPTAuthState::Failed(error) = &state {
            children.push(
                Text::new(error.clone(), appearance.monospace_font_family(), 11.)
                    .with_color(appearance.theme().ui_error_color())
                    .soft_wrap(true)
                    .finish(),
            );
        }

        if let ChatGPTAuthState::AwaitingDeviceCode {
            verification_uri,
            user_code,
        } = &state
        {
            children.push(
                Container::new(
                    FormattedTextElement::from_str(
                        user_code.clone(),
                        appearance.monospace_font_family(),
                        24.,
                    )
                    .with_weight(Weight::Bold)
                    .with_color(appearance.theme().active_ui_text_color().into())
                    .finish(),
                )
                .with_padding(Padding::uniform(12.))
                .with_background(appearance.theme().surface_1())
                .with_border(Border::all(1.).with_border_fill(appearance.theme().accent()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .finish(),
            );
            let buttons = Flex::row()
                .with_spacing(8.)
                .with_child(
                    appearance
                        .ui_builder()
                        .button(
                            ButtonVariant::Secondary,
                            self.chatgpt_open_mouse_state.clone(),
                        )
                        .with_text_label("Open sign-in page".to_owned())
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(
                                ProviderSetupModalBodyAction::OpenChatGPTDevicePage,
                            );
                        })
                        .finish(),
                )
                .with_child(
                    appearance
                        .ui_builder()
                        .button(
                            ButtonVariant::Secondary,
                            self.chatgpt_copy_mouse_state.clone(),
                        )
                        .with_text_label("Copy code".to_owned())
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(
                                ProviderSetupModalBodyAction::CopyChatGPTDeviceCode,
                            );
                        })
                        .finish(),
                )
                .finish();
            children.push(buttons);
            children.push(
                Text::new(
                    verification_uri.clone(),
                    appearance.ui_font_family(),
                    INPUT_FONT_SIZE,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
            );
        }

        if !matches!(
            state,
            ChatGPTAuthState::Connected | ChatGPTAuthState::Connecting
        ) {
            children.push(
                appearance
                    .ui_builder()
                    .button(
                        ButtonVariant::Secondary,
                        self.chatgpt_connect_mouse_state.clone(),
                    )
                    .with_text_label("Connect ChatGPT".to_owned())
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(ProviderSetupModalBodyAction::ConnectChatGPT);
                    })
                    .finish(),
            );
        }

        Flex::column()
            .with_spacing(10.)
            .with_children(children)
            .finish()
    }

    #[cfg(target_family = "wasm")]
    fn render_chatgpt_auth(
        &self,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn galaxyui::Element> {
        Flex::column()
            .with_spacing(8.)
            .with_child(Self::render_label(appearance, "ChatGPT authorization"))
            .with_child(
                Text::new(
                    "ChatGPT subscription providers are available in the desktop app.",
                    appearance.ui_font_family(),
                    INPUT_FONT_SIZE,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
            )
            .finish()
    }

    fn render_configure(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn galaxyui::Element> {
        let mut children = vec![Text::new(
            format!("Configure {}", provider_type_label(self.provider_type)),
            appearance.ui_font_family(),
            appearance.header_font_size(),
        )
        .with_color(appearance.theme().active_ui_text_color().into())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish()];

        match self.provider_type {
            ProviderSetupProviderType::ChatGPTSubscription => {
                children.push(self.render_chatgpt_auth(appearance, app));
            }
            ProviderSetupProviderType::OpenAICompatible => {
                children.push(self.render_input(appearance, "Base URL", &self.base_url_editor));
                children.push(self.render_input(appearance, "API key", &self.api_key_editor));
                children.push(
                    Text::new(
                        "The API key is stored locally and is never synced to the cloud.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::Anthropic => {
                children.push(self.render_input(appearance, "API key", &self.api_key_editor));
                children.push(
                    Text::new(
                        "The key is stored locally and is never synced to the cloud. Models will be discovered from Anthropic after the connection test.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::Gemini => {
                children.push(self.render_input(appearance, "API key", &self.api_key_editor));
                children.push(
                    Text::new(
                        "The key is stored locally and is never synced to the cloud. Models will be discovered from Google's Gemini API after the connection test.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::VertexAI => {
                children.push(self.render_input(
                    appearance,
                    "Google Cloud project ID",
                    &self.project_id_editor,
                ));
                children.push(self.render_input(appearance, "Location", &self.location_editor));
                children.push(
                    Text::new(
                        "Vertex AI uses Google Application Default Credentials. Run `gcloud auth application-default login` before testing the connection.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::Bedrock => {
                children.push(Self::render_label(appearance, "Authentication method"));
                children.push(
                    Flex::row()
                        .with_spacing(8.)
                        .with_children(
                            self.bedrock_auth_buttons
                                .iter()
                                .map(|button| ChildView::new(button).finish()),
                        )
                        .finish(),
                );
                match self.draft_bedrock.auth_method {
                    BedrockAuthMethod::Profile | BedrockAuthMethod::Sso => {
                        children.push(self.render_input(
                            appearance,
                            "AWS profile",
                            &self.bedrock_profile_editor,
                        ));
                        if self.draft_bedrock.auth_method == BedrockAuthMethod::Sso {
                            children.push(self.render_input(
                                appearance,
                                "Login command",
                                &self.bedrock_refresh_command_editor,
                            ));
                            children.push(
                                appearance
                                    .ui_builder()
                                    .switch(self.bedrock_auto_login_toggle.clone())
                                    .check(self.draft_bedrock.auto_login)
                                    .build()
                                    .on_click(|ctx, _, _| {
                                        ctx.dispatch_typed_action(
                                            ProviderSetupModalBodyAction::ToggleBedrockAutoLogin,
                                        );
                                    })
                                    .finish(),
                            );
                        }
                    }
                    BedrockAuthMethod::StaticKeys => {
                        children.push(self.render_input(
                            appearance,
                            "Access key ID",
                            &self.bedrock_access_key_editor,
                        ));
                        children.push(self.render_input(
                            appearance,
                            "Secret access key",
                            &self.bedrock_secret_key_editor,
                        ));
                    }
                }
                children.push(self.render_input(
                    appearance,
                    "AWS region",
                    &self.bedrock_region_editor,
                ));
                children.push(
                    appearance
                        .ui_builder()
                        .switch(self.bedrock_cross_region_toggle.clone())
                        .check(self.draft_bedrock.cross_region_inference)
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(
                                ProviderSetupModalBodyAction::ToggleBedrockCrossRegion,
                            );
                        })
                        .finish(),
                );
            }
            ProviderSetupProviderType::Acp => {
                children.push(
                    Text::new("ACP client", appearance.ui_font_family(), INPUT_FONT_SIZE)
                        .with_color(appearance.theme().active_ui_text_color().into())
                        .with_style(Properties::default().weight(Weight::Bold))
                        .finish(),
                );
                children.extend(
                    self.acp_agent_buttons
                        .iter()
                        .map(|button| ChildView::new(button).finish()),
                );
                if self.draft_acp.agent_id.eq_ignore_ascii_case("custom") {
                    children.push(self.render_input(
                        appearance,
                        "Executable",
                        &self.acp_command_editor,
                    ));
                    children.push(self.render_input(
                        appearance,
                        "Arguments (JSON array)",
                        &self.acp_args_editor,
                    ));
                }
                children.push(
                    Text::new(
                        "Known clients use their local executable. If the client is not installed, Galaxy will show a launch error. Choose Custom for another ACP-compatible command.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
        }

        Flex::column()
            .with_spacing(16.)
            .with_children(children)
            .finish()
    }

    fn render_discovery(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let (message, color) = match &self.discovery_state {
            DiscoveryState::Idle | DiscoveryState::Loading => (
                "Testing the connection and discovering available models...".to_string(),
                appearance.theme().active_ui_text_color().into_solid(),
            ),
            DiscoveryState::Failed(error) => (error.clone(), appearance.theme().ui_error_color()),
        };
        Flex::column()
            .with_spacing(12.)
            .with_child(
                Text::new(
                    "Test connection",
                    appearance.ui_font_family(),
                    appearance.header_font_size(),
                )
                .with_color(appearance.theme().active_ui_text_color().into())
                .with_style(Properties::default().weight(Weight::Bold))
                .finish(),
            )
            .with_child(if matches!(self.discovery_state, DiscoveryState::Loading) {
                Flex::row()
                    .with_spacing(8.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        ConstrainedBox::new(
                            Icon::Loading
                                .to_galaxyui_icon(appearance.theme().active_ui_text_color())
                                .finish(),
                        )
                        .with_width(16.)
                        .with_height(16.)
                        .finish(),
                    )
                    .with_child(
                        Text::new(message, appearance.ui_font_family(), INPUT_FONT_SIZE)
                            .with_color(color)
                            .soft_wrap(true)
                            .finish(),
                    )
                    .finish()
            } else {
                Text::new(message, appearance.ui_font_family(), INPUT_FONT_SIZE)
                    .with_color(color)
                    .soft_wrap(true)
                    .finish()
            })
            .finish()
    }

    fn render_model_table(
        &self,
        appearance: &Appearance,
        rows: Vec<Box<dyn galaxyui::Element>>,
        spacing: f32,
    ) -> Box<dyn galaxyui::Element> {
        let list = Flex::column()
            .with_spacing(spacing)
            .with_children(rows)
            .finish();
        let scrollable = ClippedScrollable::vertical(
            self.models_scroll_state.clone(),
            list,
            ScrollbarWidth::Auto,
            appearance.theme().nonactive_ui_detail().into(),
            appearance.theme().active_ui_detail().into(),
            appearance.theme().surface_1().into(),
        )
        .with_overlayed_scrollbar()
        .finish();

        ConstrainedBox::new(
            Container::new(scrollable)
                .with_padding(Padding::uniform(12.))
                .with_background(appearance.theme().surface_1())
                .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .finish(),
        )
        .with_width(MODAL_WIDTH - 56.)
        .with_max_height(430.)
        .finish()
    }

    fn model_logo(&self) -> (Icon, ColorU) {
        match self.provider_type {
            ProviderSetupProviderType::ChatGPTSubscription
            | ProviderSetupProviderType::OpenAICompatible => {
                (Icon::OpenAILogo, crate::terminal::cli_agent::OPENAI_COLOR)
            }
            ProviderSetupProviderType::Anthropic => {
                (Icon::ClaudeLogo, crate::ai::blocklist::CLAUDE_ORANGE)
            }
            ProviderSetupProviderType::Gemini | ProviderSetupProviderType::VertexAI => {
                (Icon::GeminiLogo, crate::terminal::cli_agent::GEMINI_BLUE)
            }
            ProviderSetupProviderType::Bedrock => {
                (Icon::BedrockLogo, ColorU::new(255, 153, 0, 255))
            }
            ProviderSetupProviderType::Acp => {
                let agent = self.draft_acp.agent_id.to_ascii_lowercase();
                let harness = if agent.contains("claude") {
                    Harness::Claude
                } else if agent.contains("gemini") {
                    Harness::Gemini
                } else if agent.contains("codex") {
                    Harness::Codex
                } else {
                    Harness::Unknown
                };
                (
                    harness_display::icon_for(harness),
                    harness_display::brand_color(harness)
                        .unwrap_or(ColorU::new(128, 128, 128, 255)),
                )
            }
        }
    }

    fn render_model_logo(&self) -> Box<dyn galaxyui::Element> {
        let (icon, color) = self.model_logo();
        ConstrainedBox::new(icon.to_galaxyui_icon(Fill::Solid(color)).finish())
            .with_width(MODEL_LOGO_SIZE)
            .with_height(MODEL_LOGO_SIZE)
            .finish()
    }

    fn render_model_table_header(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let header = |label: &str| {
            Text::new(label.to_owned(), appearance.ui_font_family(), 11.)
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish()
        };
        Container::new(
            Flex::row()
                .with_spacing(12.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(ConstrainedBox::new(header("Use")).with_width(52.).finish())
                .with_child(
                    ConstrainedBox::new(header("Model"))
                        .with_width(250.)
                        .finish(),
                )
                .with_child(
                    ConstrainedBox::new(header("Context"))
                        .with_width(140.)
                        .finish(),
                )
                .with_child(
                    ConstrainedBox::new(header("Capabilities"))
                        .with_width(330.)
                        .finish(),
                )
                .finish(),
        )
        .with_padding(Padding::uniform(10.).with_vertical(9.))
        .with_background(appearance.theme().surface_2())
        .with_border(Border::bottom(1.).with_border_fill(appearance.theme().outline()))
        .finish()
    }

    fn render_model_capabilities(
        &self,
        appearance: &Appearance,
        index: usize,
    ) -> Box<dyn galaxyui::Element> {
        let buttons = &self.model_capability_buttons[index];
        let first_row = buttons[..3]
            .iter()
            .map(|button| ChildView::new(button).finish())
            .collect::<Vec<_>>();
        let second_row = buttons[3..]
            .iter()
            .map(|button| ChildView::new(button).finish())
            .collect::<Vec<_>>();
        Flex::column()
            .with_spacing(6.)
            .with_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_children(first_row)
                    .finish(),
            )
            .with_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_children(second_row)
                    .finish(),
            )
            .finish()
    }

    fn render_openai_model_row(
        &self,
        appearance: &Appearance,
        index: usize,
        model: &OpenAIModelConfig,
    ) -> Box<dyn galaxyui::Element> {
        let model_info = Flex::column()
            .with_spacing(4.)
            .with_child(
                Text::new_inline(model.display_name.clone(), appearance.ui_font_family(), 12.)
                    .with_color(appearance.theme().active_ui_text_color().into())
                    .with_clip(ClipConfig::end())
                    .finish(),
            )
            .with_child(
                Text::new_inline(
                    model.model_id.clone(),
                    appearance.monospace_font_family(),
                    10.,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .with_clip(ClipConfig::end())
                .finish(),
            )
            .finish();
        let model_info = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(self.render_model_logo())
            .with_child(model_info)
            .finish();

        let context_input = appearance
            .ui_builder()
            .text_input(self.model_context_editors[index].clone())
            .with_style(UiComponentStyles {
                padding: Some(Coords {
                    top: 8.,
                    bottom: 8.,
                    left: 8.,
                    right: 8.,
                }),
                background: Some(appearance.theme().surface_1().into()),
                ..Default::default()
            })
            .build()
            .finish();

        Container::new(
            Flex::row()
                .with_spacing(12.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        appearance
                            .ui_builder()
                            .switch(self.model_switches[index].clone())
                            .check(model.enabled)
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(
                                    ProviderSetupModalBodyAction::ToggleModel(index),
                                );
                            })
                            .finish(),
                    )
                    .with_width(52.)
                    .finish(),
                )
                .with_child(ConstrainedBox::new(model_info).with_width(250.).finish())
                .with_child(ConstrainedBox::new(context_input).with_width(140.).finish())
                .with_child(
                    ConstrainedBox::new(self.render_model_capabilities(appearance, index))
                        .with_width(330.)
                        .finish(),
                )
                .finish(),
        )
        .with_padding(Padding::uniform(12.).with_vertical(10.))
        .with_border(Border::bottom(1.).with_border_fill(appearance.theme().outline()))
        .finish()
    }

    fn render_models(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        if self.provider_type == ProviderSetupProviderType::Bedrock {
            let rows = self
                .draft_bedrock
                .models
                .iter()
                .map(|model| {
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(10.)
                        .with_child(self.render_model_logo())
                        .with_child(
                            Flex::column()
                                .with_spacing(2.)
                                .with_child(
                                    Text::new(
                                        model.display_name.clone(),
                                        appearance.ui_font_family(),
                                        INPUT_FONT_SIZE,
                                    )
                                    .with_color(appearance.theme().active_ui_text_color().into())
                                    .finish(),
                                )
                                .with_child(
                                    Text::new(
                                        model.model_id.clone(),
                                        appearance.monospace_font_family(),
                                        10.,
                                    )
                                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                                    .soft_wrap(true)
                                    .finish(),
                                )
                                .finish(),
                        )
                        .finish()
                })
                .collect::<Vec<_>>();
            return Flex::column()
                .with_spacing(12.)
                .with_child(self.render_input(appearance, "Connection name", &self.name_editor))
                .with_child(
                    Text::new(
                        "These models passed AWS Bedrock availability checks. Model selection is managed by discovery and cannot be edited manually.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                )
                .with_child(self.render_model_table(appearance, rows, 10.))
                .finish();
        }
        if self.provider_type == ProviderSetupProviderType::Acp {
            let option_rows = self
                .draft_acp
                .config_options
                .iter()
                .filter(|option| {
                    matches!(
                        option.category.as_deref(),
                        Some("model") | Some("thought_level") | Some("mode")
                    )
                })
                .map(|option| {
                    let values = option
                        .options
                        .iter()
                        .map(|value| value.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(10.)
                        .with_child(self.render_model_logo())
                        .with_child(
                            Flex::column()
                                .with_spacing(2.)
                                .with_child(
                                    Text::new(
                                        option.name.clone(),
                                        appearance.ui_font_family(),
                                        INPUT_FONT_SIZE,
                                    )
                                    .with_color(appearance.theme().active_ui_text_color().into())
                                    .with_style(Properties::default().weight(Weight::Bold))
                                    .finish(),
                                )
                                .with_child(
                                    Text::new(
                                        if values.is_empty() {
                                            option.current_value.to_string()
                                        } else {
                                            values
                                        },
                                        appearance.monospace_font_family(),
                                        10.,
                                    )
                                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                                    .soft_wrap(true)
                                    .finish(),
                                )
                                .finish(),
                        )
                        .finish()
                })
                .collect::<Vec<_>>();
            let catalog = if option_rows.is_empty() {
                Text::new(
                    "No model or mode catalog has been discovered yet. Continue to test the ACP agent.",
                    appearance.ui_font_family(),
                    INPUT_FONT_SIZE,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish()
            } else {
                self.render_model_table(appearance, option_rows, 10.)
            };
            return Flex::column()
                .with_spacing(12.)
                .with_child(self.render_input(appearance, "Connection name", &self.name_editor))
                .with_child(
                    Text::new(
                        "ACP-discovered models and modes are exposed as selectable combinations in Galaxy's model picker.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                )
                .with_child(catalog)
                .finish();
        }
        let mut rows = vec![self.render_model_table_header(appearance)];
        rows.extend(
            self.draft_models
                .iter()
                .enumerate()
                .map(|(index, model)| self.render_openai_model_row(appearance, index, model)),
        );

        let table = self.render_model_table(appearance, rows, 0.);

        Flex::column()
            .with_spacing(12.)
            .with_child(self.render_input(appearance, "Connection name", &self.name_editor))
            .with_child(
                Text::new(
                    "Enable the models Galaxy should offer. Context is the maximum input window. Capabilities use Auto by default and can be overridden per model.",
                    appearance.ui_font_family(),
                    INPUT_FONT_SIZE,
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .soft_wrap(true)
                .finish(),
            )
            .with_child(table)
            .finish()
    }

    fn render_footer(&self) -> Box<dyn galaxyui::Element> {
        let mut footer = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::End)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.);
        if self.step != ProviderSetupStep::ProviderType {
            footer = footer.with_child(ChildView::new(&self.back_button).finish());
        }
        footer = footer.with_child(ChildView::new(&self.cancel_button).finish());
        if !matches!(self.step, ProviderSetupStep::Discover)
            || matches!(self.discovery_state, DiscoveryState::Failed(_))
        {
            footer = footer.with_child(ChildView::new(&self.next_button).finish());
        }
        footer.finish()
    }

    fn render_step_indicator(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let steps = [
            (ProviderSetupStep::ProviderType, "Provider"),
            (ProviderSetupStep::Configure, "Configure"),
            (ProviderSetupStep::Discover, "Test"),
            (ProviderSetupStep::Models, "Models"),
        ];
        Flex::row()
            .with_spacing(10.)
            .with_children(steps.into_iter().map(|(step, label)| {
                let active = self.step == step;
                Text::new(label, appearance.ui_font_family(), INPUT_FONT_SIZE)
                    .with_color(
                        if active {
                            appearance.theme().accent()
                        } else {
                            appearance.theme().nonactive_ui_text_color()
                        }
                        .into(),
                    )
                    .with_style(Properties::default().weight(if active {
                        Weight::Bold
                    } else {
                        Weight::Normal
                    }))
                    .finish()
            }))
            .finish()
    }
}

impl Entity for ProviderSetupModalBody {
    type Event = ProviderSetupModalBodyEvent;
}

impl View for ProviderSetupModalBody {
    fn ui_name() -> &'static str {
        "ProviderSetupModalBody"
    }

    fn render(&self, app: &AppContext) -> Box<dyn galaxyui::Element> {
        let appearance = Appearance::as_ref(app);
        let content = match self.step {
            ProviderSetupStep::ProviderType => self.render_provider_type(appearance),
            ProviderSetupStep::Configure => self.render_configure(appearance, app),
            ProviderSetupStep::Discover => self.render_discovery(appearance),
            ProviderSetupStep::Models => self.render_models(appearance),
        };
        Flex::column()
            .with_spacing(20.)
            .with_child(self.render_step_indicator(appearance))
            .with_child(content)
            .with_child(self.render_footer())
            .finish()
    }
}

impl TypedActionView for ProviderSetupModalBody {
    type Action = ProviderSetupModalBodyAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ProviderSetupModalBodyAction::SelectProvider(kind) => {
                if self.provider_type != *kind {
                    self.draft_models.clear();
                    self.discovery_state = DiscoveryState::Idle;
                }
                self.provider_type = *kind;
                if *kind == ProviderSetupProviderType::ChatGPTSubscription {
                    self.draft_base_url.clear();
                    self.draft_api_key = None;
                }
                self.sync_provider_type_buttons(ctx);
                self.sync_bedrock_auth_buttons(ctx);
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupModalBodyAction::Next => match self.step {
                ProviderSetupStep::ProviderType => {
                    self.step = ProviderSetupStep::Configure;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
                ProviderSetupStep::Configure => self.begin_discovery(ctx),
                ProviderSetupStep::Discover => {
                    if matches!(self.discovery_state, DiscoveryState::Failed(_)) {
                        self.begin_discovery(ctx);
                    }
                }
                ProviderSetupStep::Models => match self.provider_type {
                    ProviderSetupProviderType::OpenAICompatible
                    | ProviderSetupProviderType::ChatGPTSubscription
                    | ProviderSetupProviderType::Anthropic
                    | ProviderSetupProviderType::Gemini
                    | ProviderSetupProviderType::VertexAI => {
                        if self.draft_name.trim().is_empty()
                            || !self.draft_models.iter().any(|model| model.enabled)
                        {
                            return;
                        }
                        ctx.emit(ProviderSetupModalBodyEvent::SaveOpenAI {
                            editing_index: self.editing_index,
                            provider: self.draft_provider(),
                        });
                    }
                    ProviderSetupProviderType::Bedrock => {
                        if self.draft_name.trim().is_empty() || self.draft_bedrock.models.is_empty()
                        {
                            return;
                        }
                        let mut draft = self.draft_bedrock.clone();
                        draft.name = self.draft_name.trim().to_string();
                        ctx.emit(ProviderSetupModalBodyEvent::SaveBedrock(draft));
                    }
                    ProviderSetupProviderType::Acp => {
                        if self.draft_name.trim().is_empty() {
                            return;
                        }
                        let mut draft = self.draft_acp.clone();
                        draft.name = self.draft_name.trim().to_string();
                        ctx.emit(ProviderSetupModalBodyEvent::SaveAcp(draft));
                    }
                },
            },
            ProviderSetupModalBodyAction::Back => match self.step {
                ProviderSetupStep::ProviderType => {}
                ProviderSetupStep::Configure => {
                    self.step = ProviderSetupStep::ProviderType;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
                ProviderSetupStep::Discover => {
                    self.step = ProviderSetupStep::Configure;
                    self.discovery_state = DiscoveryState::Idle;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
                ProviderSetupStep::Models => {
                    self.step = ProviderSetupStep::Configure;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
            },
            ProviderSetupModalBodyAction::Cancel => {
                ctx.emit(ProviderSetupModalBodyEvent::Close);
            }
            ProviderSetupModalBodyAction::SelectAcpAgent(agent_id) => {
                self.draft_acp.agent_id = agent_id.clone();
                if !agent_id.eq_ignore_ascii_case("custom") {
                    self.draft_acp.command.clear();
                    self.draft_acp.args.clear();
                }
                self.sync_editors(ctx);
                self.sync_acp_agent_buttons(ctx);
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupModalBodyAction::ToggleModel(index) => {
                if let Some(model) = self.draft_models.get_mut(*index) {
                    model.enabled = !model.enabled;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
            }
            ProviderSetupModalBodyAction::CycleModelCapability(index, capability) => {
                if let Some(model) = self.draft_models.get_mut(*index) {
                    let key = capability.setting_key().to_string();
                    let next = model.capability_override(&key).next();
                    model.capability_overrides.insert(key, next);
                    self.update_next_button(ctx);
                    self.sync_model_switches(ctx);
                    ctx.notify();
                }
            }
            ProviderSetupModalBodyAction::ConnectChatGPT => {
                #[cfg(not(target_family = "wasm"))]
                ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.connect(ctx));
            }
            ProviderSetupModalBodyAction::OpenChatGPTDevicePage => {
                #[cfg(not(target_family = "wasm"))]
                let auth_state = ChatGPTAuthModel::as_ref(ctx).state().clone();
                let verification_uri = match auth_state {
                    ChatGPTAuthState::AwaitingDeviceCode {
                        verification_uri, ..
                    } => Some(verification_uri),
                    _ => None,
                };
                if let Some(verification_uri) = verification_uri {
                    ctx.open_url(&verification_uri);
                }
            }
            ProviderSetupModalBodyAction::CopyChatGPTDeviceCode => {
                #[cfg(not(target_family = "wasm"))]
                let auth_state = ChatGPTAuthModel::as_ref(ctx).state().clone();
                if let ChatGPTAuthState::AwaitingDeviceCode { user_code, .. } = auth_state {
                    ctx.clipboard()
                        .write(ClipboardContent::plain_text(user_code));
                }
            }
            ProviderSetupModalBodyAction::SelectBedrockAuth(method) => {
                self.draft_bedrock.auth_method = *method;
                self.sync_bedrock_auth_buttons(ctx);
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupModalBodyAction::ToggleBedrockCrossRegion => {
                self.draft_bedrock.cross_region_inference =
                    !self.draft_bedrock.cross_region_inference;
                ctx.notify();
            }
            ProviderSetupModalBodyAction::ToggleBedrockAutoLogin => {
                self.draft_bedrock.auto_login = !self.draft_bedrock.auto_login;
                ctx.notify();
            }
        }
    }
}

fn provider_type_label(kind: ProviderSetupProviderType) -> &'static str {
    match kind {
        ProviderSetupProviderType::OpenAICompatible => "OpenAI-compatible API",
        ProviderSetupProviderType::ChatGPTSubscription => "ChatGPT subscription",
        ProviderSetupProviderType::Anthropic => "Anthropic",
        ProviderSetupProviderType::Gemini => "Google Gemini",
        ProviderSetupProviderType::VertexAI => "Google Vertex AI",
        ProviderSetupProviderType::Bedrock => "AWS Bedrock",
        ProviderSetupProviderType::Acp => "ACP agent runtime",
    }
}
