use std::path::PathBuf;

use ai::LLMId;
use cloud_objects::cloud_object::{
    GenericCloudObject, GenericServerObject, GenericStringModel, JsonObjectType,
};
use cloud_objects::ids::GenericStringObjectId;
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use warp_core::channel::ChannelState;
use warp_core::features::FeatureFlag;

use crate::{JsonModel, JsonSerializer};

pub const PROFILE_NAME_MAX_LENGTH: usize = 50;

/// One permission policy for every Galaxy-owned tool in a profile.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    #[default]
    AskFirst,
    GalaxyDecides,
    AllowAll,
    #[serde(other)]
    ReadOnly,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read Only",
            Self::AskFirst => "Ask First",
            Self::GalaxyDecides => "Galaxy Decides",
            Self::AllowAll => "Allow All",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::AllowAll => "Run tools without approval or model review. Explicit denylists and workspace restrictions still apply.",
            Self::ReadOnly => {
                "Only file reads, code searches, grep, and file discovery are available. Commands, edits, MCP, computer use, and agents are disabled, except explicitly allowlisted shell commands."
            }
            Self::AskFirst => {
                "Ask before taking action. Approved commands are remembered for this profile and working directory; changed commands require approval again."
            }
            Self::GalaxyDecides => {
                "Your terminal-use model reviews actions. Safe actions run automatically; risky or unassessable actions are denied without asking you. The agent can revise its request or choose a safer approach."
            }
        }
    }

    pub fn action_permission(self) -> ActionPermission {
        match self {
            Self::ReadOnly => ActionPermission::Unknown,
            Self::AskFirst => ActionPermission::AlwaysAsk,
            Self::GalaxyDecides => ActionPermission::GalaxyDecides,
            Self::AllowAll => ActionPermission::AlwaysAllow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionPermission {
    AlwaysAllow,
    AlwaysAsk,
    /// Independently assess each action before allowing execution.
    #[serde(alias = "AgentDecides")]
    GalaxyDecides,
    // This is intended to catch deserialization errors whenever we add new variants to this enum. Say we
    // want to add a "Never" variant. Without this catch-all, old clients wouldn't be able to deserialize
    // a "Never" into one of the existing options.
    #[serde(other)]
    Unknown,
}

impl ActionPermission {
    pub fn description(&self) -> &'static str {
        match self {
            ActionPermission::GalaxyDecides => {
                "Galaxy reviews each action with your terminal-use model. Risk 0–3 runs automatically; risk 4–5 is denied without asking you."
            }
            ActionPermission::Unknown => "Unknown setting.",
            ActionPermission::AlwaysAllow => {
                "Give the Agent full autonomy  — no manual approval ever required."
            }
            ActionPermission::AlwaysAsk => {
                "Require explicit approval before the Agent takes any action."
            }
        }
    }

    pub fn is_always_ask(&self) -> bool {
        matches!(self, Self::AlwaysAsk)
    }

    pub fn is_always_allow(&self) -> bool {
        matches!(self, Self::AlwaysAllow)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteToPtyPermission {
    // This is for backwards compatibility with the old "Never" value.
    #[serde(alias = "Never")]
    AlwaysAllow,
    #[default]
    AlwaysAsk,
    AskOnFirstWrite,
    /// Independently assess each action before allowing execution.
    GalaxyDecides,
    // This is intended to catch deserialization errors whenever we add new variants to this enum.
    #[serde(other)]
    Unknown,
}

impl WriteToPtyPermission {
    pub fn description(&self) -> &'static str {
        match self {
            WriteToPtyPermission::GalaxyDecides => {
                "Galaxy reviews each action with your terminal-use model. Risk 0–3 runs automatically; risk 4–5 is denied without asking you."
            }
            WriteToPtyPermission::AlwaysAllow => ActionPermission::AlwaysAllow.description(),
            WriteToPtyPermission::AskOnFirstWrite => {
                "The agent will ask for permission the first time it needs to interact with a running command. After that, it will continue automatically for the rest of that command."
            }
            WriteToPtyPermission::AlwaysAsk => {
                "The agent will always ask for permission to interact with a running command."
            }
            WriteToPtyPermission::Unknown => ActionPermission::Unknown.description(),
        }
    }

    pub fn is_always_allow(&self) -> bool {
        matches!(self, Self::AlwaysAllow)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputerUsePermission {
    #[default]
    Never,
    AlwaysAsk,
    AlwaysAllow,
    /// Independently assess each action before allowing execution.
    GalaxyDecides,
    // This is intended to catch deserialization errors whenever we add new variants to this enum.
    #[serde(other)]
    Unknown,
}

impl ComputerUsePermission {
    pub fn description(&self) -> &'static str {
        match self {
            ComputerUsePermission::GalaxyDecides => {
                "Galaxy reviews each action with your terminal-use model. Risk 0–3 runs automatically; risk 4–5 is denied without asking you."
            }
            ComputerUsePermission::Never => {
                "Computer use tools are disabled and will not be available to the Agent."
            }
            ComputerUsePermission::AlwaysAsk => {
                "Require explicit approval before the Agent uses computer use tools."
            }
            ComputerUsePermission::AlwaysAllow => {
                "Give the Agent full autonomy to use computer use tools without approval."
            }
            ComputerUsePermission::Unknown => "Unknown setting.",
        }
    }

    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Never | Self::Unknown)
    }

    pub fn is_always_allow(&self) -> bool {
        matches!(self, Self::AlwaysAllow)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunAgentsPermission {
    NeverAllow,
    AlwaysAllow,
    #[default]
    AlwaysAsk,

    /// Independently assess each action before allowing execution.
    GalaxyDecides,
    // This is intended to catch deserialization errors whenever we add new variants to this enum.
    #[serde(other)]
    Unknown,
}

impl RunAgentsPermission {
    pub fn description(&self) -> &'static str {
        match self {
            RunAgentsPermission::GalaxyDecides => {
                "Galaxy reviews each action with your terminal-use model. Risk 0–3 runs automatically; risk 4–5 is denied without asking you."
            }
            RunAgentsPermission::NeverAllow => {
                "The Agent cannot run child agents and the run_agents tool will not be available."
            }
            RunAgentsPermission::AlwaysAllow => {
                "Give the Agent full autonomy to run child agents without approval."
            }
            RunAgentsPermission::AlwaysAsk => {
                "Require explicit approval before the Agent runs child agents."
            }
            RunAgentsPermission::Unknown => "Unknown setting.",
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(
            self,
            Self::AlwaysAllow | Self::AlwaysAsk | Self::GalaxyDecides
        )
    }

    pub fn is_always_allow(&self) -> bool {
        matches!(self, Self::AlwaysAllow)
    }

    pub fn is_never_allow(&self) -> bool {
        matches!(self, Self::NeverAllow | Self::Unknown)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AskUserQuestionPermission {
    /// Never pause; skip questions and continue with best judgment.
    Never,
    /// Pause and wait for the user, unless auto-approve mode is enabled.
    AskExceptInAutoApprove,
    /// Always pause and wait for the user to answer before continuing, even in auto-approve mode.
    #[default]
    AlwaysAsk,

    // This is intended to catch deserialization errors whenever we add new variants to this enum.
    #[serde(other)]
    Unknown,
}

impl AskUserQuestionPermission {
    pub fn label(&self) -> &'static str {
        match self {
            AskUserQuestionPermission::Never => "Never ask",
            AskUserQuestionPermission::AskExceptInAutoApprove => "Ask unless auto-approve",
            AskUserQuestionPermission::AlwaysAsk | AskUserQuestionPermission::Unknown => {
                "Always ask"
            }
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            AskUserQuestionPermission::AskExceptInAutoApprove
            | AskUserQuestionPermission::Unknown => {
                "The Agent may ask a question and pause for your response, but will continue automatically when auto-approve is on."
            }
            AskUserQuestionPermission::Never => {
                "The Agent will not ask questions and will continue with its best judgment."
            }
            AskUserQuestionPermission::AlwaysAsk => {
                "The Agent may ask a question and will pause for your response even when auto-approve is on."
            }
        }
    }
}

/// Predicate types to match commands that can be executed by Agent Mode.
#[derive(Debug, Serialize, Deserialize, Clone)]
enum AgentModeCommandExecutionPredicateType {
    /// A regex with start (`^`) and end (`$`) anchors.
    ///
    /// We want regex rules to apply to the entire cmd string so we anchor them
    /// (there isn't any efficient way to apply to the entire cmd string at match-time).
    #[serde(with = "serde_regex")]
    AnchoredRegex(Regex),
}

impl AgentModeCommandExecutionPredicateType {
    fn new_regex(regex: &str) -> Result<Self, regex::Error> {
        // Redundant anchors aren't a problem so we can unconditionally add them.
        let anchored_regex = Regex::new(&format!("^{regex}$"))?;
        Ok(Self::AnchoredRegex(anchored_regex))
    }

    fn matches(&self, cmd: &str) -> bool {
        match self {
            Self::AnchoredRegex(regex) => regex.is_match(cmd),
        }
    }
}

impl PartialEq for AgentModeCommandExecutionPredicateType {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AnchoredRegex(a), Self::AnchoredRegex(b)) => {
                // Indexing should be safe since they're guaranteed to have at least
                // the anchors around them.
                let a_unanchored = &a.as_str()[1..a.as_str().len() - 1];
                let b_unanchored = &b.as_str()[1..b.as_str().len() - 1];
                a_unanchored == b_unanchored
            }
        }
    }
}

impl std::fmt::Display for AgentModeCommandExecutionPredicateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnchoredRegex(regex) => {
                write!(f, "{}", &regex.as_str()[1..regex.as_str().len() - 1])
            }
        }
    }
}

/// A wrapper around [`AgentModeCommandExecutionPredicateType`] to enforce
/// the use of the provided constructors rather than direct construction of the variants.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(transparent)]
pub struct AgentModeCommandExecutionPredicate(AgentModeCommandExecutionPredicateType);

impl schemars::JsonSchema for AgentModeCommandExecutionPredicate {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AgentModeCommandExecutionPredicate")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        generator.subschema_for::<String>()
    }
}

impl AgentModeCommandExecutionPredicate {
    pub fn new_regex(regex: &str) -> Result<Self, regex::Error> {
        Ok(Self(AgentModeCommandExecutionPredicateType::new_regex(
            regex,
        )?))
    }

    pub fn matches(&self, cmd: &str) -> bool {
        self.0.matches(cmd)
    }
}

impl std::fmt::Display for AgentModeCommandExecutionPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl settings_value::SettingsValue for AgentModeCommandExecutionPredicate {
    fn to_file_value(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_string())
    }

    fn from_file_value(value: &serde_json::Value) -> Option<Self> {
        value.as_str().and_then(|s| Self::new_regex(s).ok())
    }
}

lazy_static! {
    static ref OPTIONAL_ARGS_REGEX: Regex =
        Regex::new(r"(\s.*)?").expect("Can parse optional args regex");
}

cfg_if::cfg_if! {
    if #[cfg(test)] {
        lazy_static! {
            // Compiling the regexes for the default command execution allowlist/denylist can be slow
            // in an unoptimized build, so we use empty lists in unit tests.
            pub static ref DEFAULT_COMMAND_EXECUTION_ALLOWLIST: Vec<AgentModeCommandExecutionPredicate> = vec![];
            pub static ref DEFAULT_COMMAND_EXECUTION_DENYLIST: Vec<AgentModeCommandExecutionPredicate> = vec![];
        }
    } else {
        lazy_static! {
            pub static ref DEFAULT_COMMAND_EXECUTION_ALLOWLIST: Vec<AgentModeCommandExecutionPredicate> = vec![
                AgentModeCommandExecutionPredicate::new_regex(&format!("cat{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default cat rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("echo{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default echo rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex("find .*").expect("Can parse default find rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("grep{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default grep rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("ls{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default ls rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex("which .*").expect("Can parse default which rule into regex"),
            ];

            pub static ref DEFAULT_COMMAND_EXECUTION_DENYLIST: Vec<AgentModeCommandExecutionPredicate> = vec![
                AgentModeCommandExecutionPredicate::new_regex(&format!("bash{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default bash rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("fish{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default fish rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("pwsh{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default pwsh rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("sh{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default sh rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("zsh{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default zsh rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("curl{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default curl rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("eval{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default eval rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("exec{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default exec rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("source{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default source rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("wget{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default wget rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("dig{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default dig rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("nslookup{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default nslookup rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("host{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default host rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("ssh{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default ssh rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("scp{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default scp rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("rsync{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default rsync rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("telnet{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default telnet rule into regex"),
                AgentModeCommandExecutionPredicate::new_regex(&format!("rm{}", OPTIONAL_ARGS_REGEX.as_str())).expect("Can parse default rm rule into regex"),
            ];
        }
    }
}

/// Core data structure representing an AI execution profile, which includes model configuration,
/// behavior settings, and permissions.
///
/// NOTE: `planning_model` was removed after planning via subagent was deprecated; serialized legacy
/// profiles may include a `planning_model` field and this field name should remain reserved
/// indefinitely.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AIExecutionProfile {
    pub name: String,
    pub permission_mode: PermissionMode,
    /// Digests of exact approved commands and their execution environment.
    pub approved_command_hashes: Vec<String>,
    /// None includes all providers; an empty list explicitly permits none.
    pub orchestration_providers: Option<Vec<String>>,
    pub is_default_profile: bool,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub apply_code_diffs: ActionPermission,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub read_files: ActionPermission,

    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub execute_commands: ActionPermission,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub write_to_pty: WriteToPtyPermission,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub mcp_permissions: ActionPermission,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub ask_user_question: AskUserQuestionPermission,
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub run_agents: RunAgentsPermission,

    /// Always ask for permission for these commands
    pub command_denylist: Vec<AgentModeCommandExecutionPredicate>,

    /// Explicit command approvals that apply in every permission mode. Denylists win.
    pub command_allowlist: Vec<AgentModeCommandExecutionPredicate>,

    /// When the read_files is set to AlwaysAsk, autoread from these directories
    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub directory_allowlist: Vec<PathBuf>,

    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub mcp_allowlist: Vec<uuid::Uuid>,
    pub mcp_denylist: Vec<uuid::Uuid>,

    // Legacy transport projection; permission_mode is authoritative.
    #[serde(skip_serializing)]
    pub computer_use: ComputerUsePermission,

    pub base_model: Option<LLMId>,
    pub coding_model: Option<LLMId>,
    pub cli_agent_model: Option<LLMId>,
    pub computer_use_model: Option<LLMId>,

    pub context_window_limit: Option<u32>,

    /// Whether plans created by the agent should be automatically synced to Warp Drive
    pub autosync_plans_to_warp_drive: bool,

    /// Whether the agent may use web search when helpful for completing tasks
    pub web_search_enabled: bool,
}

/// Reads old profiles once; legacy per-tool permissions are never serialized again.
#[derive(Deserialize)]
#[serde(default)]
struct PersistedExecutionProfile {
    permission_mode: Option<PermissionMode>,
    approved_command_hashes: Vec<String>,
    orchestration_providers: Option<Vec<String>>,
    name: String,
    is_default_profile: bool,
    apply_code_diffs: ActionPermission,
    read_files: ActionPermission,
    execute_commands: ActionPermission,
    write_to_pty: WriteToPtyPermission,
    mcp_permissions: ActionPermission,
    ask_user_question: AskUserQuestionPermission,
    run_agents: RunAgentsPermission,
    command_denylist: Vec<AgentModeCommandExecutionPredicate>,
    command_allowlist: Vec<AgentModeCommandExecutionPredicate>,
    directory_allowlist: Vec<PathBuf>,
    mcp_allowlist: Vec<uuid::Uuid>,
    mcp_denylist: Vec<uuid::Uuid>,
    computer_use: ComputerUsePermission,
    base_model: Option<LLMId>,
    coding_model: Option<LLMId>,
    cli_agent_model: Option<LLMId>,
    computer_use_model: Option<LLMId>,
    context_window_limit: Option<u32>,
    autosync_plans_to_warp_drive: bool,
    web_search_enabled: bool,
}

impl Default for PersistedExecutionProfile {
    fn default() -> Self {
        let profile = AIExecutionProfile::default();
        Self {
            permission_mode: None,
            approved_command_hashes: Vec::new(),
            orchestration_providers: None,
            name: profile.name,
            is_default_profile: profile.is_default_profile,
            apply_code_diffs: profile.apply_code_diffs,
            read_files: profile.read_files,
            execute_commands: profile.execute_commands,
            write_to_pty: profile.write_to_pty,
            mcp_permissions: profile.mcp_permissions,
            ask_user_question: profile.ask_user_question,
            run_agents: profile.run_agents,
            command_denylist: profile.command_denylist,
            command_allowlist: profile.command_allowlist,
            directory_allowlist: profile.directory_allowlist,
            mcp_allowlist: profile.mcp_allowlist,
            mcp_denylist: profile.mcp_denylist,
            computer_use: profile.computer_use,
            base_model: profile.base_model,
            coding_model: profile.coding_model,
            cli_agent_model: profile.cli_agent_model,
            computer_use_model: profile.computer_use_model,
            context_window_limit: profile.context_window_limit,
            autosync_plans_to_warp_drive: profile.autosync_plans_to_warp_drive,
            web_search_enabled: profile.web_search_enabled,
        }
    }
}

impl<'de> Deserialize<'de> for AIExecutionProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let saved = PersistedExecutionProfile::deserialize(deserializer)?;
        let permission_mode = saved
            .permission_mode
            .unwrap_or(match saved.execute_commands {
                ActionPermission::AlwaysAllow | ActionPermission::GalaxyDecides => {
                    PermissionMode::GalaxyDecides
                }
                ActionPermission::AlwaysAsk => PermissionMode::AskFirst,
                ActionPermission::Unknown => PermissionMode::ReadOnly,
            });
        let mut profile = Self {
            permission_mode,
            approved_command_hashes: saved.approved_command_hashes,
            orchestration_providers: saved.orchestration_providers,
            name: saved.name,
            is_default_profile: saved.is_default_profile,
            apply_code_diffs: saved.apply_code_diffs,
            read_files: saved.read_files,
            execute_commands: saved.execute_commands,
            write_to_pty: saved.write_to_pty,
            mcp_permissions: saved.mcp_permissions,
            ask_user_question: saved.ask_user_question,
            run_agents: saved.run_agents,
            command_denylist: saved.command_denylist,
            command_allowlist: saved.command_allowlist,
            directory_allowlist: saved.directory_allowlist,
            mcp_allowlist: saved.mcp_allowlist,
            mcp_denylist: saved.mcp_denylist,
            computer_use: saved.computer_use,
            base_model: saved.base_model,
            coding_model: saved.coding_model,
            cli_agent_model: saved.cli_agent_model,
            computer_use_model: saved.computer_use_model,
            context_window_limit: saved.context_window_limit,
            autosync_plans_to_warp_drive: saved.autosync_plans_to_warp_drive,
            web_search_enabled: saved.web_search_enabled,
        };
        profile.project_permission_mode();
        // Bound restored approvals as well as newly granted ones.
        if profile.approved_command_hashes.len() > 256 {
            profile
                .approved_command_hashes
                .drain(..profile.approved_command_hashes.len() - 256);
        }
        Ok(profile)
    }
}

impl Default for AIExecutionProfile {
    fn default() -> Self {
        Self {
            name: Default::default(),
            is_default_profile: false,
            permission_mode: PermissionMode::AskFirst,
            approved_command_hashes: Vec::new(),
            orchestration_providers: None,
            apply_code_diffs: ActionPermission::AlwaysAsk,
            read_files: ActionPermission::AlwaysAllow,
            execute_commands: ActionPermission::AlwaysAsk,
            write_to_pty: WriteToPtyPermission::AlwaysAsk,
            mcp_permissions: ActionPermission::AlwaysAsk,
            ask_user_question: AskUserQuestionPermission::AlwaysAsk,
            run_agents: RunAgentsPermission::AlwaysAsk,
            command_denylist: Vec::new(),
            command_allowlist: Vec::new(),
            directory_allowlist: Vec::new(),
            mcp_allowlist: Vec::new(),
            mcp_denylist: Vec::new(),
            computer_use: ComputerUsePermission::AlwaysAsk,
            base_model: None,
            coding_model: None,
            cli_agent_model: None,
            computer_use_model: None,
            context_window_limit: None,
            autosync_plans_to_warp_drive: true,
            web_search_enabled: true,
        }
    }
}

impl AIExecutionProfile {
    pub fn includes_orchestration_provider(&self, name: &str) -> bool {
        self.orchestration_providers.as_ref().is_none_or(|providers| providers.iter().any(|provider| provider == name))
    }

    /// Adapter for legacy providers that still consume per-tool permission fields.
    pub fn project_permission_mode(&mut self) {
        let permission = self.permission_mode.action_permission();
        self.apply_code_diffs = permission;
        self.execute_commands = permission;
        self.mcp_permissions = permission;
        self.read_files = if self.permission_mode == PermissionMode::GalaxyDecides {
            ActionPermission::GalaxyDecides
        } else {
            ActionPermission::AlwaysAllow
        };
        self.write_to_pty = match self.permission_mode {
            PermissionMode::GalaxyDecides => WriteToPtyPermission::GalaxyDecides,
            PermissionMode::AllowAll => WriteToPtyPermission::AlwaysAllow,
            PermissionMode::ReadOnly | PermissionMode::AskFirst => WriteToPtyPermission::AlwaysAsk,
        };
        self.computer_use = match self.permission_mode {
            PermissionMode::ReadOnly => ComputerUsePermission::Never,
            PermissionMode::AskFirst => ComputerUsePermission::AlwaysAsk,
            PermissionMode::GalaxyDecides => ComputerUsePermission::GalaxyDecides,
            PermissionMode::AllowAll => ComputerUsePermission::AlwaysAllow,
        };
        self.run_agents = match self.permission_mode {
            PermissionMode::ReadOnly => RunAgentsPermission::NeverAllow,
            PermissionMode::AskFirst => RunAgentsPermission::AlwaysAsk,
            PermissionMode::GalaxyDecides => RunAgentsPermission::GalaxyDecides,
            PermissionMode::AllowAll => RunAgentsPermission::AlwaysAllow,
        };
        // Allow All bypasses tool approval, while clarification can still need user input.
        self.ask_user_question = match self.permission_mode {
            PermissionMode::AskFirst | PermissionMode::AllowAll => {
                AskUserQuestionPermission::AlwaysAsk
            }
            PermissionMode::ReadOnly | PermissionMode::GalaxyDecides => {
                AskUserQuestionPermission::Never
            }
        };
        self.directory_allowlist.clear();
        self.mcp_allowlist.clear();
    }

    #[cfg(feature = "agent_mode_evals")]
    pub fn create_agent_mode_eval_profile() -> Self {
        Self {
            name: "Agent Mode Eval".to_string(),
            is_default_profile: false,
            permission_mode: PermissionMode::GalaxyDecides,
            approved_command_hashes: Vec::new(),
            orchestration_providers: None,
            apply_code_diffs: ActionPermission::AlwaysAllow,
            read_files: ActionPermission::AlwaysAllow,
            execute_commands: ActionPermission::AlwaysAllow,
            write_to_pty: WriteToPtyPermission::AlwaysAllow,
            mcp_permissions: ActionPermission::AlwaysAllow,
            ask_user_question: AskUserQuestionPermission::Never,
            run_agents: RunAgentsPermission::AlwaysAllow,
            command_denylist: Vec::new(),
            command_allowlist: Vec::new(),
            directory_allowlist: Vec::new(),
            mcp_allowlist: Vec::new(),
            mcp_denylist: Vec::new(),
            computer_use: ComputerUsePermission::Never,
            base_model: None,
            coding_model: None,
            cli_agent_model: None,
            computer_use_model: None,
            context_window_limit: None,
            autosync_plans_to_warp_drive: false,
            web_search_enabled: true,
        }
    }

    /// This creates a CLI-specific profile that will never ask the user for permission,
    /// since we cannot do so in a non-interactive setting.
    pub fn create_default_cli_profile(
        is_sandboxed: bool,
        computer_use_override: Option<bool>,
    ) -> Self {
        let command_denylist = if is_sandboxed {
            Vec::new()
        } else {
            DEFAULT_COMMAND_EXECUTION_DENYLIST.to_vec()
        };

        let computer_use_permission = match computer_use_override {
            Some(true) => {
                if is_sandboxed || FeatureFlag::LocalComputerUse.is_enabled() {
                    ComputerUsePermission::AlwaysAllow
                } else {
                    ComputerUsePermission::Never
                }
            }
            Some(false) => ComputerUsePermission::Never,
            None => {
                if is_sandboxed && ChannelState::channel().is_dogfood() {
                    ComputerUsePermission::AlwaysAllow
                } else {
                    ComputerUsePermission::Never
                }
            }
        };

        Self {
            name: "Default (CLI)".to_owned(),
            is_default_profile: true,
            permission_mode: PermissionMode::GalaxyDecides,
            approved_command_hashes: Vec::new(),
            orchestration_providers: None,
            apply_code_diffs: ActionPermission::AlwaysAllow,
            read_files: ActionPermission::AlwaysAllow,
            execute_commands: ActionPermission::AlwaysAllow,
            mcp_permissions: ActionPermission::AlwaysAllow,
            write_to_pty: WriteToPtyPermission::AlwaysAllow,
            ask_user_question: AskUserQuestionPermission::Never,
            run_agents: RunAgentsPermission::AlwaysAllow,
            command_denylist,
            command_allowlist: DEFAULT_COMMAND_EXECUTION_ALLOWLIST.to_vec(),
            directory_allowlist: Vec::new(),
            mcp_allowlist: Vec::new(),
            mcp_denylist: Vec::new(),
            computer_use: computer_use_permission,
            base_model: None,
            coding_model: None,
            cli_agent_model: None,
            computer_use_model: None,
            context_window_limit: None,
            autosync_plans_to_warp_drive: FeatureFlag::SyncAmbientPlans.is_enabled(),
            web_search_enabled: true,
        }
    }
}

impl JsonModel for AIExecutionProfile {
    fn json_object_type() -> JsonObjectType {
        JsonObjectType::AIExecutionProfile
    }
}

pub type CloudAIExecutionProfile =
    GenericCloudObject<GenericStringObjectId, CloudAIExecutionProfileModel>;
pub type CloudAIExecutionProfileModel = GenericStringModel<AIExecutionProfile, JsonSerializer>;
pub type ServerAIExecutionProfile =
    GenericServerObject<GenericStringObjectId, CloudAIExecutionProfileModel>;

#[cfg(test)]
#[path = "ai_execution_profile_tests.rs"]
mod tests;
