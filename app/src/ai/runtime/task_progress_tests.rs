use super::*;

fn snapshot(tasks: &[(&str, TaskListStatus)]) -> TaskListSnapshot {
    TaskListSnapshot {
        tasks: tasks.iter().map(|(id, status)| TaskListItem {
            id: (*id).to_owned(),
            title: format!("Task {id}"),
            status: *status,
        }).collect(),
    }
}

fn task_with_operations(operations: Vec<api::message::update_todos::Operation>) -> api::Task {
    api::Task {
        id: "root".to_owned(),
        messages: operations.into_iter().map(|operation| api::Message {
            message: Some(api::message::Message::UpdateTodos(api::message::UpdateTodos {
                operation: Some(operation),
            })),
            ..Default::default()
        }).collect(),
        ..Default::default()
    }
}

#[test]
fn task_status_updates_reuse_the_list_and_survive_a_followup_request() {
    use TaskListStatus::{Completed, InProgress, Pending};
    use api::message::update_todos::Operation;

    let initial = snapshot(&[("inspect", InProgress), ("implement", Pending), ("verify", Pending)]);
    let next = snapshot(&[("inspect", Completed), ("implement", InProgress), ("verify", Pending)]);
    let updates = todo_operations(Some(&initial), &next);
    assert!(matches!(&updates[0], Operation::UpdatePendingTodos(_)));
    assert!(matches!(&updates[1], Operation::MarkTodosCompleted(mark) if mark.todo_ids == ["inspect"]));
    let mut operations = todo_operations(None, &initial);
    operations.extend(updates);
    let root = task_with_operations(operations);
    let unrelated_child = api::Task { id: "child".to_owned(), ..Default::default() };
    assert_eq!(task_list_from_api_tasks(&[unrelated_child, root], "root"), Some(next.clone()));
    assert!(todo_operations(Some(&next), &next).is_empty());

    let finished = snapshot(&[("inspect", Completed), ("implement", Completed), ("verify", Completed)]);
    let operations = todo_operations(Some(&next), &finished);
    let Operation::UpdatePendingTodos(update) = &operations[0] else {
        panic!("normal progress preserves the existing checklist");
    };
    assert_eq!(update.updated_pending_todos.len(), 2);
    assert!(!update.updated_pending_todos.iter().any(|task| task.id == "inspect"));
}

#[test]
fn reopening_completed_work_replaces_the_protocol_list_without_losing_ids() {
    use TaskListStatus::{Completed, InProgress};
    let old = snapshot(&[("inspect", Completed), ("verify", Completed)]);
    let reopened = snapshot(&[("inspect", Completed), ("verify", InProgress)]);
    let operations = todo_operations(Some(&old), &reopened);
    assert!(matches!(&operations[0], api::message::update_todos::Operation::CreateTodoList(_)));
    let root = task_with_operations(operations);
    assert_eq!(task_list_from_api_tasks(&[root], "root"), Some(reopened));
}

#[test]
fn redelivered_progress_cannot_duplicate_completed_items_in_followup_state() {
    use TaskListStatus::{Completed, InProgress, Pending};
    let initial = snapshot(&[("inspect", InProgress), ("verify", Pending)]);
    let next = snapshot(&[("inspect", Completed), ("verify", InProgress)]);
    let update = todo_operations(Some(&initial), &next);
    let mut operations = todo_operations(None, &initial);
    operations.extend(update.clone());
    operations.extend(update);
    assert_eq!(task_list_from_api_tasks(&[task_with_operations(operations)], "root"), Some(next));
}

#[test]
fn current_snapshot_replaces_stale_prompt_state_without_accumulating_reminders() {
    let initial = snapshot(&[("inspect", TaskListStatus::InProgress)]);
    let current = snapshot(&[("inspect", TaskListStatus::Completed)]);
    let mut request = TurnRequest::new("model", Vec::new());
    request.system_prompt = Some("Follow user instructions.\n".to_owned());
    request.tools = vec![task_list_tool()];
    add_task_list_context(&mut request, Some(&initial));
    assert_eq!(initial_task_list(&request), Some(initial));
    add_task_list_context(&mut request, Some(&current));
    let prompt = request.system_prompt.clone();
    add_task_list_context(&mut request, Some(&current));
    assert_eq!(request.system_prompt, prompt);
    assert_eq!(initial_task_list(&request), Some(current));
    let prompt = request.system_prompt.unwrap();
    assert_eq!(prompt.matches(SNAPSHOT_START).count(), 1);
    assert_eq!(prompt.matches(TASK_LIST_REMINDER).count(), 1);
    assert!(prompt.starts_with("Follow user instructions."));
}

#[test]
fn rejected_plan_creation_cannot_seed_visible_or_durable_tasks() {
    let batch = PendingToolBatch {
        work_id: galaxy_agent_core::ExternalWorkId {
            run_id: galaxy_agent_core::ProviderRunId::new("run"),
            epoch: galaxy_agent_core::RunEpoch::new(1),
        },
        calls: vec![galaxy_agent_core::PendingToolCall {
            call: galaxy_agent_core::ToolCall {
                id: "plan".to_owned(),
                name: "create_plan".to_owned(),
                arguments: serde_json::json!({"documents": [{"content": "## Tasks\n- [ ] Inspect\n- [ ] Verify"}]}),
            },
            state: PendingToolCallState::Resolved { result: galaxy_agent_core::ToolResult {
                images: Vec::new(),
                call_id: "plan".to_owned(),
                content: "Not advertised".to_owned(),
                status: galaxy_agent_core::ToolResultStatus::Error,
            }},
        }],
    };
    assert!(task_list_from_plan_batch(&batch).is_none());
}
