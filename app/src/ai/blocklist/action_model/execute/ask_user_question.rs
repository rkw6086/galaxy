use ai::agent::action_result::{AskUserQuestionAnswerItem, AskUserQuestionResult};
use futures::future::BoxFuture;
use futures::FutureExt;
use galaxyui::{Entity, EntityId, ModelContext, SingletonEntity};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};
use crate::ai::agent::{AIAgentActionResultType, AIAgentActionType};
use crate::ai::blocklist::BlocklistAIPermissions;

pub enum AskUserQuestionDecision {
    Completed(Vec<AskUserQuestionAnswerItem>),
    Cancelled,
}

pub struct AskUserQuestionExecutor {
    result_rx: (
        async_channel::Sender<AskUserQuestionDecision>,
        async_channel::Receiver<AskUserQuestionDecision>,
    ),
    terminal_view_id: EntityId,
}

impl AskUserQuestionExecutor {
    pub fn new(terminal_view_id: EntityId) -> Self {
        Self {
            result_rx: async_channel::unbounded(),
            terminal_view_id,
        }
    }

    pub(super) fn should_autoexecute(
        &self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        !BlocklistAIPermissions::as_ref(ctx).can_ask_user_question(
            &input.conversation_id,
            Some(self.terminal_view_id),
            ctx,
        )
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let questions = match &input.action.action {
            AIAgentActionType::AskUserQuestion { questions } => questions,
            _ => {
                return ActionExecution::InvalidAction;
            }
        };

        // For child agent conversations, route the question to the parent
        // for silent auto-answer instead of presenting UI to the user.
        if let Some(parent_conversation_id) =
            self.parent_conversation_id(input.conversation_id, ctx)
        {
            let question_text = questions
                .iter()
                .map(|q| q.question.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let options: Vec<String> = questions
                .iter()
                .filter_map(|q| q.multiple_choice_options())
                .flatten()
                .map(|o| o.label.clone())
                .collect();

            OrchestrationEventService::handle(ctx).update(ctx, |service, ctx| {
                service.route_subagent_question_to_parent(
                    input.conversation_id,
                    parent_conversation_id,
                    question_text,
                    options,
                    0,
                    ctx,
                );
            });

            // Wait for the parent's answer to arrive via the same channel
            let receiver = self.result_rx.1.clone();
            return ActionExecution::new_async(
                async move { receiver.recv().await },
                |result, _ctx| match result {
                    Ok(AskUserQuestionDecision::Completed(answers)) => {
                        AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Success {
                            answers,
                        })
                    }
                    Ok(AskUserQuestionDecision::Cancelled) | Err(_) => {
                        AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Cancelled)
                    }
                },
            );
        }

        if self.should_autoexecute(input, ctx) {
            let question_ids = questions
                .iter()
                .map(|question| question.question_id.clone())
                .collect();
            return ActionExecution::Sync(AIAgentActionResultType::AskUserQuestion(
                AskUserQuestionResult::SkippedByAutoApprove { question_ids },
            ));
        }

        let receiver = self.result_rx.1.clone();
        ActionExecution::new_async(
            async move { receiver.recv().await },
            |result, _ctx| match result {
                Ok(AskUserQuestionDecision::Completed(answers)) => {
                    AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Success {
                        answers,
                    })
                }
                Ok(AskUserQuestionDecision::Cancelled) => {
                    AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Cancelled)
                }
                Err(_) => {
                    AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Cancelled)
                }
            },
        )
    }

    fn parent_conversation_id(
        &self,
        conversation_id: crate::ai::agent::conversation::AIConversationId,
        ctx: &ModelContext<Self>,
    ) -> Option<crate::ai::agent::conversation::AIConversationId> {
        let history_model = BlocklistAIHistoryModel::as_ref(ctx);
        let conversation = history_model.conversation(&conversation_id)?;
        conversation.parent_conversation_id()
    }

    pub(super) fn preprocess_action(
        &mut self,
        _action: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }

    pub fn complete(&self, answers: Vec<AskUserQuestionAnswerItem>) {
        let _ = self
            .result_rx
            .0
            .try_send(AskUserQuestionDecision::Completed(answers));
    }

    pub fn cancel(&self) {
        let _ = self
            .result_rx
            .0
            .try_send(AskUserQuestionDecision::Cancelled);
    }
}

#[cfg(test)]
impl Default for AskUserQuestionExecutor {
    fn default() -> Self {
        Self::new(EntityId::new())
    }
}

impl Entity for AskUserQuestionExecutor {
    type Event = ();
}

#[cfg(test)]
#[path = "ask_user_question_tests.rs"]
mod tests;
