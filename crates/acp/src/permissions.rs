use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SelectedPermissionOutcome, ToolKind,
};
use futures::future::{BoxFuture, FutureExt as _};

/// Galaxy permissions that may be granted to an ACP agent for one turn.
///
/// This deliberately grants only categories marked `AlwaysAllow` by the
/// active Galaxy execution profile. Interactive permissions remain denied
/// until Galaxy can surface the agent's permission choices in its own UI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcpPermissionPolicy {
    /// Permit file and data reads, including searches.
    pub read: bool,
    /// Permit edits, deletes, and moves.
    pub edit: bool,
    /// Permit command and code execution.
    pub execute: bool,
    /// Permit fetching external data.
    pub fetch: bool,
    /// Permit uncategorized tools, including MCP tools.
    pub other: bool,
}

impl AcpPermissionPolicy {
    fn allows(self, kind: ToolKind) -> bool {
        match kind {
            ToolKind::Read | ToolKind::Search => self.read,
            ToolKind::Edit | ToolKind::Delete | ToolKind::Move => self.edit,
            ToolKind::Execute => self.execute,
            ToolKind::Think => true,
            ToolKind::Fetch => self.fetch,
            ToolKind::SwitchMode => false,
            ToolKind::Other => self.other,
            _ => false,
        }
    }
}

/// Information supplied to Galaxy's permission hook.
#[derive(Clone, Debug)]
pub struct PermissionContext {
    /// The original ACP permission request.
    pub request: RequestPermissionRequest,
    /// Whether this turn was explicitly launched in Galaxy's autonomous
    /// execution mode.
    pub auto_approve: bool,
    /// Category permissions inherited from the active Galaxy profile.
    pub policy: AcpPermissionPolicy,
}

/// A decision returned by a [`PermissionHandler`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PermissionDecision {
    /// Select the first one-shot (then persistent) allow option.
    Allow,
    /// Select the first one-shot (then persistent) reject option.
    Deny,
    /// Select a specific option advertised by the agent.
    Select(PermissionOptionId),
    /// Report that the permission interaction was cancelled.
    Cancel,
}

/// Host hook used to resolve ACP permission requests.
pub trait PermissionHandler: Send + Sync {
    /// Returns a permission decision without blocking the ACP dispatch loop.
    fn decide(&self, context: PermissionContext) -> BoxFuture<'static, PermissionDecision>;
}

/// Safe default permission hook.
///
/// Requests are rejected unless the individual turn explicitly opts into
/// automatic approval.
#[derive(Debug, Default)]
pub struct DenyByDefaultPermissionHandler;

impl PermissionHandler for DenyByDefaultPermissionHandler {
    fn decide(&self, context: PermissionContext) -> BoxFuture<'static, PermissionDecision> {
        async move {
            let kind = context.request.tool_call.fields.kind.unwrap_or_default();
            if context.auto_approve || context.policy.allows(kind) {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny
            }
        }
        .boxed()
    }
}

pub(crate) fn outcome_for_decision(
    request: &RequestPermissionRequest,
    decision: PermissionDecision,
) -> RequestPermissionOutcome {
    let selected = match decision {
        PermissionDecision::Allow => request
            .options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .map(|option| option.option_id.clone()),
        PermissionDecision::Deny => request
            .options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::RejectOnce)
            .or_else(|| {
                request
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::RejectAlways)
            })
            .map(|option| option.option_id.clone()),
        PermissionDecision::Select(option_id) => request
            .options
            .iter()
            .find(|option| option.option_id == option_id)
            .map(|option| option.option_id.clone()),
        PermissionDecision::Cancel => None,
    };

    selected.map_or(RequestPermissionOutcome::Cancelled, |option_id| {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
    })
}

pub(crate) type SharedPermissionHandler = Arc<dyn PermissionHandler>;

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
