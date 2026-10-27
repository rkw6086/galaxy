use ai::agent::action_result::AIAgentActionResultType;
use futures::future::BoxFuture;
use futures::FutureExt;
use galaxyui::{Entity, ModelContext};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};
use crate::ai::agent::{AIAgentActionType, UseComputerResult};
use crate::features::FeatureFlag;

pub struct UseComputerExecutor;

impl UseComputerExecutor {
    pub fn new() -> Self {
        Self
    }

    pub(super) fn should_autoexecute(
        &self,
        input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentActionType::UseComputer(_) = &action.action else {
            return false;
        };

        // The shared action boundary applies the active permission mode to every action.
        // Once that check passes, the executor has no additional approval requirement.
        true
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentActionType::UseComputer(request) = &action.action else {
            return ActionExecution::InvalidAction;
        };

        let actions = request.actions.clone();
        let screenshot_params = request.screenshot_params;
        // Gate per-window targeting behind the client feature flag. When off, the actor forces the
        // legacy full-screen path so results are identical to the pre-existing implementation.
        let background_enabled = FeatureFlag::BackgroundComputerUse.is_enabled();
        // Actor initialization reads the macOS keyboard layout through Carbon APIs that require
        // the main thread. Construct it here, just like RequestComputerUseExecutor, before
        // moving the Send actor into the background action future.
        #[cfg(target_os = "macos")]
        let mut actor = computer_use::create_actor();
        ActionExecution::new_async(
            async move {
                #[cfg(not(target_os = "macos"))]
                let mut actor = computer_use::create_actor();
                match actor
                    .perform_actions(
                        &actions,
                        computer_use::Options {
                            screenshot_params,
                            background_enabled,
                        },
                    )
                    .await
                {
                    Ok(result) => UseComputerResult::Success(result),
                    Err(error) => UseComputerResult::Error(error),
                }
            },
            |res, _ctx| AIAgentActionResultType::UseComputer(res),
        )
    }

    pub(super) fn preprocess_action(
        &mut self,
        _input: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

impl Entity for UseComputerExecutor {
    type Event = ();
}
