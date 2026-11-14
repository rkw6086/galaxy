use galaxy_cli::agent::Harness;
use galaxy_core::ui::theme::Fill;
use galaxyui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, Flex, FormattedTextElement, Hoverable, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, Padding, ParentElement, Radius, RowBackground, ScrollStateHandle,
    Scrollable, ScrollableElement, ScrollbarWidth, Table, TableColumnWidth, TableConfig, TableHeader,
    TableStateHandle, TableVerticalSizing, Text,
};
use galaxyui::fonts::{Properties, Weight};
use galaxyui::platform::Cursor;
use galaxyui::text_layout::ClipConfig;
use galaxyui::ui_components::button::ButtonVariant;
use galaxyui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use galaxyui::ui_components::switch::SwitchStateHandle;
use galaxyui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};
use pathfinder_color::ColorU;

pub(super) fn model_catalog_table(
    state: TableStateHandle,
    scroll_state: ScrollStateHandle,
    row_count: usize,
    render_row: impl Fn(usize, &AppContext) -> Vec<Box<dyn Element>> + 'static,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let headers = [
        ("Use", TableColumnWidth::Fixed(64.)),
        ("Model", TableColumnWidth::Flex(2.)),
        ("Context", TableColumnWidth::Fixed(136.)),
        ("Capabilities", TableColumnWidth::Flex(1.5)),
    ]
    .into_iter()
    .map(|(label, width)| {
        TableHeader::new(
            Text::new(label.to_owned(), appearance.ui_font_family(), 11.)
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .with_width(width)
    })
    .collect();
    // Keep catalog layout proportional to the viewport, including when the
    // surrounding settings page gives its children unbounded height.
    let table = Table::new(state, 700., 360.)
        .with_headers(headers)
        .with_row_count(row_count)
        .with_row_render_fn(render_row)
        .with_config(TableConfig {
            border_width: 1.,
            border_color: appearance.theme().outline().into(),
            outer_border: true,
            column_dividers: false,
            row_dividers: true,
            cell_padding: 12.,
            header_background: appearance.theme().surface_2().into(),
            row_background: RowBackground::uniform(appearance.theme().surface_1().into()),
            fixed_header: true,
            vertical_sizing: TableVerticalSizing::Viewported,
            measure_body_cells_for_intrinsic_widths: false,
        })
        .finish_scrollable();
    ConstrainedBox::new(
        Scrollable::vertical(
            scroll_state,
            table,
            ScrollbarWidth::Auto,
            appearance.theme().nonactive_ui_detail().into(),
            appearance.theme().active_ui_detail().into(),
            appearance.theme().surface_1().into(),
        )
        .with_overlayed_scrollbar()
        .finish(),
    )
    .with_max_height(360.)
    .finish()
}

#[cfg(not(target_family = "wasm"))]
use crate::ai::chatgpt_auth::{ChatGPTAuthModel, ChatGPTAuthModelEvent, ChatGPTAuthState};
use crate::ai::harness_display;
use crate::ai::llms::{merge_discovered_provider_models, replace_litellm_catalog, LLMPreferences};
use crate::appearance::Appearance;
use crate::editor::{
    EditOrigin, EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::settings::ai::{
    AcpConfigOptionSettings, BedrockAuthMethod, BedrockModelConfig, ModelCapabilityOverride,
    OpenAIModelConfig, OpenAIProviderConfig, OpenAIProviderKind,
};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, NakedTheme, PrimaryTheme, SecondaryTheme,
};
use crate::view_components::{Dropdown, DropdownItem};

const INPUT_FONT_SIZE: f32 = 12.;
const MODEL_LOGO_SIZE: f32 = 20.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderSetupStep {
    ProviderType,
    Configure,
    Discover,
    Models,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSetupProviderType {
    OpenAI,
    LiteLLM,
    ChatGPTSubscription,
    Anthropic,
    Gemini,
    VertexAI,
    Bedrock,
    Acp,
}

const PROVIDER_TYPE_OPTIONS: &[(ProviderSetupProviderType, &str, &str)] = &[
    (
        ProviderSetupProviderType::OpenAI,
        "OpenAI",
        "Connect directly to OpenAI's API with an API key.",
    ),
    (
        ProviderSetupProviderType::LiteLLM,
        "LiteLLM",
        "Connect LiteLLM and use its richer model metadata APIs during discovery.",
    ),
    (
        ProviderSetupProviderType::ChatGPTSubscription,
        "ChatGPT subscription",
        "Use your ChatGPT Plus or Pro subscription with native OAuth.",
    ),
    (
        ProviderSetupProviderType::Anthropic,
        "Anthropic / Claude subscription",
        "Use an Anthropic API key or a Claude Code authorization token with Galaxy’s own tools.",
    ),
    (
        ProviderSetupProviderType::Bedrock,
        "AWS Bedrock",
        "Use the AWS Bedrock credentials and model configuration already managed by Galaxy.",
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

pub enum ProviderSetupViewEvent {
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
pub enum ProviderSetupViewAction {
    SelectProvider(ProviderSetupProviderType),
    JumpToStep(ProviderSetupStep),
    Next,
    Back,
    Cancel,
    ToggleModel(usize),
    CycleModelCapability(usize, CapabilityKey),
    ConnectChatGPT,
    CancelChatGPTSignIn,
    ConnectClaude,
    SignOutClaude,
    CancelClaudeSignIn,
    SelectBedrockAuth(BedrockAuthMethod),
    SelectBedrockProfile(String),
    ToggleBedrockCrossRegion,
    ToggleBedrockAutoLogin,
    SelectAcpAgent(String),
}

pub struct ProviderSetupView {
    step: ProviderSetupStep,
    editing_index: Option<usize>,
    provider_type: ProviderSetupProviderType,
    provider_type_locked: bool,
    draft_name: String,
    draft_base_url: String,
    draft_api_key: Option<String>,
    draft_project_id: String,
    draft_location: String,
    draft_models: Vec<OpenAIModelConfig>,
    draft_bedrock: BedrockProviderDraft,
    draft_acp: AcpProviderDraft,
    discovery_state: DiscoveryState,
    acp_setup_status: String,
    acp_discovery_generation: u64,
    acp_setup_abort: Option<futures::future::AbortHandle>,
    provider_type_buttons: Vec<ViewHandle<ActionButton>>,
    acp_agent_buttons: Vec<ViewHandle<ActionButton>>,
    name_editor: ViewHandle<EditorView>,
    base_url_editor: ViewHandle<EditorView>,
    api_key_editor: ViewHandle<EditorView>,
    project_id_editor: ViewHandle<EditorView>,
    location_editor: ViewHandle<EditorView>,
    bedrock_profile_dropdown: ViewHandle<Dropdown<ProviderSetupViewAction>>,
    bedrock_region_request: usize,
    bedrock_region_editor: ViewHandle<EditorView>,
    bedrock_refresh_command_editor: ViewHandle<EditorView>,
    bedrock_access_key_editor: ViewHandle<EditorView>,
    bedrock_secret_key_editor: ViewHandle<EditorView>,
    acp_agent_id_editor: ViewHandle<EditorView>,
    acp_command_editor: ViewHandle<EditorView>,
    acp_args_editor: ViewHandle<EditorView>,
    chatgpt_connect_mouse_state: MouseStateHandle,
    claude_connect_mouse_state: MouseStateHandle,
    claude_signin_abort: Option<futures::future::AbortHandle>,
    claude_signin_generation: u64,
    claude_signin_status: String,
    claude_credentials_available: bool,
    bedrock_auth_buttons: Vec<ViewHandle<ActionButton>>,
    bedrock_cross_region_toggle: SwitchStateHandle,
    bedrock_auto_login_toggle: SwitchStateHandle,
    model_switches: Vec<SwitchStateHandle>,
    bedrock_model_switches: Vec<SwitchStateHandle>,
    model_capability_switches: Vec<[SwitchStateHandle; 2]>,
    model_capability_buttons: Vec<Vec<ViewHandle<ActionButton>>>,
    model_context_editors: Vec<ViewHandle<EditorView>>,
    provider_type_scroll_state: ClippedScrollStateHandle,
    models_scroll_state: ClippedScrollStateHandle,
    model_table_state: TableStateHandle,
    model_table_scroll_state: ScrollStateHandle,
    step_tab_mouse_states: Vec<MouseStateHandle>,
    back_button: ViewHandle<ActionButton>,
    cancel_button: ViewHandle<ActionButton>,
    next_button: ViewHandle<ActionButton>,
}

impl ProviderSetupView {
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
                            ctx.dispatch_typed_action(ProviderSetupViewAction::SelectProvider(
                                kind,
                            ));
                        })
                })
            })
            .collect();

        let name_editor = Self::create_editor("Connection name", false, ctx);
        let base_url_editor = Self::create_editor("https://api.example.com/v1", false, ctx);
        let api_key_editor = Self::create_editor("sk-... (optional)", true, ctx);
        let project_id_editor = Self::create_editor("my-google-cloud-project", false, ctx);
        let location_editor = Self::create_editor("global", false, ctx);
        let bedrock_profile_dropdown = ctx.add_typed_action_view(Dropdown::new);
        let bedrock_region_editor =
            Self::create_editor("Region from AWS profile (or enter an override)", false, ctx);
        let bedrock_refresh_command_editor = Self::create_editor("aws sso login", false, ctx);
        let bedrock_access_key_editor = Self::create_editor("AKIA...", false, ctx);
        let bedrock_secret_key_editor = Self::create_editor("Secret access key", true, ctx);
        let acp_agent_id_editor = Self::create_editor("claude-acp", false, ctx);
        let acp_command_editor = Self::create_editor("Optional executable", false, ctx);
        let acp_args_editor = Self::create_editor(r#"["arg1", "arg2"]"#, false, ctx);

        let acp_agent_buttons = galaxy_acp::known_acp_agents()
            .iter()
            .map(|agent| {
                let id = agent.id.to_owned();
                ctx.add_typed_action_view(move |_| {
                    ActionButton::new(agent.name, NakedTheme)
                        .with_full_width(true)
                        .on_click({
                            let id = id.clone();
                            move |ctx| {
                                ctx.dispatch_typed_action(ProviderSetupViewAction::SelectAcpAgent(
                                    id.clone(),
                                ));
                            }
                        })
                })
            })
            .collect::<Vec<_>>();

        let bedrock_auth_buttons = [
            BedrockAuthMethod::Profile,
            BedrockAuthMethod::Sso,
            BedrockAuthMethod::StaticKeys,
        ]
        .into_iter()
        .map(|method| {
            ctx.add_typed_action_view(move |_| {
                ActionButton::new(method.display_name(), NakedTheme).on_click(move |ctx| {
                    ctx.dispatch_typed_action(ProviderSetupViewAction::SelectBedrockAuth(method));
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
            if matches!(event, EditorEvent::Edited(origin) if *origin != EditOrigin::SystemEdit) {
                let value = editor.as_ref(ctx).buffer_text(ctx);
                me.cancel_claude_signin();
                me.draft_api_key = (!value.trim().is_empty()).then_some(value);
                me.claude_credentials_available = false;
                if me.provider_type == ProviderSetupProviderType::Anthropic {
                    me.draft_models.clear();
                }
                me.update_next_button(ctx);
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
                ctx.dispatch_typed_action(ProviderSetupViewAction::Back);
            })
        });
        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(ProviderSetupViewAction::Cancel);
            })
        });
        let next_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Next", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(ProviderSetupViewAction::Next);
            })
        });

        Self {
            step: ProviderSetupStep::ProviderType,
            editing_index: None,
            provider_type: ProviderSetupProviderType::LiteLLM,
            provider_type_locked: false,
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
                agent_id: "claude-acp".to_string(),
                command: String::new(),
                args: Vec::new(),
                config_options: Vec::new(),
            },
            discovery_state: DiscoveryState::Idle,
            acp_setup_status: String::new(),
            acp_discovery_generation: 0,
            acp_setup_abort: None,
            provider_type_buttons,
            acp_agent_buttons,
            name_editor,
            base_url_editor,
            api_key_editor,
            project_id_editor,
            location_editor,
            bedrock_profile_dropdown,
            bedrock_region_request: 0,
            bedrock_region_editor,
            bedrock_refresh_command_editor,
            bedrock_access_key_editor,
            bedrock_secret_key_editor,
            acp_agent_id_editor,
            acp_command_editor,
            acp_args_editor,
            chatgpt_connect_mouse_state: MouseStateHandle::default(),
            claude_connect_mouse_state: MouseStateHandle::default(),
            claude_signin_abort: None,
            claude_signin_generation: 0,
            claude_signin_status: String::new(),
            claude_credentials_available: false,
            bedrock_auth_buttons,
            bedrock_cross_region_toggle: SwitchStateHandle::default(),
            bedrock_auto_login_toggle: SwitchStateHandle::default(),
            model_switches: Vec::new(),
            bedrock_model_switches: Vec::new(),
            model_capability_switches: Vec::new(),
            model_capability_buttons: Vec::new(),
            model_context_editors: Vec::new(),
            provider_type_scroll_state: ClippedScrollStateHandle::default(),
            models_scroll_state: ClippedScrollStateHandle::default(),
            model_table_state: TableStateHandle::new(0, |_, _| Vec::new()),
            model_table_scroll_state: ScrollStateHandle::default(),
            step_tab_mouse_states: (0..4).map(|_| MouseStateHandle::default()).collect(),
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

    fn default_name(provider_type: ProviderSetupProviderType) -> &'static str {
        match provider_type {
            ProviderSetupProviderType::OpenAI => "OpenAI",
            ProviderSetupProviderType::LiteLLM => "LiteLLM",
            ProviderSetupProviderType::ChatGPTSubscription => "ChatGPT Subscription",
            ProviderSetupProviderType::Anthropic => "Anthropic",
            ProviderSetupProviderType::Gemini => "Google Gemini",
            ProviderSetupProviderType::VertexAI => "Google Vertex AI",
            ProviderSetupProviderType::Bedrock => "AWS Bedrock",
            ProviderSetupProviderType::Acp => "Claude Code",
        }
    }

    fn default_base_url(provider_type: ProviderSetupProviderType) -> &'static str {
        match provider_type {
            ProviderSetupProviderType::OpenAI => "https://api.openai.com/v1",
            ProviderSetupProviderType::LiteLLM
            | ProviderSetupProviderType::ChatGPTSubscription
            | ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI
            | ProviderSetupProviderType::Bedrock
            | ProviderSetupProviderType::Acp => "",
        }
    }

    pub fn begin_create(
        &mut self,
        provider_type: ProviderSetupProviderType,
        ctx: &mut ViewContext<Self>,
    ) {
        self.cancel_acp_setup();
        self.cancel_claude_signin();
        self.step = ProviderSetupStep::Configure;
        self.editing_index = None;
        self.provider_type = provider_type;
        self.provider_type_locked = true;
        self.draft_name = Self::default_name(provider_type).to_string();
        self.draft_base_url = Self::default_base_url(provider_type).to_string();
        self.draft_api_key = None;
        self.claude_credentials_available = false;
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
            agent_id: "claude-acp".to_string(),
            command: String::new(),
            args: Vec::new(),
            config_options: Vec::new(),
        };
        self.discovery_state = DiscoveryState::Idle;
        self.sync_editors(ctx);
        if self.provider_type == ProviderSetupProviderType::Bedrock {
            self.load_bedrock_profile_region(ctx);
        }
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
        self.cancel_claude_signin();
        // Editing an existing provider is a local catalog operation. Do not
        // send the user through credentials or model discovery again.
        self.step = ProviderSetupStep::Models;
        self.editing_index = Some(editing_index);
        self.provider_type_locked = true;
        self.provider_type = match provider.kind {
            OpenAIProviderKind::OpenAI => ProviderSetupProviderType::OpenAI,
            OpenAIProviderKind::LiteLLM => ProviderSetupProviderType::LiteLLM,
            OpenAIProviderKind::ChatGPTSubscription => {
                ProviderSetupProviderType::ChatGPTSubscription
            }
            OpenAIProviderKind::Anthropic => ProviderSetupProviderType::Anthropic,
            OpenAIProviderKind::Gemini => ProviderSetupProviderType::Gemini,
            OpenAIProviderKind::VertexAI => ProviderSetupProviderType::VertexAI,
        };
        self.draft_name = provider.name;
        self.draft_base_url = provider.base_url;
        self.draft_api_key = provider.api_key;
        {
            use galaxyui_extras::secure_storage::AppContextExt as _;
            self.claude_credentials_available =
                self.draft_api_key.as_deref().is_some_and(|reference| {
                    crate::ai::claude_subscription::is_stored_credential(reference)
                        && ctx.secure_storage().read_value(reference).is_ok()
                });
        }
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
        self.provider_type_locked = true;
        self.provider_type = ProviderSetupProviderType::Bedrock;
        self.draft_name = draft.name.clone();
        self.draft_bedrock = draft;
        self.draft_models.clear();
        self.discovery_state = DiscoveryState::Idle;
        self.bedrock_region_request += 1;
        self.sync_editors(ctx);
        self.sync_provider_type_buttons(ctx);
        self.sync_bedrock_auth_buttons(ctx);
        self.sync_model_switches(ctx);
        self.update_next_button(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    pub fn begin_edit_acp(&mut self, draft: AcpProviderDraft, ctx: &mut ViewContext<Self>) {
        self.cancel_acp_setup();
        self.step = if draft.config_options.is_empty() {
            ProviderSetupStep::Configure
        } else {
            ProviderSetupStep::Models
        };
        self.editing_index = None;
        self.provider_type_locked = true;
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

    pub fn acp_discovery_generation(&self) -> u64 {
        self.acp_discovery_generation
    }

    pub fn acp_discovery_is_current(&self, generation: u64) -> bool {
        self.provider_type == ProviderSetupProviderType::Acp
            && self.step == ProviderSetupStep::Discover
            && matches!(self.discovery_state, DiscoveryState::Loading)
            && self.acp_discovery_generation == generation
    }

    #[cfg(feature = "local_fs")]
    pub fn prepare_acp_setup(&mut self) -> futures::future::AbortRegistration {
        self.cancel_acp_setup();
        let (handle, registration) = futures::future::AbortHandle::new_pair();
        self.acp_setup_abort = Some(handle);
        registration
    }

    fn cancel_acp_setup(&mut self) {
        if let Some(handle) = self.acp_setup_abort.take() {
            handle.abort();
        }
    }

    pub fn set_acp_setup_status(&mut self, status: String, ctx: &mut ViewContext<Self>) {
        self.acp_setup_status = status;
        ctx.notify();
    }

    pub fn show_connection_error(&mut self, error: String, ctx: &mut ViewContext<Self>) {
        self.discovery_state = DiscoveryState::Failed(error);
        self.step = ProviderSetupStep::Discover;
        self.update_next_button(ctx);
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

    fn load_bedrock_profile_region(&mut self, ctx: &mut ViewContext<Self>) {
        self.bedrock_region_request += 1;
        #[cfg(not(target_family = "wasm"))]
        {
            use aws_config::meta::region::ProvideRegion;
            let request = self.bedrock_region_request;
            let profile = self.draft_bedrock.profile.clone();
            let requested_profile = profile.clone();
            let previous_region = self.draft_bedrock.region.clone();
            ctx.spawn(
                async move {
                    let provider = aws_config::profile::ProfileFileRegionProvider::builder()
                        .profile_name(profile)
                        .build();
                    ProvideRegion::region(&provider)
                        .await
                        .map(|region| region.as_ref().to_owned())
                },
                move |me, region, ctx| {
                    if request != me.bedrock_region_request
                        || me.provider_type != ProviderSetupProviderType::Bedrock
                        || me.draft_bedrock.profile != requested_profile
                        || me.draft_bedrock.region != previous_region
                    {
                        return;
                    }
                    me.draft_bedrock.region = region.unwrap_or_default();
                    me.bedrock_region_editor.update(ctx, |editor, ctx| {
                        editor.system_reset_buffer_text(&me.draft_bedrock.region, ctx);
                    });
                    me.update_next_button(ctx);
                    ctx.notify();
                },
            );
        }
    }

    fn sync_editors(&self, ctx: &mut ViewContext<Self>) {
        self.name_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_name, ctx);
        });
        self.base_url_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_base_url, ctx);
        });
        self.api_key_editor.update(ctx, |editor, ctx| {
            let credential = self.draft_api_key.as_deref().unwrap_or_default();
            let text = if crate::ai::claude_subscription::is_stored_credential(credential) {
                ""
            } else {
                credential
            };
            editor.system_reset_buffer_text(text, ctx);
        });
        self.project_id_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_project_id, ctx);
        });
        self.location_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text(&self.draft_location, ctx);
        });
        let mut profiles = vec!["default".to_owned()];
        #[cfg(not(target_family = "wasm"))]
        profiles.extend(crate::ai::provider::external_config::list_aws_profiles());
        profiles.push(self.draft_bedrock.profile.clone());
        profiles.retain(|profile| !profile.is_empty());
        profiles.sort();
        profiles.dedup();
        self.bedrock_profile_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_items(
                profiles
                    .into_iter()
                    .map(|profile| {
                        DropdownItem::new(
                            profile.clone(),
                            ProviderSetupViewAction::SelectBedrockProfile(profile),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown.set_selected_by_action(
                ProviderSetupViewAction::SelectBedrockProfile(self.draft_bedrock.profile.clone()),
                ctx,
            );
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
        self.bedrock_model_switches
            .resize_with(self.draft_bedrock.models.len(), SwitchStateHandle::default);
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
                                    ProviderSetupViewAction::CycleModelCapability(index, key),
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
                            model.default_context_size = Some(context_size);
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
            ProviderSetupStep::Configure => ("Next", !self.is_configure_valid()),
            ProviderSetupStep::Discover => (
                if matches!(self.discovery_state, DiscoveryState::Failed(_)) {
                    "Retry"
                } else {
                    "Testing..."
                },
                !matches!(self.discovery_state, DiscoveryState::Failed(_)),
            ),
            ProviderSetupStep::Models => match self.provider_type {
                ProviderSetupProviderType::OpenAI
                | ProviderSetupProviderType::LiteLLM
                | ProviderSetupProviderType::ChatGPTSubscription
                | ProviderSetupProviderType::Anthropic
                | ProviderSetupProviderType::Gemini
                | ProviderSetupProviderType::VertexAI => (
                    "Save",
                    self.draft_name.trim().is_empty()
                        || !self.draft_models.iter().any(|model| model.enabled)
                        || (self.provider_type == ProviderSetupProviderType::Anthropic
                            && !self.is_configure_valid()),
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
                ProviderSetupProviderType::OpenAI => OpenAIProviderKind::OpenAI,
                ProviderSetupProviderType::LiteLLM => OpenAIProviderKind::LiteLLM,
                ProviderSetupProviderType::ChatGPTSubscription => {
                    OpenAIProviderKind::ChatGPTSubscription
                }
                ProviderSetupProviderType::Bedrock | ProviderSetupProviderType::Acp => {
                    OpenAIProviderKind::LiteLLM
                }
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
                ProviderSetupProviderType::OpenAI
                    | ProviderSetupProviderType::LiteLLM
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

    fn jump_to_step(&mut self, step: ProviderSetupStep, ctx: &mut ViewContext<Self>) {
        if !self.can_jump_to_step(step) {
            return;
        }

        match step {
            ProviderSetupStep::ProviderType => {
                self.step = ProviderSetupStep::ProviderType;
                self.discovery_state = DiscoveryState::Idle;
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupStep::Configure => {
                self.step = ProviderSetupStep::Configure;
                self.discovery_state = DiscoveryState::Idle;
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupStep::Discover => {
                self.begin_discovery(ctx);
            }
            ProviderSetupStep::Models => {
                self.step = ProviderSetupStep::Models;
                self.discovery_state = DiscoveryState::Idle;
                self.sync_model_switches(ctx);
                self.update_next_button(ctx);
                ctx.focus(&self.name_editor);
                ctx.notify();
            }
        }
    }

    fn bedrock_client_config(&self) -> crate::ai::provider::client::BedrockClientConfig {
        crate::ai::provider::client::BedrockClientConfig {
            auth_method: self.draft_bedrock.auth_method,
            profile: self.draft_bedrock.profile.clone(),
            region: self.draft_bedrock.region.clone(),
            access_key_id: self.draft_bedrock.access_key_id.clone(),
            secret_access_key: self.draft_bedrock.secret_access_key.clone(),
            session_token: None,
            credentials_expiry: None,
            invocation_model_id: None,
            cross_region_inference: self.draft_bedrock.cross_region_inference,
        }
    }

    fn begin_discovery(&mut self, ctx: &mut ViewContext<Self>) {
        self.cancel_acp_setup();
        self.bedrock_region_request += 1;
        self.step = ProviderSetupStep::Discover;
        self.discovery_state = DiscoveryState::Loading;
        self.acp_setup_status.clear();
        self.acp_discovery_generation = self.acp_discovery_generation.wrapping_add(1);
        self.update_next_button(ctx);
        ctx.notify();

        match self.provider_type {
            ProviderSetupProviderType::ChatGPTSubscription => {
                self.try_discover_chatgpt(ctx);
                return;
            }
            ProviderSetupProviderType::Bedrock => {
                let config = self.bedrock_client_config();
                let requested_config = config.clone();
                let generation = self.acp_discovery_generation;
                let verification_generation = LLMPreferences::handle(ctx)
                    .update(ctx, |preferences, _| preferences.begin_bedrock_verification());
                ctx.spawn(
                    async move {
                        crate::ai::provider::discovery::discover_available_models(config).await
                    },
                    move |me, result, ctx| {
                        if generation != me.acp_discovery_generation
                            || me.provider_type != ProviderSetupProviderType::Bedrock
                            || me.step != ProviderSetupStep::Discover
                            || me.bedrock_client_config() != requested_config
                        {
                            return;
                        }
                        match result {
                        Ok(discovery) => {
                            for record in discovery.records {
                                crate::ai::remote_logging::log_view_event(ctx, record);
                            }
                            let models = discovery.models;
                            let accepted = LLMPreferences::handle(ctx).update(ctx, |preferences, ctx| {
                                preferences.record_bedrock_verification(
                                    verification_generation,
                                    requested_config,
                                    &models,
                                    ctx,
                                )
                            });
                            if !accepted {
                                me.discovery_state = DiscoveryState::Failed(
                                    "A newer Bedrock connection test superseded this result. Retry the test.".to_owned(),
                                );
                                me.update_next_button(ctx);
                                ctx.notify();
                                return;
                            }
                            me.draft_bedrock.models =
                                crate::ai::provider::discovery::preserve_model_toggles(
                                    &me.draft_bedrock.models,
                                    models,
                                );
                            me.sync_model_switches(ctx);
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
                        }
                    },
                );
                return;
            }
            ProviderSetupProviderType::Acp => {
                ctx.emit(ProviderSetupViewEvent::RequestAcpDiscovery(
                    self.draft_acp.clone(),
                ));
                return;
            }
            ProviderSetupProviderType::OpenAI
            | ProviderSetupProviderType::LiteLLM
            | ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI => {}
        }

        self.discovery_state = DiscoveryState::Loading;
        self.update_next_button(ctx);
        ctx.notify();

        let provider = self.draft_provider();
        let existing_models = self.draft_models.clone();
        let authoritative_catalog = provider.kind == OpenAIProviderKind::LiteLLM;
        let generation = self.acp_discovery_generation;
        let requested_kind = self.provider_type;
        let requested_credential = self.draft_api_key.clone();
        ctx.spawn(
            async move { LLMPreferences::discover_openai_provider_models(provider).await },
            move |me, result, ctx| {
                if generation != me.acp_discovery_generation
                    || requested_kind != me.provider_type
                    || requested_credential != me.draft_api_key
                    || me.step != ProviderSetupStep::Discover
                {
                    return;
                }
                match result {
                    Ok(models) => {
                        me.draft_models = if authoritative_catalog {
                            replace_litellm_catalog(&existing_models, models)
                        } else {
                            merge_discovered_provider_models(&existing_models, models)
                        };
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

        self.discovery_state = DiscoveryState::Loading;
        self.update_next_button(ctx);
        ctx.notify();

        let provider = self.draft_provider();
        let existing_models = self.draft_models.clone();
        ctx.spawn(
            async move { LLMPreferences::discover_openai_provider_models(provider).await },
            move |me, result, ctx| match result {
                Ok(models) => {
                    me.draft_models = crate::ai::llms::merge_discovered_chatgpt_subscription_models(
                        &existing_models,
                        models,
                    );
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
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
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

    fn cancel_claude_signin(&mut self) {
        self.claude_signin_generation = self.claude_signin_generation.wrapping_add(1);
        if let Some(abort) = self.claude_signin_abort.take() {
            abort.abort();
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(reference) = self.draft_api_key.as_deref() {
            crate::ai::claude_oauth::discard_draft(reference);
        }
        self.claude_signin_status.clear();
    }

    #[cfg(not(target_family = "wasm"))]
    fn connect_claude(&mut self, ctx: &mut ViewContext<Self>) {
        if self.claude_signin_abort.is_some() {
            return;
        }
        self.cancel_claude_signin();
        let login = match crate::ai::claude_oauth::BrowserLogin::start() {
            Ok(login) => login,
            Err(error) => {
                self.claude_signin_status = error;
                ctx.notify();
                return;
            }
        };
        let generation = self.claude_signin_generation;
        let (abort, registration) = futures::future::AbortHandle::new_pair();
        self.claude_signin_abort = Some(abort);
        self.claude_signin_status = "Waiting for Claude sign-in in your browser…".to_string();
        self.update_next_button(ctx);
        ctx.open_url(login.url.as_str());
        ctx.notify();
        ctx.spawn(
            futures::future::Abortable::new(login.finish(), registration),
            move |me, result, ctx| {
                if generation != me.claude_signin_generation
                    || me.provider_type != ProviderSetupProviderType::Anthropic
                {
                    return;
                }
                me.claude_signin_abort = None;
                match result {
                    Ok(Ok(credentials)) => {
                        let reference = format!(
                            "{}{}",
                            crate::ai::claude_subscription::OAUTH_REFERENCE_PREFIX,
                            uuid::Uuid::new_v4()
                        );
                        match crate::ai::claude_oauth::save_credentials(
                            &reference,
                            &credentials,
                            ctx,
                        ) {
                            Ok(()) => {
                                me.draft_api_key = Some(reference);
                                me.claude_credentials_available = true;
                                me.draft_models.clear();
                                me.claude_signin_status =
                                    "Claude subscription connected.".to_string();
                                me.sync_editors(ctx);
                            }
                            Err(error) => me.claude_signin_status = error,
                        }
                    }
                    Ok(Err(error)) => me.claude_signin_status = error,
                    Err(_) => me.claude_signin_status = "Claude sign-in cancelled.".to_string(),
                }
                me.update_next_button(ctx);
                ctx.notify();
            },
        );
    }

    fn render_claude_auth(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let connected = self.claude_credentials_available;
        let description = if !self.claude_signin_status.is_empty() {
            self.claude_signin_status.as_str()
        } else if connected {
            "Claude subscription credentials saved on this device."
        } else {
            "Sign in with your Claude subscription in your browser."
        };
        let mut children = vec![
            Self::render_label(appearance, "Claude subscription authorization"),
            Text::new(
                description.to_string(),
                appearance.ui_font_family(),
                INPUT_FONT_SIZE,
            )
            .with_color(appearance.theme().nonactive_ui_text_color().into())
            .soft_wrap(true)
            .finish(),
        ];
        {
            let pending = self.claude_signin_abort.is_some();
            children.push(
                appearance
                    .ui_builder()
                    .button(
                        ButtonVariant::Secondary,
                        self.claude_connect_mouse_state.clone(),
                    )
                    .with_text_label(
                        if pending {
                            "Cancel sign-in"
                        } else if connected {
                            "Sign out"
                        } else {
                            "Sign in with Claude"
                        }
                        .to_string(),
                    )
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(if pending {
                            ProviderSetupViewAction::CancelClaudeSignIn
                        } else if connected {
                            ProviderSetupViewAction::SignOutClaude
                        } else {
                            ProviderSetupViewAction::ConnectClaude
                        });
                    })
                    .finish(),
            );
        }
        Flex::column()
            .with_spacing(10.)
            .with_children(children)
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
            ChatGPTAuthState::AwaitingBrowser => "Waiting for ChatGPT sign-in in your browser...",
            ChatGPTAuthState::ExchangingToken => "Completing sign-in...",
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

        let pending = matches!(state,
            ChatGPTAuthState::AwaitingBrowser | ChatGPTAuthState::ExchangingToken
        );
        {
            children.push(
                appearance
                    .ui_builder()
                    .button(
                        ButtonVariant::Secondary,
                        self.chatgpt_connect_mouse_state.clone(),
                    )
                    .with_text_label(if pending {
                        "Cancel sign-in"
                    } else if matches!(state, ChatGPTAuthState::Connected) {
                        "Reconnect ChatGPT"
                    } else {
                        "Connect ChatGPT"
                    }.to_owned())
                    .build()
                    .on_click(move |ctx, _, _| {
                        let action = if pending {
                            ProviderSetupViewAction::CancelChatGPTSignIn
                        } else {
                            ProviderSetupViewAction::ConnectChatGPT
                        };
                        ctx.dispatch_typed_action(action);
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
            ProviderSetupProviderType::OpenAI => {
                children.push(self.render_input(appearance, "Base URL", &self.base_url_editor));
                children.push(self.render_input(appearance, "API key", &self.api_key_editor));
                children.push(
                    Text::new(
                        "The API key is stored locally and is never synced to the cloud. Models will be discovered from OpenAI's /models endpoint.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::LiteLLM => {
                children.push(self.render_input(appearance, "Base URL", &self.base_url_editor));
                children.push(self.render_input(appearance, "API key", &self.api_key_editor));
                children.push(
                    Text::new(
                        "The API key is stored locally and is never synced to the cloud. LiteLLM model discovery uses /model/info for rich metadata, then falls back to /models.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
            }
            ProviderSetupProviderType::Anthropic => {
                children.push(self.render_claude_auth(appearance));
                if !self
                    .draft_api_key
                    .as_deref()
                    .is_some_and(crate::ai::claude_subscription::is_stored_credential)
                {
                    children.push(self.render_input(
                        appearance,
                        "API key or setup token (optional alternative)",
                        &self.api_key_editor,
                    ));
                }
                children.push(
                    Text::new(
                        "Sign in with Claude to use your subscription. OAuth credentials are saved in Galaxy’s secure credential store and refreshed automatically. Alternatively, paste an API key or a token from `claude setup-token`; pasted setup tokens must be replaced manually if they expire. API keys use Anthropic API billing.",
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .soft_wrap(true)
                    .finish(),
                );
                children.push(
                    Text::new(
                        crate::ai::claude_subscription::SUBSCRIPTION_WARNING,
                        appearance.ui_font_family(),
                        INPUT_FONT_SIZE,
                    )
                    .with_color(appearance.theme().accent().into())
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
                        children.push(Self::render_label(appearance, "AWS profile"));
                        children.push(ChildView::new(&self.bedrock_profile_dropdown).finish());
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
                                            ProviderSetupViewAction::ToggleBedrockAutoLogin,
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
                                ProviderSetupViewAction::ToggleBedrockCrossRegion,
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
                        "Test downloads Claude Code and its ACP adapter when missing. Sign-in opens through Claude Code in your browser; Galaxy does not store your Claude credentials.",
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

    fn render_discovery(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn galaxyui::Element> {
        let (message, color) = match &self.discovery_state {
            DiscoveryState::Idle | DiscoveryState::Loading => (
                if self.provider_type == ProviderSetupProviderType::Acp
                    && !self.acp_setup_status.is_empty()
                {
                    self.acp_setup_status.clone()
                } else {
                    "Testing the connection and discovering available models...".to_string()
                },
                appearance.theme().active_ui_text_color().into_solid(),
            ),
            DiscoveryState::Failed(error) => (error.clone(), appearance.theme().ui_error_color()),
        };
        let mut column = Flex::column()
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
            });
        if self.provider_type == ProviderSetupProviderType::ChatGPTSubscription
            && matches!(self.discovery_state, DiscoveryState::Failed(_))
        {
            column = column.with_child(self.render_chatgpt_auth(appearance, app));
        }
        column.finish()
    }

    fn render_model_table(
        &self,
        appearance: &Appearance,
        rows: Vec<Box<dyn galaxyui::Element>>,
        spacing: f32,
    ) -> Box<dyn galaxyui::Element> {
        let list = Flex::column()
            .with_spacing(spacing)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
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
        .with_max_height(430.)
        .finish()
    }

    fn model_logo(&self) -> (Icon, ColorU) {
        match self.provider_type {
            ProviderSetupProviderType::ChatGPTSubscription
            | ProviderSetupProviderType::OpenAI
            | ProviderSetupProviderType::LiteLLM => {
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

    fn render_model_capabilities(
        buttons: &[ViewHandle<ActionButton>],
    ) -> Box<dyn galaxyui::Element> {
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

    fn render_openai_model_cells(
        appearance: &Appearance,
        index: usize,
        model: &OpenAIModelConfig,
        switch: SwitchStateHandle,
        context_editor: ViewHandle<EditorView>,
        buttons: &[ViewHandle<ActionButton>],
    ) -> Vec<Box<dyn Element>> {
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
        let context_input = appearance
            .ui_builder()
            .text_input(context_editor)
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

        let toggle = appearance
            .ui_builder()
            .switch(switch)
            .check(model.enabled)
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(ProviderSetupViewAction::ToggleModel(index))
            })
            .finish();
        vec![
            Flex::row().with_child(toggle).finish(),
            model_info,
            ConstrainedBox::new(context_input).with_width(100.).finish(),
            Self::render_model_capabilities(buttons),
        ]
    }

    fn render_models(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        if self.provider_type == ProviderSetupProviderType::Bedrock {
            let models = self.draft_bedrock.models.clone();
            let switches = self.bedrock_model_switches.clone();
            let table = model_catalog_table(
                self.model_table_state.clone(),
                self.model_table_scroll_state.clone(),
                models.len(),
                move |index, app| {
                    let appearance = Appearance::as_ref(app);
                    let model = &models[index];
                    let text = |value: String| {
                        Text::new(value, appearance.ui_font_family(), 11.)
                            .with_color(appearance.theme().nonactive_ui_text_color().into())
                            .soft_wrap(true)
                            .finish()
                    };
                    let toggle = appearance
                        .ui_builder()
                        .switch(switches[index].clone())
                        .check(model.enabled)
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(ProviderSetupViewAction::ToggleModel(index))
                        })
                        .finish();
                    let name = Flex::column()
                        .with_spacing(3.)
                        .with_child(text(model.display_name.clone()))
                        .with_child(text(model.model_id.clone()))
                        .finish();
                    let context = galaxy_bedrock_model_catalog::model_metadata(&model.model_id)
                        .and_then(|metadata| metadata.context_window_tokens)
                        .map(|tokens| tokens.to_string())
                        .unwrap_or_else(|| "Unknown".to_owned());
                    vec![
                        Flex::row().with_child(toggle).finish(),
                        name,
                        text(context),
                        text(
                            if model.vision_supported {
                                "Images"
                            } else {
                                "Text"
                            }
                            .to_owned(),
                        ),
                    ]
                },
                appearance,
            );
            return Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(12.)
                .with_child(self.render_input(appearance, "Connection name", &self.name_editor))
                .with_child(Text::new("Only models passing AWS agreement, authorization, entitlement, and region checks are offered. Choose which models Galaxy should enable.", appearance.ui_font_family(), INPUT_FONT_SIZE)
                    .with_color(appearance.theme().nonactive_ui_text_color().into()).soft_wrap(true).finish())
                .with_child(self.render_model_table(appearance, vec![table], 0.)).finish();
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
        let models = self.draft_models.clone();
        let switches = self.model_switches.clone();
        let editors = self.model_context_editors.clone();
        let buttons = self.model_capability_buttons.clone();
        let table = model_catalog_table(
            self.model_table_state.clone(),
            self.model_table_scroll_state.clone(),
            models.len(),
            move |index, app| {
                Self::render_openai_model_cells(
                    Appearance::as_ref(app),
                    index,
                    &models[index],
                    switches[index].clone(),
                    editors[index].clone(),
                    &buttons[index],
                )
            },
            appearance,
        );
        let table = self.render_model_table(appearance, vec![table], 0.);

        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
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
        if self.can_go_back() {
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

    fn can_go_back(&self) -> bool {
        match self.step {
            ProviderSetupStep::ProviderType => false,
            ProviderSetupStep::Configure => !self.provider_type_locked,
            ProviderSetupStep::Discover | ProviderSetupStep::Models => true,
        }
    }

    fn is_configure_valid(&self) -> bool {
        if self.claude_signin_abort.is_some() {
            return false;
        }
        match self.provider_type {
            ProviderSetupProviderType::OpenAI => {
                !self.draft_base_url.trim().is_empty()
                    && self
                        .draft_api_key
                        .as_deref()
                        .is_some_and(|key| !key.trim().is_empty())
            }
            ProviderSetupProviderType::LiteLLM => !self.draft_base_url.trim().is_empty(),
            ProviderSetupProviderType::Anthropic => {
                self.draft_api_key.as_deref().is_some_and(|key| {
                    if crate::ai::claude_subscription::is_stored_credential(key) {
                        self.claude_credentials_available
                    } else {
                        !key.trim().is_empty()
                    }
                })
            }
            ProviderSetupProviderType::Gemini => self
                .draft_api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()),
            ProviderSetupProviderType::VertexAI => !self.draft_project_id.trim().is_empty(),
            ProviderSetupProviderType::Acp => {
                !self.draft_acp.agent_id.trim().is_empty()
                    && (!self.draft_acp.agent_id.eq_ignore_ascii_case("custom")
                        || !self.draft_acp.command.trim().is_empty())
            }
            ProviderSetupProviderType::ChatGPTSubscription | ProviderSetupProviderType::Bedrock => {
                true
            }
        }
    }

    fn has_model_catalog(&self) -> bool {
        match self.provider_type {
            ProviderSetupProviderType::OpenAI
            | ProviderSetupProviderType::LiteLLM
            | ProviderSetupProviderType::ChatGPTSubscription
            | ProviderSetupProviderType::Anthropic
            | ProviderSetupProviderType::Gemini
            | ProviderSetupProviderType::VertexAI => !self.draft_models.is_empty(),
            ProviderSetupProviderType::Bedrock => !self.draft_bedrock.models.is_empty(),
            ProviderSetupProviderType::Acp => !self.draft_acp.config_options.is_empty(),
        }
    }

    fn step_tab_index(step: ProviderSetupStep) -> usize {
        match step {
            ProviderSetupStep::ProviderType => 0,
            ProviderSetupStep::Configure => 1,
            ProviderSetupStep::Discover => 2,
            ProviderSetupStep::Models => 3,
        }
    }

    fn can_jump_to_step(&self, step: ProviderSetupStep) -> bool {
        if self.step == step {
            return false;
        }

        match step {
            ProviderSetupStep::ProviderType => !self.provider_type_locked,
            ProviderSetupStep::Configure => true,
            ProviderSetupStep::Discover => {
                !matches!(self.discovery_state, DiscoveryState::Loading)
                    && self.is_configure_valid()
            }
            ProviderSetupStep::Models => self.has_model_catalog(),
        }
    }

    fn render_step_tab(
        &self,
        step: ProviderSetupStep,
        label: &'static str,
        appearance: &Appearance,
    ) -> Box<dyn galaxyui::Element> {
        let active = self.step == step;
        let enabled = self.can_jump_to_step(step);
        let Some(mouse_state) = self
            .step_tab_mouse_states
            .get(Self::step_tab_index(step))
            .cloned()
        else {
            return Text::new(label, appearance.ui_font_family(), INPUT_FONT_SIZE)
                .with_color(appearance.theme().disabled_ui_text_color().into())
                .finish();
        };

        let tab = Hoverable::new(mouse_state, move |mouse_state| {
            let theme = appearance.theme();
            let text_color = if active {
                theme.accent()
            } else if enabled {
                theme.nonactive_ui_text_color()
            } else {
                theme.disabled_ui_text_color()
            };
            let mut container = Container::new(
                Text::new_inline(
                    label.to_string(),
                    appearance.ui_font_family(),
                    INPUT_FONT_SIZE,
                )
                .with_color(text_color.into())
                .with_style(Properties::default().weight(if active {
                    Weight::Bold
                } else {
                    Weight::Normal
                }))
                .finish(),
            )
            .with_horizontal_padding(10.)
            .with_vertical_padding(6.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)));

            if active {
                container = container.with_background(theme.surface_overlay_1());
            } else if enabled && mouse_state.is_hovered() {
                container = container.with_background(theme.surface_overlay_2());
            }

            container.finish()
        });

        if enabled {
            tab.on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(ProviderSetupViewAction::JumpToStep(step));
            })
            .with_cursor(Cursor::PointingHand)
            .finish()
        } else {
            tab.finish()
        }
    }

    fn render_step_indicator(&self, appearance: &Appearance) -> Box<dyn galaxyui::Element> {
        let locked_steps = [
            (ProviderSetupStep::Configure, "Configure"),
            (ProviderSetupStep::Discover, "Test"),
            (ProviderSetupStep::Models, "Models"),
        ];
        let selectable_steps = [
            (ProviderSetupStep::ProviderType, "Provider"),
            (ProviderSetupStep::Configure, "Configure"),
            (ProviderSetupStep::Discover, "Test"),
            (ProviderSetupStep::Models, "Models"),
        ];
        let steps: &[(ProviderSetupStep, &str)] = if self.provider_type_locked {
            &locked_steps
        } else {
            &selectable_steps
        };
        Flex::row()
            .with_spacing(10.)
            .with_children(
                steps
                    .iter()
                    .map(|(step, label)| self.render_step_tab(*step, label, appearance)),
            )
            .finish()
    }
}

impl Entity for ProviderSetupView {
    type Event = ProviderSetupViewEvent;
}

impl View for ProviderSetupView {
    fn ui_name() -> &'static str {
        "ProviderSetupView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn galaxyui::Element> {
        let appearance = Appearance::as_ref(app);
        let content = match self.step {
            ProviderSetupStep::ProviderType => self.render_provider_type(appearance),
            ProviderSetupStep::Configure => self.render_configure(appearance, app),
            ProviderSetupStep::Discover => self.render_discovery(appearance, app),
            ProviderSetupStep::Models => self.render_models(appearance),
        };
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(20.)
            .with_child(self.render_step_indicator(appearance))
            .with_child(content)
            .with_child(self.render_footer())
            .finish()
    }
}

impl TypedActionView for ProviderSetupView {
    type Action = ProviderSetupViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ProviderSetupViewAction::SelectProvider(kind) => {
                if self.provider_type != *kind {
                    #[cfg(not(target_family = "wasm"))]
                    if self.provider_type == ProviderSetupProviderType::ChatGPTSubscription {
                        ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.cancel_sign_in(ctx));
                    }
                    self.cancel_claude_signin();
                    self.draft_api_key = None;
                    self.claude_credentials_available = false;
                    self.draft_models.clear();
                    self.discovery_state = DiscoveryState::Idle;
                }
                self.provider_type = *kind;
                self.draft_name = Self::default_name(*kind).to_string();
                self.draft_base_url = Self::default_base_url(*kind).to_string();
                if matches!(*kind, ProviderSetupProviderType::ChatGPTSubscription) {
                    self.draft_api_key = None;
                }
                self.sync_editors(ctx);
                if self.provider_type == ProviderSetupProviderType::Bedrock
                    && self.draft_bedrock.region.is_empty()
                {
                    self.load_bedrock_profile_region(ctx);
                }
                self.sync_provider_type_buttons(ctx);
                self.sync_bedrock_auth_buttons(ctx);
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupViewAction::JumpToStep(step) => self.jump_to_step(*step, ctx),
            ProviderSetupViewAction::Next => match self.step {
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
                    ProviderSetupProviderType::OpenAI
                    | ProviderSetupProviderType::LiteLLM
                    | ProviderSetupProviderType::ChatGPTSubscription
                    | ProviderSetupProviderType::Anthropic
                    | ProviderSetupProviderType::Gemini
                    | ProviderSetupProviderType::VertexAI => {
                        if self.draft_name.trim().is_empty()
                            || !self.draft_models.iter().any(|model| model.enabled)
                            || (self.provider_type == ProviderSetupProviderType::Anthropic
                                && !self.is_configure_valid())
                        {
                            return;
                        }
                        ctx.emit(ProviderSetupViewEvent::SaveOpenAI {
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
                        ctx.emit(ProviderSetupViewEvent::SaveBedrock(draft));
                    }
                    ProviderSetupProviderType::Acp => {
                        if self.draft_name.trim().is_empty() {
                            return;
                        }
                        let mut draft = self.draft_acp.clone();
                        draft.name = self.draft_name.trim().to_string();
                        ctx.emit(ProviderSetupViewEvent::SaveAcp(draft));
                    }
                },
            },
            ProviderSetupViewAction::Back => match self.step {
                ProviderSetupStep::ProviderType => {}
                ProviderSetupStep::Configure => {
                    if self.provider_type_locked {
                        return;
                    }
                    self.step = ProviderSetupStep::ProviderType;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
                ProviderSetupStep::Discover => {
                    self.cancel_acp_setup();
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
            ProviderSetupViewAction::Cancel => {
                self.cancel_acp_setup();
                self.cancel_claude_signin();
                #[cfg(not(target_family = "wasm"))]
                if self.provider_type == ProviderSetupProviderType::ChatGPTSubscription {
                    ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.cancel_sign_in(ctx));
                }
                self.discovery_state = DiscoveryState::Idle;
                ctx.emit(ProviderSetupViewEvent::Close);
            }
            ProviderSetupViewAction::SelectAcpAgent(agent_id) => {
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
            ProviderSetupViewAction::ToggleModel(index) => {
                let models = if self.provider_type == ProviderSetupProviderType::Bedrock {
                    &mut self.draft_bedrock.models
                } else {
                    if let Some(model) = self.draft_models.get_mut(*index) {
                        model.enabled = !model.enabled;
                        self.update_next_button(ctx);
                        ctx.notify();
                    }
                    return;
                };
                if let Some(model) = models.get_mut(*index) {
                    model.enabled = !model.enabled;
                    self.update_next_button(ctx);
                    ctx.notify();
                }
            }
            ProviderSetupViewAction::CycleModelCapability(index, capability) => {
                if let Some(model) = self.draft_models.get_mut(*index) {
                    let key = capability.setting_key().to_string();
                    let next = model.capability_override(&key).next();
                    model.capability_overrides.insert(key, next);
                    self.update_next_button(ctx);
                    self.sync_model_switches(ctx);
                    ctx.notify();
                }
            }
            ProviderSetupViewAction::ConnectChatGPT => {
                #[cfg(not(target_family = "wasm"))]
                ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.connect(ctx));
            }
            ProviderSetupViewAction::CancelChatGPTSignIn => {
                #[cfg(not(target_family = "wasm"))]
                ChatGPTAuthModel::handle(ctx).update(ctx, |model, ctx| model.cancel_sign_in(ctx));
            }
            ProviderSetupViewAction::ConnectClaude => {
                #[cfg(not(target_family = "wasm"))]
                self.connect_claude(ctx);
            }
            ProviderSetupViewAction::SignOutClaude => {
                if let Some(reference) = self.draft_api_key.take() {
                    crate::ai::claude_subscription::remove_credential(&reference, ctx);
                }
                self.cancel_claude_signin();
                self.claude_credentials_available = false;
                self.claude_signin_status = "Signed out of Claude on this device.".to_string();
                self.draft_models.clear();
                self.discovery_state = DiscoveryState::Idle;
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupViewAction::CancelClaudeSignIn => {
                self.cancel_claude_signin();
                self.claude_signin_status = "Claude sign-in cancelled.".to_string();
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupViewAction::SelectBedrockProfile(profile) => {
                if self.draft_bedrock.profile != *profile {
                    self.draft_bedrock.profile = profile.clone();
                    self.draft_bedrock.region.clear();
                    self.draft_bedrock.models.clear();
                    self.discovery_state = DiscoveryState::Idle;
                    self.bedrock_region_editor.update(ctx, |editor, ctx| {
                        editor.system_reset_buffer_text("", ctx);
                    });
                    // The dropdown dispatches this action while it is borrowed.
                    // Update its selected label only after that handler returns.
                    let selected_profile = profile.clone();
                    ctx.spawn(async {}, move |me, _, ctx| {
                        if me.draft_bedrock.profile != selected_profile {
                            return;
                        }
                        me.bedrock_profile_dropdown.update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                ProviderSetupViewAction::SelectBedrockProfile(selected_profile),
                                ctx,
                            );
                        });
                    });
                    self.load_bedrock_profile_region(ctx);
                    self.update_next_button(ctx);
                    ctx.notify();
                }
            }
            ProviderSetupViewAction::SelectBedrockAuth(method) => {
                self.draft_bedrock.auth_method = *method;
                self.sync_bedrock_auth_buttons(ctx);
                self.update_next_button(ctx);
                ctx.notify();
            }
            ProviderSetupViewAction::ToggleBedrockCrossRegion => {
                self.draft_bedrock.cross_region_inference =
                    !self.draft_bedrock.cross_region_inference;
                ctx.notify();
            }
            ProviderSetupViewAction::ToggleBedrockAutoLogin => {
                self.draft_bedrock.auto_login = !self.draft_bedrock.auto_login;
                ctx.notify();
            }
        }
    }
}

fn provider_type_label(kind: ProviderSetupProviderType) -> &'static str {
    match kind {
        ProviderSetupProviderType::OpenAI => "OpenAI",
        ProviderSetupProviderType::LiteLLM => "LiteLLM",
        ProviderSetupProviderType::ChatGPTSubscription => "ChatGPT subscription",
        ProviderSetupProviderType::Anthropic => "Anthropic",
        ProviderSetupProviderType::Gemini => "Google Gemini",
        ProviderSetupProviderType::VertexAI => "Google Vertex AI",
        ProviderSetupProviderType::Bedrock => "AWS Bedrock",
        ProviderSetupProviderType::Acp => "Claude Code",
    }
}

impl Drop for ProviderSetupView {
    fn drop(&mut self) {
        self.cancel_claude_signin();
        self.cancel_acp_setup();
    }
}
