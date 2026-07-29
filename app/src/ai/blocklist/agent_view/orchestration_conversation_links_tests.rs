use warpui::App;

use super::{canonical_agent_id, conversation_id_for_agent_id};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::BlocklistAIHistoryModel;

#[test]
fn canonical_agent_id_preserves_plain_ids() {
    assert_eq!(canonical_agent_id("child-agent-id"), "child-agent-id");
}

#[test]
fn canonical_agent_id_strips_direct_provider_inline_output() {
    assert_eq!(
        canonical_agent_id(
            "child-agent-id\n\nAgent output:\nFinished the task.\n\nAgent output:\nNested text"
        ),
        "child-agent-id"
    );
}

#[test]
fn conversation_id_fallback_rejects_uuid_absent_from_local_history() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let unknown_conversation_id = AIConversationId::new();

        let resolved =
            app.read(|ctx| conversation_id_for_agent_id(&unknown_conversation_id.to_string(), ctx));

        assert_eq!(resolved, None);
    });
}
