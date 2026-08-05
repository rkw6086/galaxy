use galaxy_agent_core::RuntimeActivity;

const SERVER_MESSAGE_DATA_PREFIX: &str = "galaxy:runtime-activity:v1:";

pub(crate) fn encode(activity: &RuntimeActivity) -> Result<String, serde_json::Error> {
    serde_json::to_string(activity).map(|json| format!("{SERVER_MESSAGE_DATA_PREFIX}{json}"))
}

pub(crate) fn decode(server_message_data: &str) -> Option<RuntimeActivity> {
    let json = server_message_data.strip_prefix(SERVER_MESSAGE_DATA_PREFIX)?;
    serde_json::from_str(json).ok()
}
