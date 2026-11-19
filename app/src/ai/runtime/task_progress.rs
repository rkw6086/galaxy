//! Task state is kept outside the compactable transcript and projected only from
//! explicit, validated model updates. Finishing a response never finishes tasks.

use galaxy_agent_core::{
    PendingToolBatch, PendingToolCallState, ProviderRun, ProviderRunState, TaskListItem,
    TaskListSnapshot, TaskListStatus, ToolDefinition, ToolResultStatus, TurnRequest,
    UPDATE_TASK_LIST_NAME,
};
use warp_multi_agent_api as api;

const SNAPSHOT_START: &str = "\n<galaxy-task-list>\n";
const SNAPSHOT_END: &str = "\n</galaxy-task-list>\n";
const TASK_LIST_REMINDER: &str = "\nKeep the current task list synchronized with the work at meaningful milestones and before finishing. If a step is now done or work has moved to another step, call update_task_list with the evidence-based status. Pending work must remain visible.\n";

pub(super) const TASK_PROGRESS_GUIDANCE: &str = "\n## Task List Progress\nUse `update_task_list` to keep the visible checklist current throughout multi-step work. It accepts the complete list, with stable task IDs, titles, and pending/in_progress/completed statuses. Keep at most one task in progress and place it before pending tasks. After creating a plan, use its checklist and IDs from the current task list; do not create a competing list. Update when starting the next step, completing a meaningful step, changing scope, or discovering a blocker, and reconcile the list before the final response. Preserve unfinished tasks when blocked or waiting for a decision; explain the blocker in the task title if needed. Mark completed only when the work and required verification actually succeeded. Planning or returning a draft does not complete its implementation tasks. Do not recreate an unchanged list, update after every poll, or spend turns just restating progress. Tool panes show work; task updates do not require prose announcements. CLI command monitoring must continue using its advertised monitoring tools.\n";

pub(super) fn task_list_tool() -> ToolDefinition {
    ToolDefinition {
        name: UPDATE_TASK_LIST_NAME.to_owned(),
        description: "Create or update the visible task list for this conversation. Send the complete list with stable IDs, retaining completed and unfinished work. Update at meaningful milestones, when the current step changes, and before finishing; mark completed only after the work and required checks succeed. This records task progress only and does not execute the tasks or edit the plan document.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array", "minItems": 1, "maxItems": 50,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "minLength": 1, "maxLength": 200},
                            "title": {"type": "string", "minLength": 1, "maxLength": 1000},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                        },
                        "required": ["id", "title", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        }),
    }
}

pub(crate) fn task_list_from_api_tasks(
    tasks: &[api::Task],
    root_task_id: &str,
) -> Option<TaskListSnapshot> {
    use api::message::update_todos::Operation;

    let task = tasks.iter().find(|task| task.id == root_task_id)?;
    let mut pending = Vec::<api::TodoItem>::new();
    let mut completed = Vec::<api::TodoItem>::new();
    for message in &task.messages {
        let Some(api::message::Message::UpdateTodos(update)) = &message.message else {
            continue;
        };
        let Some(operation) = update.operation.as_ref() else {
            continue;
        };
        match operation {
            Operation::CreateTodoList(create) => {
                pending = create.initial_todos.clone();
                completed.clear();
            }
            Operation::UpdatePendingTodos(update) => {
                pending = update.updated_pending_todos.clone();
            }
            Operation::MarkTodosCompleted(mark) => {
                for id in &mark.todo_ids {
                    if let Some(index) = pending.iter().position(|task| task.id == *id) {
                        let item = pending.remove(index);
                        if let Some(old) = completed.iter_mut().find(|task| task.id == *id) {
                            *old = item;
                        } else {
                            completed.push(item);
                        }
                    }
                }
            }
        }
    }
    let snapshot = TaskListSnapshot {
        tasks: completed
            .into_iter()
            .map(|task| task_item(task, TaskListStatus::Completed))
            .chain(pending.into_iter().enumerate().map(|(index, task)| {
                task_item(
                    task,
                    if index == 0 {
                        TaskListStatus::InProgress
                    } else {
                        TaskListStatus::Pending
                    },
                )
            }))
            .collect(),
    };
    snapshot.validate().ok()?;
    Some(snapshot)
}

fn task_item(task: api::TodoItem, status: TaskListStatus) -> TaskListItem {
    TaskListItem {
        id: task.id,
        title: task.title,
        status,
    }
}

pub(super) fn initial_task_list(request: &TurnRequest) -> Option<TaskListSnapshot> {
    let prompt = request.system_prompt.as_deref()?;
    let (_, snapshot) = prompt.rsplit_once(SNAPSHOT_START)?;
    let (snapshot, _) = snapshot.split_once(SNAPSHOT_END)?;
    let snapshot: TaskListSnapshot = serde_json::from_str(snapshot).ok()?;
    snapshot.validate().ok()?;
    Some(snapshot)
}

pub(super) fn add_task_list_context(
    request: &mut TurnRequest,
    snapshot: Option<&TaskListSnapshot>,
) {
    let Some(prompt) = request.system_prompt.as_mut() else {
        return;
    };
    if prompt.ends_with(TASK_LIST_REMINDER) {
        prompt.truncate(prompt.len() - TASK_LIST_REMINDER.len());
    }
    if let Some(start) = prompt.rfind(SNAPSHOT_START) {
        if let Some(end) = prompt[start..].find(SNAPSHOT_END) {
            prompt.replace_range(start..start + end + SNAPSHOT_END.len(), "");
        }
    }
    if let Some(snapshot) = snapshot {
        prompt.push_str(SNAPSHOT_START);
        prompt.push_str(&serde_json::to_string(snapshot).expect("serializable task list"));
        prompt.push_str(SNAPSHOT_END);
    }
    if request
        .tools
        .iter()
        .any(|tool| tool.name == UPDATE_TASK_LIST_NAME)
    {
        prompt.push_str(TASK_LIST_REMINDER);
    }
}

/// Preserve the existing plan checklist behavior, with stable IDs shared by the
/// UI and the next provider request. Rejected tools cannot create task lists.
pub(super) fn task_list_from_plan_batch(batch: &PendingToolBatch) -> Option<TaskListSnapshot> {
    let plan_call = batch.calls.iter().find(|pending| {
        matches!(
            pending.call.name.as_str(),
            "create_plan" | "create_documents"
        ) && !matches!(
            &pending.state,
            PendingToolCallState::Resolved { result }
                if result.status != galaxy_agent_core::ToolResultStatus::Success
        )
    })?;
    let content = plan_call
        .call
        .arguments
        .get("documents")?
        .as_array()?
        .first()?
        .get("content")?
        .as_str()?;
    let mut lines = content.lines();
    lines.find(|line| matches!(line.trim(), "## Tasks" | "## Implementation Tasks"))?;
    let tasks = lines
        .take_while(|line| !line.trim().starts_with("## "))
        .filter_map(|line| line.trim().strip_prefix("- [ ]").map(str::trim))
        .filter(|item| !item.is_empty())
        .take(50)
        .enumerate()
        .map(|(index, item)| TaskListItem {
            id: format!("plan-{}-{index}", plan_call.call.id),
            title: item
                .split_once(" - ")
                .map_or(item, |(title, _)| title)
                .trim()
                .chars()
                .take(1_000)
                .collect(),
            status: TaskListStatus::Pending,
        })
        .collect::<Vec<_>>();
    if tasks.len() < 2 {
        return None;
    }
    let snapshot = TaskListSnapshot { tasks };
    snapshot.validate().ok()?;
    Some(snapshot)
}

fn todo_item(task: &TaskListItem) -> api::TodoItem {
    api::TodoItem {
        id: task.id.clone(),
        title: task.title.clone(),
        description: task.title.clone(),
    }
}

pub(super) fn explicit_task_updates(batch: &PendingToolBatch) -> Vec<TaskListSnapshot> {
    batch
        .calls
        .iter()
        .filter_map(|pending| {
            if pending.call.name != UPDATE_TASK_LIST_NAME {
                return None;
            }
            let PendingToolCallState::Resolved { result } = &pending.state else {
                return None;
            };
            if result.status != ToolResultStatus::Success {
                return None;
            }
            galaxy_agent_core::task_list_from_tool_result(&result.content)
        })
        .collect()
}

pub(crate) fn restored_task_list(run: &ProviderRun) -> Option<TaskListSnapshot> {
    // A mixed batch can have projected an inline update before the other tools
    // finish. Conversely an inline-only batch can commit during normalization
    // before its UI projection was ever persisted. Reconcile both crash windows.
    if let ProviderRunState::AwaitingTools { batch } = run.state() {
        if let Some(snapshot) = explicit_task_updates(batch).pop() {
            return Some(snapshot);
        }
    }
    run.task_list().cloned()
}

pub(super) fn todo_operations(
    previous: Option<&TaskListSnapshot>,
    snapshot: &TaskListSnapshot,
) -> Vec<api::message::update_todos::Operation> {
    use api::message::update_todos::Operation;

    if previous == Some(snapshot) {
        return Vec::new();
    }
    // Existing completed items are immutable in the transcript protocol. A new
    // scope or reopening a completed item therefore starts a replacement list.
    let replace = previous.is_none_or(|previous| {
        previous.tasks.iter().any(|old| {
            old.status == TaskListStatus::Completed
                && !snapshot.tasks.iter().any(|task| {
                    task.id == old.id
                        && task.title == old.title
                        && task.status == TaskListStatus::Completed
                })
        })
    });
    let already_completed = |id: &str| {
        !replace
            && previous.is_some_and(|previous| {
                previous
                    .tasks
                    .iter()
                    .any(|task| task.id == id && task.status == TaskListStatus::Completed)
            })
    };
    let todos = snapshot
        .tasks
        .iter()
        .filter(|task| !already_completed(&task.id))
        .map(todo_item)
        .collect::<Vec<_>>();
    let mut operations = vec![if replace {
        Operation::CreateTodoList(api::CreateTodoList {
            initial_todos: todos,
        })
    } else {
        Operation::UpdatePendingTodos(api::UpdatePendingTodos {
            updated_pending_todos: todos,
        })
    }];
    let todo_ids = snapshot
        .tasks
        .iter()
        .filter(|task| task.status == TaskListStatus::Completed && !already_completed(&task.id))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    if !todo_ids.is_empty() {
        operations.push(Operation::MarkTodosCompleted(api::MarkTodosCompleted {
            todo_ids,
        }));
    }
    operations
}

#[cfg(test)]
#[path = "task_progress_tests.rs"]
mod tests;
