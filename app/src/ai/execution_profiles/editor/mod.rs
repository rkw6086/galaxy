use galaxy_core::ui::theme::color::internal_colors;
use itertools::Itertools;
use regex::Regex;
use settings::Setting;
use thousands::Separable;
use warpui::elements::{
    Align, Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CrossAxisAlignment, Expanded, Flex, Highlight, MouseStateHandle, ParentElement,
    PartialClickableElement, ScrollbarWidth, Text,
};
use warpui::fonts::Properties;
use warpui::platform::Cursor;
use warpui::ui_components::slider::SliderStateHandle;
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{
    AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use crate::ai::blocklist::BlocklistAIPermissions;
use crate::ai::execution_profiles::model_menu_items::{
    available_model_menu_items, collapse_model_reasoning_choices, reasoning_choices_for_model,
};
use crate::ai::execution_profiles::profiles::{
    AIExecutionProfilesModel, AIExecutionProfilesModelEvent, ClientProfileId,
};
use crate::ai::execution_profiles::{
    AIExecutionProfile, AIExecutionProfileAppExt as _, PermissionMode,
};
use crate::ai::llms::{
    DisableReason, LLMContextWindow, LLMId, LLMInfo, LLMPreferences, LLMPreferencesEvent,
    LLMProvider,
};
use crate::cloud_object::model::generic_string_model::StringModel;
use crate::editor::{
    EditorView, Event as EditorEvent, InteractionState, SingleLineEditorOptions, TextOptions,
};
use crate::menu::{MenuItem, MenuItemFields, MenuTooltipPosition};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view;
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent};
use crate::settings::{AISettings, AISettingsChangedEvent, AgentModeCommandExecutionPredicate};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{ActionButton, DangerSecondaryTheme};
use crate::view_components::dropdown::DropdownAction;
use crate::view_components::{
    Dropdown, DropdownItem, FilterableDropdown, SubmittableTextInput, SubmittableTextInputEvent,
};
use crate::workspace::WorkspaceAction;
use crate::workspaces::user_workspaces::{UserWorkspaces, UserWorkspacesEvent};
use crate::{Appearance, TemplatableMCPServerManager};

const MODEL_MENU_WIDTH: f32 = 480.;

/// Renders a footer banner for model dropdowns informing free-plan users that
/// frontier models require an upgrade, with a clickable "Upgrade" link.
fn render_upgrade_footer(
    upgrade_mouse_state: MouseStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let surface = theme.surface_2();
    let text_color = theme.main_text_color(surface);

    let info_icon = ConstrainedBox::new(
        galaxy_core::ui::Icon::Info
            .to_galaxyui_icon(text_color)
            .finish(),
    )
    .with_width(16.)
    .with_height(16.)
    .finish();

    let label = "Frontier models are unavailable on free plans. Upgrade";
    let upgrade_start = label.len() - "Upgrade".len();
    let info_text = Text::new(
        label,
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .with_color(text_color.into())
    .with_single_highlight(
        Highlight::new()
            .with_properties(Properties::default())
            .with_foreground_color(internal_colors::accent_fg(theme).into()),
        (upgrade_start..label.len()).collect(),
    )
    .with_hoverable_char_range(
        upgrade_start..label.len(),
        upgrade_mouse_state,
        Some(Cursor::PointingHand),
        |_is_hovered, _ctx, _app| {},
    )
    .with_clickable_char_range(upgrade_start..label.len(), move |_modifiers, ctx, _app| {
        ctx.dispatch_typed_action(WorkspaceAction::ShowUpgrade);
    })
    .finish();

    let inner = Container::new(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Container::new(info_icon)
                    .with_margin_right(6.)
                    .with_margin_top(2.)
                    .finish(),
            )
            .with_child(Expanded::new(1., info_text).finish())
            .finish(),
    )
    .with_horizontal_padding(16.)
    .with_vertical_padding(6.)
    .with_background(internal_colors::fg_overlay_1(theme))
    .with_border(Border::top(1.).with_border_color(internal_colors::neutral_3(theme)))
    .finish();

    Container::new(inner).with_background(surface).finish()
}

#[derive(Default)]
struct TooltipMouseStateHandles {
    permission_mode_tooltip_mouse_state: MouseStateHandle,
    mcp_denylist_editor_tooltip_mouse_state: MouseStateHandle,
}

pub mod manager;
pub use manager::*;

pub const HEADER_TEXT: &str = "Profile Editor";

#[derive(Debug, Clone)]
pub enum ExecutionProfileEditorViewEvent {
    Pane(PaneEvent),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProfileModelRole {
    Base,
    FullTerminalUse,
    ComputerUse,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionProfileEditorViewAction {
    Save,
    Close,
    SetBaseModel {
        id: LLMId,
    },
    SetModelReasoning {
        role: ProfileModelRole,
        id: LLMId,
    },
    /// Fired continuously while the user drags the context window slider.
    ContextWindowSliderDragged {
        value: u32,
    },
    /// Fired when the user commits a new context window value (slider drop,
    /// track click, or input box commit).
    SetContextWindowSize {
        value: u32,
    },
    SetCodingModel {
        id: LLMId,
    },
    SetFullTerminalUseModel {
        id: LLMId,
    },
    SetComputerUseModel {
        id: LLMId,
    },

    SetOrchestrationProviders {
        providers: Option<Vec<String>>,
    },
    RemoveFromCommandAllowlist {
        predicate: AgentModeCommandExecutionPredicate,
    },
    SetPermissionMode {
        mode: PermissionMode,
    },
    AddToCommandDenylist {
        predicate: AgentModeCommandExecutionPredicate,
    },
    RemoveFromCommandDenylist {
        predicate: AgentModeCommandExecutionPredicate,
    },
    AddToMCPDenylist {
        id: uuid::Uuid,
    },
    RemoveFromMCPDenylist {
        id: uuid::Uuid,
    },
    DeleteProfile,
    SetPlanAutoSync {
        enabled: bool,
    },
    SetWebSearchEnabled {
        enabled: bool,
    },
    /// User committed a new startup timeout value.
    /// `None` resets to the built-in default (120 s); a positive integer sets it explicitly.
    SetModelStartTimeout {
        value: Option<u32>,
    },
}

pub struct ExecutionProfileEditorView {
    profile_id: ClientProfileId,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
    clipped_scroll_state: ClippedScrollStateHandle,
    base_model_dropdown: ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
    context_window_slider_state: SliderStateHandle,
    context_window_editor: ViewHandle<EditorView>,
    last_synced_context_window_editor_value: Option<u32>,
    dragged_context_window_value: Option<u32>,
    coding_model_dropdown: ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
    full_terminal_use_model_dropdown:
        ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
    computer_use_model_dropdown: ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
    base_reasoning_dropdown: ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
    full_terminal_reasoning_dropdown: ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
    computer_use_reasoning_dropdown: ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
    reasoning_roles: Vec<ProfileModelRole>,
    last_model_selections: Option<[Option<LLMId>; 4]>,
    permission_mode_dropdown: ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
    command_denylist_editor: ViewHandle<SubmittableTextInput>,
    command_allowlist_editor: ViewHandle<SubmittableTextInput>,
    command_allowlist_mouse_state_handles: Vec<MouseStateHandle>,
    orchestration_provider_checkboxes: Vec<(String, MouseStateHandle)>,
    all_providers_checkbox: MouseStateHandle,
    command_denylist_mouse_state_handles: Vec<MouseStateHandle>,
    command_denylist_tooltip_mouse_state_handles: Vec<MouseStateHandle>,
    mcp_denylist_dropdown: ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
    mcp_denylist_mouse_state_handles: Vec<MouseStateHandle>,
    profile_name_editor: ViewHandle<EditorView>,
    delete_button: ViewHandle<ActionButton>,
    tooltip_mouse_state_handles: TooltipMouseStateHandles,
    plan_auto_sync_switch: SwitchStateHandle,
    web_search_switch: SwitchStateHandle,
    upgrade_footer_mouse_state: MouseStateHandle,
    model_start_timeout_editor: ViewHandle<EditorView>,
    last_synced_model_start_timeout_editor_value: Option<Option<u32>>,
}

impl ExecutionProfileEditorView {
    pub fn new(profile_id: ClientProfileId, ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_ctx| PaneConfiguration::new(HEADER_TEXT));

        let permission_mode_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_items(
                [
                    PermissionMode::ReadOnly,
                    PermissionMode::AskFirst,
                    PermissionMode::GalaxyDecides,
                    PermissionMode::AllowAll,
                ]
                .into_iter()
                .map(|mode| {
                    DropdownItem::new(
                        mode.label(),
                        ExecutionProfileEditorViewAction::SetPermissionMode { mode },
                    )
                })
                .collect(),
                ctx,
            );
            dropdown
        });
        let mcp_denylist_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_menu_header_to_static("Select MCP servers");
            dropdown
        });

        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let profile_data = permissions.permissions_profile_for_id(ctx, profile_id);

        let mcp_denylist_mouse_state_handles = profile_data
            .mcp_denylist
            .iter()
            .map(|_| Default::default())
            .collect();

        let base_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(MODEL_MENU_WIDTH);
            dropdown.set_menu_width(MODEL_MENU_WIDTH, ctx);
            dropdown
        });

        // Initialize the context window editor buffer with the profile's
        // persisted limit (or the active model's max as a sensible default).
        // The slider's current position is derived from the profile on each
        // render, so no local Cell is needed.
        let initial_context_window_value = initial_context_window_display_value(&profile_data, ctx);
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
            editor.set_buffer_text(&initial_context_window_value.separate_with_commas(), ctx);
            editor
        });
        let last_synced_context_window_editor_value = Some(initial_context_window_value);

        let coding_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(MODEL_MENU_WIDTH);
            dropdown.set_menu_width(MODEL_MENU_WIDTH, ctx);
            dropdown
        });
        let full_terminal_use_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(MODEL_MENU_WIDTH);
            dropdown.set_menu_width(MODEL_MENU_WIDTH, ctx);
            dropdown
        });
        let computer_use_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(MODEL_MENU_WIDTH);
            dropdown.set_menu_width(MODEL_MENU_WIDTH, ctx);
            dropdown
        });
        let base_reasoning_dropdown = ctx.add_typed_action_view(Dropdown::new);
        let full_terminal_reasoning_dropdown = ctx.add_typed_action_view(Dropdown::new);
        let computer_use_reasoning_dropdown = ctx.add_typed_action_view(Dropdown::new);
        let command_allowlist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. ^cargo test$", ctx);
            input
        });
        let command_denylist_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("e.g. rm .*", ctx);
            input
        });

        let command_denylist_mouse_state_handles = profile_data
            .command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();

        let profile_name_editor = ctx.add_view(|ctx| {
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    max_buffer_len: Some(super::PROFILE_NAME_MAX_LENGTH),
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("e.g. \"YOLO code\"", ctx);
            editor
        });

        let font_family = Appearance::as_ref(ctx).ui_font_family();

        profile_name_editor.update(ctx, |editor, ctx| {
            editor.set_font_size(12., ctx);
            editor.set_font_family(font_family, ctx);
        });

        Self::update_profile_name_editor(&profile_name_editor, &profile_data, ctx);

        let delete_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Delete profile", DangerSecondaryTheme)
                .with_icon(Icon::Trash)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(ExecutionProfileEditorViewAction::DeleteProfile);
                })
        });

        let initial_timeout_text = profile_data
            .model_start_timeout_secs
            .map(|s| s.to_string())
            .unwrap_or_default();
        let model_start_timeout_editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(Appearance::as_ref(ctx).ui_font_size()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text("default (120)", ctx);
            editor.set_buffer_text(&initial_timeout_text, ctx);
            editor
        });
        let last_synced_model_start_timeout_editor_value =
            Some(profile_data.model_start_timeout_secs);

        let mut view = Self {
            profile_id,
            pane_configuration,
            focus_handle: None,
            clipped_scroll_state: Default::default(),
            base_model_dropdown,
            context_window_slider_state,
            context_window_editor,
            last_synced_context_window_editor_value,
            dragged_context_window_value: None,
            coding_model_dropdown,
            full_terminal_use_model_dropdown,
            computer_use_model_dropdown,
            base_reasoning_dropdown,
            full_terminal_reasoning_dropdown,
            computer_use_reasoning_dropdown,
            reasoning_roles: Vec::new(),
            last_model_selections: None,
            permission_mode_dropdown,
            command_denylist_editor,
            command_allowlist_editor,
            command_allowlist_mouse_state_handles: Vec::new(),
            orchestration_provider_checkboxes: Vec::new(),
            all_providers_checkbox: Default::default(),
            command_denylist_mouse_state_handles,
            command_denylist_tooltip_mouse_state_handles: profile_data
                .command_denylist
                .iter()
                .map(|_| Default::default())
                .collect(),
            mcp_denylist_dropdown,
            mcp_denylist_mouse_state_handles,
            profile_name_editor,
            delete_button,
            tooltip_mouse_state_handles: Default::default(),
            plan_auto_sync_switch: Default::default(),
            web_search_switch: Default::default(),
            upgrade_footer_mouse_state: Default::default(),
            model_start_timeout_editor,
            last_synced_model_start_timeout_editor_value,
        };

        ctx.subscribe_to_view(&view.profile_name_editor, |view, _, event, ctx| {
            if let EditorEvent::Edited(_) = event {
                view.save_profile_name_if_valid(ctx);
            }
        });

        ctx.subscribe_to_view(&view.context_window_editor, |view, _, event, ctx| {
            view.handle_context_window_editor_event(event, ctx);
        });

        ctx.subscribe_to_view(&view.command_allowlist_editor, |view, _, event, ctx| {
            if let SubmittableTextInputEvent::Submit(value) = event {
                let Ok(predicate) = AgentModeCommandExecutionPredicate::new_regex(value) else {
                    return;
                };
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
                    profiles.add_to_command_allowlist(view.profile_id, &predicate, ctx);
                });
                ctx.notify();
            }
        });

        ctx.subscribe_to_view(&view.command_denylist_editor, |view, _, event, ctx| {
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
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.add_to_command_denylist(view.profile_id, &predicate, ctx);
                });
                ctx.notify();
            }
        });

        ctx.subscribe_to_view(
            &view.model_start_timeout_editor,
            |view, _, event, ctx| match event {
                EditorEvent::Enter | EditorEvent::Blurred | EditorEvent::Escape => {
                    view.commit_model_start_timeout_editor(ctx);
                }
                _ => {}
            },
        );

        ctx.subscribe_to_model(&LLMPreferences::handle(ctx), |me, _, event, ctx| {
            let permissions = BlocklistAIPermissions::as_ref(ctx);
            let current_permissions = permissions.permissions_profile_for_id(ctx, me.profile_id);

            match event {
                LLMPreferencesEvent::UpdatedAvailableLLMs => {
                    me.update_mouse_state_handles(ctx);
                    Self::refresh_filterable_model_dropdown(
                        &me.base_model_dropdown,
                        current_permissions.base_model.clone(),
                        |prefs, app| prefs.get_base_llm_choices_for_agent_mode(app).collect_vec(),
                        |id| ExecutionProfileEditorViewAction::SetBaseModel { id },
                        |prefs| prefs.get_default_base_model().id.clone(),
                        &me.upgrade_footer_mouse_state,
                        ctx,
                    );
                    Self::refresh_coding_model_dropdown(
                        &me.coding_model_dropdown,
                        current_permissions.coding_model.clone(),
                        ctx,
                    );
                    Self::refresh_filterable_model_dropdown(
                        &me.full_terminal_use_model_dropdown,
                        current_permissions.cli_agent_model.clone(),
                        |prefs, app| prefs.get_cli_agent_llm_choices(app).collect_vec(),
                        |id| ExecutionProfileEditorViewAction::SetFullTerminalUseModel { id },
                        |prefs| prefs.get_default_cli_agent_model().id.clone(),
                        &me.upgrade_footer_mouse_state,
                        ctx,
                    );
                    Self::refresh_filterable_model_dropdown(
                        &me.computer_use_model_dropdown,
                        current_permissions.computer_use_model.clone(),
                        |prefs, app| prefs.get_computer_use_llm_choices(app).collect_vec(),
                        |id| ExecutionProfileEditorViewAction::SetComputerUseModel { id },
                        |prefs| prefs.get_default_computer_use_model().id.clone(),
                        &me.upgrade_footer_mouse_state,
                        ctx,
                    );
                    me.sync_context_window_editor(ctx, false);
                }
                LLMPreferencesEvent::UpdatedActiveAgentModeLLM => {
                    Self::refresh_filterable_model_dropdown(
                        &me.base_model_dropdown,
                        current_permissions.base_model.clone(),
                        |prefs, app| prefs.get_base_llm_choices_for_agent_mode(app).collect_vec(),
                        |id| ExecutionProfileEditorViewAction::SetBaseModel { id },
                        |prefs| prefs.get_default_base_model().id.clone(),
                        &me.upgrade_footer_mouse_state,
                        ctx,
                    );
                    me.sync_context_window_editor(ctx, false);
                }
                LLMPreferencesEvent::UpdatedActiveCodingLLM => {
                    Self::refresh_coding_model_dropdown(
                        &me.coding_model_dropdown,
                        current_permissions.coding_model.clone(),
                        ctx,
                    );
                }
            }
            me.refresh_reasoning_dropdowns(ctx);
        });

        ctx.subscribe_to_model(
            &AIExecutionProfilesModel::handle(ctx),
            |me, _, event, ctx| {
                if matches!(event, AIExecutionProfilesModelEvent::ProfileUpdated(profile_id) if *profile_id == me.profile_id) {
                    me.refresh_profile_state(ctx);
                    me.update_mouse_state_handles(ctx);
                }
            },
        );

        let workspace = UserWorkspaces::handle(ctx);
        ctx.subscribe_to_model(&workspace, |me, _, event, ctx| {
            if let UserWorkspacesEvent::TeamsChanged = event {
                Self::update_all_editor_interaction_states(me, ctx);
                me.update_mouse_state_handles(ctx);
                ctx.notify();
            }
        });
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            if let AISettingsChangedEvent::IsAnyAIEnabled { .. } = event {
                Self::update_all_editor_interaction_states(me, ctx);
                me.last_model_selections = None;
                me.refresh_profile_state(ctx);
                me.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
        });

        Self::update_all_editor_interaction_states(&view, ctx);

        view.refresh_profile_state(ctx);

        view.update_mouse_state_handles(ctx);

        view
    }

    pub fn profile_id(&self) -> ClientProfileId {
        self.profile_id
    }

    fn update_mouse_state_handles(&mut self, ctx: &mut ViewContext<Self>) {
        let app = ctx;
        let permissions = BlocklistAIPermissions::as_ref(app);
        let current_permissions = permissions.permissions_profile_for_id(app, self.profile_id);

        self.command_allowlist_mouse_state_handles = current_permissions
            .command_allowlist
            .iter()
            .map(|_| Default::default())
            .collect();
        let mut names: Vec<_> = LLMPreferences::as_ref(app)
            .get_base_llm_choices_for_agent_mode(app)
            .filter(|model| {
                !matches!(model.id.as_str(), "none" | "placeholder")
                    && !crate::ai::custom_model_routers::is_custom_router_id(model.id.as_str())
            })
            // Cloud catalog descriptions name vendors, not configured connections.
            // Direct registry models already carry the user's connection name.
            .filter(|model| {
                matches!(
                    model.provider,
                    LLMProvider::LiteLLM | LLMProvider::Bedrock | LLMProvider::Acp
                )
            })
            .map(|model| {
                LLMPreferences::as_ref(app)
                    .orchestration_provider_name(model)
                    .to_string()
            })
            .collect();
        names.extend(
            AISettings::as_ref(app)
                .openai_providers
                .value()
                .iter()
                .map(|provider| provider.name.clone()),
        );
        names.retain(|name| !name.trim().is_empty());
        names.sort();
        names.dedup();
        self.orchestration_provider_checkboxes
            .retain(|(name, _)| names.contains(name));
        for name in names {
            if !self
                .orchestration_provider_checkboxes
                .iter()
                .any(|(existing, _)| existing == &name)
            {
                self.orchestration_provider_checkboxes
                    .push((name, Default::default()));
            }
        }
        self.orchestration_provider_checkboxes
            .sort_by(|a, b| a.0.cmp(&b.0));

        self.command_denylist_mouse_state_handles = current_permissions
            .command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();

        self.command_denylist_tooltip_mouse_state_handles = current_permissions
            .command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();

        self.mcp_denylist_mouse_state_handles = current_permissions
            .mcp_denylist
            .iter()
            .map(|_| Default::default())
            .collect();
    }

    fn refresh_profile_state(&mut self, ctx: &mut ViewContext<Self>) {
        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let current_permissions = permissions.permissions_profile_for_id(ctx, self.profile_id);
        let model_selections = [
            current_permissions.base_model.clone(),
            current_permissions.coding_model.clone(),
            current_permissions.cli_agent_model.clone(),
            current_permissions.computer_use_model.clone(),
        ];
        // Name, permissions, and context-limit edits do not change model menus.
        // Catalog updates refresh them independently through LLMPreferences.
        if self.last_model_selections.as_ref() != Some(&model_selections) {
            self.last_model_selections = Some(model_selections);
            Self::refresh_filterable_model_dropdown(
                &self.base_model_dropdown,
                current_permissions.base_model.clone(),
                |prefs, app| prefs.get_base_llm_choices_for_agent_mode(app).collect_vec(),
                |id| ExecutionProfileEditorViewAction::SetBaseModel { id },
                |prefs| prefs.get_default_base_model().id.clone(),
                &self.upgrade_footer_mouse_state,
                ctx,
            );
            Self::refresh_coding_model_dropdown(
                &self.coding_model_dropdown,
                current_permissions.coding_model.clone(),
                ctx,
            );
            Self::refresh_filterable_model_dropdown(
                &self.full_terminal_use_model_dropdown,
                current_permissions.cli_agent_model.clone(),
                |prefs, app| prefs.get_cli_agent_llm_choices(app).collect_vec(),
                |id| ExecutionProfileEditorViewAction::SetFullTerminalUseModel { id },
                |prefs| prefs.get_default_cli_agent_model().id.clone(),
                &self.upgrade_footer_mouse_state,
                ctx,
            );
            Self::refresh_filterable_model_dropdown(
                &self.computer_use_model_dropdown,
                current_permissions.computer_use_model.clone(),
                |prefs, app| prefs.get_computer_use_llm_choices(app).collect_vec(),
                |id| ExecutionProfileEditorViewAction::SetComputerUseModel { id },
                |prefs| prefs.get_default_computer_use_model().id.clone(),
                &self.upgrade_footer_mouse_state,
                ctx,
            );
            self.refresh_reasoning_dropdowns(ctx);
        }

        self.permission_mode_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(
                ExecutionProfileEditorViewAction::SetPermissionMode {
                    mode: current_permissions.permission_mode,
                },
                ctx,
            );
            if AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                dropdown.set_enabled(ctx);
            } else {
                dropdown.set_disabled(ctx);
            }
        });
        Self::refresh_mcp_dropdown(
            &self.mcp_denylist_dropdown,
            |uuid| ExecutionProfileEditorViewAction::AddToMCPDenylist { id: uuid },
            &current_permissions.mcp_denylist,
            ctx,
        );

        Self::update_profile_name_editor(&self.profile_name_editor, &current_permissions, ctx);
        self.sync_context_window_editor(ctx, false);
        ctx.notify();
    }

    fn refresh_reasoning_dropdowns(&mut self, ctx: &mut ViewContext<Self>) {
        let profile =
            BlocklistAIPermissions::as_ref(ctx).permissions_profile_for_id(ctx, self.profile_id);
        self.reasoning_roles.clear();
        for role in [
            ProfileModelRole::Base,
            ProfileModelRole::FullTerminalUse,
            ProfileModelRole::ComputerUse,
        ] {
            let prefs = LLMPreferences::as_ref(ctx);
            let (id, choices, dropdown) = match role {
                ProfileModelRole::Base => (
                    profile
                        .base_model
                        .clone()
                        .unwrap_or_else(|| prefs.get_default_base_model().id.clone()),
                    prefs.get_base_llm_choices_for_agent_mode(ctx).collect_vec(),
                    &self.base_reasoning_dropdown,
                ),
                ProfileModelRole::FullTerminalUse => (
                    profile
                        .cli_agent_model
                        .clone()
                        .unwrap_or_else(|| prefs.get_default_cli_agent_model().id.clone()),
                    prefs.get_cli_agent_llm_choices(ctx).collect_vec(),
                    &self.full_terminal_reasoning_dropdown,
                ),
                ProfileModelRole::ComputerUse => (
                    profile
                        .computer_use_model
                        .clone()
                        .unwrap_or_else(|| prefs.get_default_computer_use_model().id.clone()),
                    prefs.get_computer_use_llm_choices(ctx).collect_vec(),
                    &self.computer_use_reasoning_dropdown,
                ),
            };
            let id = prefs
                .get_llm_info(&id)
                .map_or(id.clone(), |model| model.id.clone());
            let reasoning_choices = reasoning_choices_for_model(&id, &choices);
            let items: Vec<MenuItem<DropdownAction>> = reasoning_choices
                .iter()
                .map(|model| {
                    let mut fields = MenuItemFields::new(
                        model
                            .reasoning_level
                            .clone()
                            .unwrap_or_else(|| "Provider default".to_owned()),
                    )
                    .with_on_select_action(DropdownAction::select_action_and_close(
                        ExecutionProfileEditorViewAction::SetModelReasoning {
                            role,
                            id: model.id.clone(),
                        },
                    ))
                    .with_disabled(model.disable_reason.is_some());
                    if let Some(reason) = &model.disable_reason {
                        fields = fields
                            .with_tooltip(reason.tooltip_text())
                            .with_tooltip_position(MenuTooltipPosition::Above);
                    }
                    fields.into_item()
                })
                .collect();
            if !reasoning_choices.is_empty() {
                self.reasoning_roles.push(role);
            }
            dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_rich_items(items, ctx);
                dropdown.set_selected_by_action(
                    ExecutionProfileEditorViewAction::SetModelReasoning { role, id },
                    ctx,
                );
                if AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    dropdown.set_enabled(ctx);
                } else {
                    dropdown.set_disabled(ctx);
                }
            });
        }
        ctx.notify();
    }

    fn refresh_filterable_model_dropdown<G, A, D>(
        menu: &ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
        profile_model: Option<LLMId>,
        get_choices: G,
        create_action: A,
        get_default_id: D,
        upgrade_mouse_state: &MouseStateHandle,
        ctx: &mut ViewContext<Self>,
    ) where
        G: for<'a> FnOnce(&'a LLMPreferences, &AppContext) -> Vec<&'a LLMInfo>,
        A: Fn(LLMId) -> ExecutionProfileEditorViewAction,
        D: FnOnce(&LLMPreferences) -> LLMId,
    {
        menu.update(ctx, |dropdown, ctx| {
            let disabled_by_ai_toggle = !AISettings::as_ref(ctx).is_any_ai_enabled(ctx);

            if disabled_by_ai_toggle {
                dropdown.set_disabled(ctx);
            } else {
                dropdown.set_enabled(ctx);
            }

            let llm_prefs = LLMPreferences::handle(ctx);
            let llm_prefs = llm_prefs.as_ref(ctx);
            let model_to_select = profile_model.unwrap_or_else(|| get_default_id(llm_prefs));
            let model_to_select = llm_prefs
                .get_llm_info(&model_to_select)
                .map_or(model_to_select.clone(), |model| model.id.clone());
            let choices = collapse_model_reasoning_choices(
                get_choices(llm_prefs, ctx),
                Some(&model_to_select),
            );

            let has_upgrade_gated_models = choices
                .iter()
                .any(|llm| matches!(llm.disable_reason, Some(DisableReason::RequiresUpgrade)));

            let items = available_model_menu_items(
                choices,
                |llm| DropdownAction::select_action_and_close(create_action(llm.id.clone())),
                None,
                None,
                false,
                true,
                ctx,
            );
            dropdown.set_rich_items(items, ctx);

            if has_upgrade_gated_models {
                let mouse_state = upgrade_mouse_state.clone();
                dropdown.set_footer(
                    move |app| render_upgrade_footer(mouse_state.clone(), app),
                    ctx,
                );
            } else {
                dropdown.clear_footer(ctx);
            }

            dropdown.set_selected_by_action(create_action(model_to_select), ctx);
            ctx.notify();
        });
        ctx.notify();
    }

    fn refresh_coding_model_dropdown(
        menu: &ViewHandle<Dropdown<ExecutionProfileEditorViewAction>>,
        profile_coding_model: Option<LLMId>,
        ctx: &mut ViewContext<Self>,
    ) {
        menu.update(ctx, |dropdown, ctx| {
            let disabled_by_ai_toggle = !AISettings::as_ref(ctx).is_any_ai_enabled(ctx);

            if disabled_by_ai_toggle {
                dropdown.set_disabled(ctx);
            } else {
                dropdown.set_enabled(ctx);
            }

            let choices = LLMPreferences::as_ref(ctx)
                .get_coding_llm_choices(ctx)
                .collect_vec();

            let items = available_model_menu_items(
                choices,
                |llm| {
                    DropdownAction::select_action_and_close(
                        ExecutionProfileEditorViewAction::SetCodingModel { id: llm.id.clone() },
                    )
                },
                None,
                None,
                false,
                false,
                ctx,
            );
            dropdown.set_rich_items(items, ctx);

            let model_to_select = profile_coding_model.unwrap_or_else(|| {
                LLMPreferences::as_ref(ctx)
                    .get_default_coding_model()
                    .id
                    .clone()
            });
            dropdown.set_selected_by_action(
                ExecutionProfileEditorViewAction::SetCodingModel {
                    id: model_to_select,
                },
                ctx,
            );
            ctx.notify();
        });
        ctx.notify();
    }

    fn refresh_mcp_dropdown<F>(
        dropdown: &ViewHandle<FilterableDropdown<ExecutionProfileEditorViewAction>>,
        action_creator: F,
        profile_mcp_denylist: &[uuid::Uuid],
        ctx: &mut ViewContext<Self>,
    ) where
        F: Fn(uuid::Uuid) -> ExecutionProfileEditorViewAction,
    {
        let all_mcp_servers = TemplatableMCPServerManager::get_all_cloud_synced_mcp_servers(ctx);
        dropdown.update(ctx, |dropdown, ctx| {
            let mcps_in_dropdown: Vec<(uuid::Uuid, String)> = all_mcp_servers
                .into_iter()
                .filter(|(uuid, _server_name)| !profile_mcp_denylist.contains(uuid))
                .collect();

            dropdown.set_items(
                mcps_in_dropdown
                    .iter()
                    .map(|(uuid, server_name)| {
                        DropdownItem::new(server_name, action_creator(*uuid))
                    })
                    .collect(),
                ctx,
            );
            ctx.notify()
        });
        ctx.notify();
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
    }

    fn save_profile_name_if_valid(&self, ctx: &mut ViewContext<Self>) {
        let new_name = self.profile_name_editor.read(ctx, |editor, ctx| {
            editor.buffer_text(ctx).trim().to_string()
        });

        if new_name.is_empty() {
            return;
        }

        let current_name = BlocklistAIPermissions::as_ref(ctx)
            .permissions_profile_for_id(ctx, self.profile_id)
            .name;

        if current_name != new_name {
            AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                profiles_model.set_profile_name(self.profile_id, &new_name, ctx);
            });
        }
    }

    fn update_profile_name_editor(
        profile_name_editor: &ViewHandle<EditorView>,
        profile_data: &AIExecutionProfile,
        ctx: &mut ViewContext<Self>,
    ) {
        profile_name_editor.update(ctx, |editor, ctx| {
            let display_name = profile_data.display_name();

            // Only update the buffer text if it's different from what's currently displayed
            // This preserves the cursor position when the text hasn't changed
            let current_text = editor.buffer_text(ctx);
            if current_text != display_name {
                editor.set_buffer_text(&display_name, ctx);
            }
        });
    }

    fn update_all_editor_interaction_states(view: &Self, ctx: &mut ViewContext<Self>) {
        let is_any_ai_enabled = AISettings::as_ref(ctx).is_any_ai_enabled(ctx);
        Self::update_editor_interaction_state(
            view.profile_name_editor.clone(),
            is_any_ai_enabled,
            ctx,
        );
        Self::update_editor_interaction_state(
            view.command_denylist_editor.as_ref(ctx).editor().clone(),
            is_any_ai_enabled,
            ctx,
        );
    }

    fn update_editor_interaction_state(
        editor: ViewHandle<EditorView>,
        is_editable: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        editor.update(ctx, |editor, ctx| {
            if !is_editable {
                editor.set_interaction_state(InteractionState::Disabled, ctx);
            } else {
                editor.set_interaction_state(InteractionState::Editable, ctx);
            }
        });
    }

    fn configurable_context_window(&self, app: &AppContext) -> Option<LLMContextWindow> {
        let profile =
            BlocklistAIPermissions::as_ref(app).permissions_profile_for_id(app, self.profile_id);
        profile.configurable_context_window(app)
    }

    fn current_context_window_display_value(&self, app: &AppContext) -> Option<u32> {
        let profile =
            BlocklistAIPermissions::as_ref(app).permissions_profile_for_id(app, self.profile_id);
        profile.context_window_display_value(app)
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
                let Some(cw) = self.configurable_context_window(ctx) else {
                    return;
                };
                let buffer_text = self.context_window_editor.as_ref(ctx).buffer_text(ctx);
                let cleaned: String = buffer_text
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != ',')
                    .collect();
                if let Ok(parsed) = cleaned.parse::<u32>() {
                    let clamped = parsed.clamp(cw.min, cw.max);
                    if Some(clamped) != self.current_context_window_display_value(ctx) {
                        AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                            profiles_model.set_context_window_limit(
                                self.profile_id,
                                Some(clamped),
                                ctx,
                            );
                        });
                    }
                }
                self.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
            _ => {}
        }
    }

    fn sync_context_window_editor(&mut self, ctx: &mut ViewContext<Self>, force: bool) {
        self.dragged_context_window_value = None;
        let Some(value) = self.current_context_window_display_value(ctx) else {
            self.last_synced_context_window_editor_value = None;
            self.context_window_slider_state.reset_offset();
            ctx.notify();
            return;
        };

        let formatted = value.separate_with_commas();
        let should_update = if force {
            true
        } else {
            match self.last_synced_context_window_editor_value {
                Some(last_value) => {
                    self.context_window_editor.as_ref(ctx).buffer_text(ctx)
                        == last_value.separate_with_commas()
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

    /// Parse the editor buffer and dispatch a `SetModelStartTimeout` action.
    /// An empty buffer resets to `None` (default).
    /// Zero, overflow, and non-numeric input are invalid: the buffer is
    /// unconditionally restored to the last persisted value.
    fn commit_model_start_timeout_editor(&mut self, ctx: &mut ViewContext<Self>) {
        let buffer_text = self.model_start_timeout_editor.as_ref(ctx).buffer_text(ctx);
        // Delegate to the pure classifier; Err means invalid input.
        let new_value = match parse_timeout_input(&buffer_text) {
            Ok(value) => value,
            Err(()) => {
                // Zero, overflow, or non-numeric: restore the persisted value.
                self.force_sync_model_start_timeout_editor(ctx);
                return;
            }
        };
        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let current = permissions
            .permissions_profile_for_id(ctx, self.profile_id)
            .model_start_timeout_secs;
        if new_value != current {
            ctx.dispatch_typed_action(&ExecutionProfileEditorViewAction::SetModelStartTimeout {
                value: new_value,
            });
        }
        // Always sync after a valid commit so the buffer reflects the
        // canonical persisted value (e.g. stripped whitespace/commas).
        self.force_sync_model_start_timeout_editor(ctx);
    }

    /// Sync the editor buffer from the persisted profile value,
    /// skipping the update when the buffer already reflects the current value.
    fn sync_model_start_timeout_editor(&mut self, ctx: &mut ViewContext<Self>) {
        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let current = permissions
            .permissions_profile_for_id(ctx, self.profile_id)
            .model_start_timeout_secs;
        // Only skip when both the last-synced value AND the buffer text already
        // match; avoids a redundant redraw but never leaves invalid text in place.
        if self.last_synced_model_start_timeout_editor_value == Some(current) {
            let formatted = current.map(|s| s.to_string()).unwrap_or_default();
            if self.model_start_timeout_editor.as_ref(ctx).buffer_text(ctx) == formatted {
                return;
            }
        }
        self.force_sync_model_start_timeout_editor(ctx);
    }

    /// Unconditionally reset the editor buffer to the current persisted value.
    /// Use this whenever invalid input must be discarded.
    fn force_sync_model_start_timeout_editor(&mut self, ctx: &mut ViewContext<Self>) {
        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let current = permissions
            .permissions_profile_for_id(ctx, self.profile_id)
            .model_start_timeout_secs;
        let formatted = current.map(|s| s.to_string()).unwrap_or_default();
        self.model_start_timeout_editor.update(ctx, |editor, ctx| {
            if editor.buffer_text(ctx) != formatted {
                editor.system_reset_buffer_text(&formatted, ctx);
            }
        });
        self.last_synced_model_start_timeout_editor_value = Some(current);
        ctx.notify();
    }
}

/// Classify raw text from the model startup timeout editor.
///
/// Returns `Ok(None)` for a blank/whitespace input (reset to default),
/// `Ok(Some(secs))` for a valid positive integer,
/// and `Err(())` for zero, non-numeric, or overflow input.
/// This is a pure function extracted for testability.
pub(crate) fn parse_timeout_input(raw: &str) -> Result<Option<u32>, ()> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect();
    if cleaned.is_empty() {
        return Ok(None);
    }
    match cleaned.parse::<u32>() {
        Ok(0) => Err(()),
        Ok(secs) => Ok(Some(secs)),
        Err(_) => Err(()),
    }
}

fn initial_context_window_display_value(
    profile_data: &AIExecutionProfile,
    app: &AppContext,
) -> u32 {
    profile_data
        .context_window_display_value(app)
        .unwrap_or_else(|| {
            LLMPreferences::as_ref(app)
                .get_default_base_model()
                .context_window
                .default_max
        })
}

mod ui_helpers;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

impl View for ExecutionProfileEditorView {
    fn ui_name() -> &'static str {
        "ExecutionProfileEditorView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        use ui_helpers::*;

        let permissions = BlocklistAIPermissions::as_ref(app);
        let profile_data = permissions.permissions_profile_for_id(app, self.profile_id);

        let mut column = Flex::column()
            .with_child(render_header_section(appearance, &self.profile_name_editor))
            .with_child(render_models_section(appearance, self, app))
            .with_child(render_permissions_section(
                appearance,
                self,
                &profile_data,
                app,
            ));

        if !profile_data.is_default_profile {
            column.add_child(ChildView::new(&self.delete_button).finish());
        }

        let content = Container::new(column.finish())
            .with_uniform_padding(16.)
            .finish();

        ClippedScrollable::vertical(
            self.clipped_scroll_state.clone(),
            Align::new(content).top_center().finish(),
            ScrollbarWidth::Auto,
            appearance.theme().nonactive_ui_detail().into(),
            appearance.theme().active_ui_detail().into(),
            galaxyui::elements::Fill::None,
        )
        .finish()
    }
}

impl Entity for ExecutionProfileEditorView {
    type Event = ExecutionProfileEditorViewEvent;
}

impl TypedActionView for ExecutionProfileEditorView {
    type Action = ExecutionProfileEditorViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ExecutionProfileEditorViewAction::Save => {
                // TODO: Implement save logic
                log::info!("Save profile");
            }
            ExecutionProfileEditorViewAction::Close => {
                ctx.emit(ExecutionProfileEditorViewEvent::Pane(PaneEvent::Close));
            }
            ExecutionProfileEditorViewAction::SetBaseModel { id } => {
                // Changing the base model resets any persisted context window
                // override — the new model may have a different range (or not
                // be configurable at all). The user can pick a new value for
                // the new model if they want one.
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_base_model(self.profile_id, Some(id.clone()), ctx);
                    profiles_model.set_context_window_limit(self.profile_id, None, ctx);
                });
                self.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetModelReasoning { role, id } => {
                AIExecutionProfilesModel::handle(ctx).update(
                    ctx,
                    |profiles_model, ctx| match role {
                        ProfileModelRole::Base => {
                            profiles_model.set_base_model(self.profile_id, Some(id.clone()), ctx)
                        }
                        ProfileModelRole::FullTerminalUse => profiles_model.set_cli_agent_model(
                            self.profile_id,
                            Some(id.clone()),
                            ctx,
                        ),
                        ProfileModelRole::ComputerUse => profiles_model.set_computer_use_model(
                            self.profile_id,
                            Some(id.clone()),
                            ctx,
                        ),
                    },
                );
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::ContextWindowSliderDragged { value } => {
                if !AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    self.sync_context_window_editor(ctx, true);
                    return;
                }
                // Transient drag update: reflect the current slider position
                // in the input box without persisting to the profile yet.
                // Persistence happens on SetContextWindowSize (drop / commit).
                if self.configurable_context_window(ctx).is_some() {
                    self.dragged_context_window_value = Some(*value);
                    let formatted = value.separate_with_commas();
                    self.context_window_editor.update(ctx, |editor, ctx| {
                        editor.system_reset_buffer_text(&formatted, ctx);
                    });
                    ctx.notify();
                }
            }
            ExecutionProfileEditorViewAction::SetContextWindowSize { value } => {
                self.dragged_context_window_value = None;
                if !AISettings::as_ref(ctx).is_any_ai_enabled(ctx) {
                    self.sync_context_window_editor(ctx, true);
                    return;
                }
                let Some(cw) = self.configurable_context_window(ctx) else {
                    return;
                };
                let clamped = (*value).clamp(cw.min, cw.max);
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_context_window_limit(self.profile_id, Some(clamped), ctx);
                });
                self.sync_context_window_editor(ctx, true);
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetCodingModel { id } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_coding_model(self.profile_id, Some(id.clone()), ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetFullTerminalUseModel { id } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_cli_agent_model(self.profile_id, Some(id.clone()), ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetComputerUseModel { id } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_computer_use_model(self.profile_id, Some(id.clone()), ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetOrchestrationProviders { providers } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
                    profiles.set_orchestration_providers(self.profile_id, providers.clone(), ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::RemoveFromCommandAllowlist { predicate } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
                    profiles.remove_from_command_allowlist(self.profile_id, predicate, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetPermissionMode { mode } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
                    profiles.set_permission_mode(self.profile_id, *mode, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::AddToCommandDenylist { predicate } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.add_to_command_denylist(self.profile_id, predicate, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::RemoveFromCommandDenylist { predicate } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.remove_from_command_denylist(self.profile_id, predicate, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::AddToMCPDenylist { id } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.add_to_mcp_denylist(self.profile_id, id, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::RemoveFromMCPDenylist { id } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.remove_from_mcp_denylist(self.profile_id, id, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::DeleteProfile => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.delete_profile(self.profile_id, ctx);
                });
                ctx.emit(ExecutionProfileEditorViewEvent::Pane(PaneEvent::Close));
            }
            ExecutionProfileEditorViewAction::SetPlanAutoSync { enabled } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_autosync_plans_to_warp_drive(self.profile_id, *enabled, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetWebSearchEnabled { enabled } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_web_search_enabled(self.profile_id, *enabled, ctx);
                });
                ctx.notify();
            }
            ExecutionProfileEditorViewAction::SetModelStartTimeout { value } => {
                AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles_model, ctx| {
                    profiles_model.set_model_start_timeout_secs(self.profile_id, *value, ctx);
                });
                self.sync_model_start_timeout_editor(ctx);
                ctx.notify();
            }
        }
    }
}

impl BackingView for ExecutionProfileEditorView {
    type PaneHeaderOverflowMenuAction = ExecutionProfileEditorViewAction;
    type CustomAction = ();
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        _action: &Self::PaneHeaderOverflowMenuAction,
        _ctx: &mut galaxyui::ViewContext<Self>,
    ) {
        self.handle_action(_action, _ctx)
    }

    fn close(&mut self, ctx: &mut galaxyui::ViewContext<Self>) {
        self.save_profile_name_if_valid(ctx);
        ctx.emit(ExecutionProfileEditorViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut galaxyui::ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        _app: &AppContext,
    ) -> view::HeaderContent {
        view::HeaderContent::Standard(view::StandardHeader {
            title: HEADER_TEXT.into(),
            title_secondary: None,
            title_style: None,
            title_clip_config: galaxyui::text_layout::ClipConfig::start(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: None,
            options: view::StandardHeaderOptions {
                always_show_icons: true,
                ..Default::default()
            },
        })
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}
