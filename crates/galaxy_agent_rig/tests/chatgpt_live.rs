use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::providers::chatgpt::{self, ChatGPTAuth};

fn codex_auth_path() -> PathBuf {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(codex_home).join("auth.json");
    }

    let home = std::env::var_os("HOME").expect("HOME must be set to locate ~/.codex/auth.json");
    PathBuf::from(home).join(".codex").join("auth.json")
}

fn load_codex_auth(path: &Path) -> ChatGPTAuth {
    let file = File::open(path)
        .unwrap_or_else(|error| panic!("failed to open {}: {error}", path.display()));
    let document: serde_json::Value = serde_json::from_reader(BufReader::new(file))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    let tokens = document
        .get("tokens")
        .unwrap_or_else(|| panic!("{} does not contain a tokens object", path.display()));
    let access_token = tokens
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| panic!("{} does not contain an access token", path.display()));
    let account_id = tokens
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .filter(|account_id| !account_id.is_empty())
        .map(str::to_string);

    ChatGPTAuth::AccessToken {
        access_token: access_token.to_string(),
        account_id,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "makes a live ChatGPT request using local Codex credentials"]
async fn live_chatgpt_backend_via_rig_records_full_response() {
    let auth_path = codex_auth_path();
    let client = chatgpt::Client::builder()
        .api_key(load_codex_auth(&auth_path))
        .allow_device_flow(false)
        .build()
        .expect("Rig ChatGPT client should build");
    let model_id =
        std::env::var("GALAXY_CHATGPT_LIVE_MODEL").unwrap_or_else(|_| chatgpt::GPT_5_4.to_string());
    let prompt = std::env::var("GALAXY_CHATGPT_LIVE_PROMPT").unwrap_or_else(|_| {
        "Reply with exactly two short sentences explaining what a live backend smoke test verifies."
            .to_string()
    });
    let model = client.completion_model(&model_id);
    let request = model.completion_request(prompt).build();

    let response = model
        .completion(request)
        .await
        .expect("live ChatGPT completion should succeed");
    let recorded = serde_json::to_string_pretty(&response)
        .expect("the normalized Rig response should serialize");

    println!("CHATGPT_LIVE_RESPONSE_BEGIN\n{recorded}\nCHATGPT_LIVE_RESPONSE_END");
}
