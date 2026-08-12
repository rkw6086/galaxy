//! ChatGPT subscription browser OAuth state used by the AI settings page.
//!
//! Instead of using Rig's device-code flow (which requires users to copy a code),
//! this module implements a standard OAuth 2.0 Authorization Code + PKCE flow:
//!   1. Open the browser to OpenAI's authorize endpoint
//!   2. User approves in browser
//!   3. Browser redirects back to `galaxy://chatgpt/oauth2callback?code=...&state=...`
//!   4. We exchange the code for tokens and write them to Rig's auth file

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use galaxy_core::channel::ChannelState;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use rand::Rng;
use sha2::{Digest, Sha256};
use url::Url;

const CHATGPT_AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const CHATGPT_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CHATGPT_SCOPES: &str = "openid profile email offline_access";

/// Current state of the local ChatGPT subscription connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatGPTAuthState {
    NotConnected,
    AwaitingBrowser,
    ExchangingToken,
    Connected,
    Failed(String),
}

#[derive(Clone, Debug)]
pub(crate) enum ChatGPTAuthModelEvent {
    StateChanged,
}

/// Coordinates browser-based OAuth authorization for ChatGPT subscriptions.
pub(crate) struct ChatGPTAuthModel {
    state: ChatGPTAuthState,
    /// PKCE code verifier stored between authorize and callback.
    pending_code_verifier: Option<String>,
    /// CSRF state token stored between authorize and callback.
    pending_state: Option<String>,
}

impl ChatGPTAuthModel {
    pub(crate) fn new() -> Self {
        Self {
            state: ChatGPTAuthState::NotConnected,
            pending_code_verifier: None,
            pending_state: None,
        }
    }

    pub(crate) fn state(&self) -> &ChatGPTAuthState {
        &self.state
    }

    /// Attempts to connect using existing Codex credentials, falling back to browser OAuth.
    pub(crate) fn connect(&mut self, ctx: &mut ModelContext<Self>) {
        if matches!(
            self.state,
            ChatGPTAuthState::AwaitingBrowser | ChatGPTAuthState::ExchangingToken
        ) {
            return;
        }

        // Try to import credentials from ~/.codex/auth.json first.
        if let Ok(()) = import_codex_credentials() {
            self.state = ChatGPTAuthState::Connected;
            ctx.emit(ChatGPTAuthModelEvent::StateChanged);
            return;
        }

        // No existing credentials — start the browser OAuth flow.
        let code_verifier = generate_random_string(64);
        let code_challenge = compute_code_challenge(&code_verifier);
        let state = generate_random_string(32);
        let redirect_uri = chatgpt_redirect_uri();

        let authorize_url = format!(
            "{CHATGPT_AUTHORIZE_URL}?\
             client_id={CHATGPT_CLIENT_ID}\
             &response_type=code\
             &redirect_uri={redirect_uri}\
             &code_challenge={code_challenge}\
             &code_challenge_method=S256\
             &state={state}\
             &scope={}",
            urlencoding::encode(CHATGPT_SCOPES),
        );

        self.pending_code_verifier = Some(code_verifier);
        self.pending_state = Some(state);
        self.state = ChatGPTAuthState::AwaitingBrowser;
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);

        ctx.open_url(&authorize_url);
    }

    /// Called when the OS routes back `galaxy://chatgpt/oauth2callback?code=...&state=...`
    pub(crate) fn handle_oauth_callback(&mut self, url: &Url, ctx: &mut ModelContext<Self>) {
        let Some(expected_state) = self.pending_state.take() else {
            self.fail(
                "Received OAuth callback but no authorization was in progress.",
                ctx,
            );
            return;
        };
        let Some(code_verifier) = self.pending_code_verifier.take() else {
            self.fail("Received OAuth callback but code verifier is missing.", ctx);
            return;
        };

        // Extract query parameters
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();

        // Check for error response from the authorization server
        if let Some(error) = params.get("error") {
            let description = params
                .get("error_description")
                .map(|d| d.to_string())
                .unwrap_or_else(|| error.to_string());
            self.fail(&format!("ChatGPT authorization denied: {description}"), ctx);
            return;
        }

        let Some(code) = params.get("code") else {
            self.fail("OAuth callback missing authorization code.", ctx);
            return;
        };
        let code = code.to_string();

        let Some(state) = params.get("state") else {
            self.fail("OAuth callback missing state parameter.", ctx);
            return;
        };

        if *state != expected_state {
            self.fail("OAuth callback state mismatch (possible CSRF).", ctx);
            return;
        }

        self.state = ChatGPTAuthState::ExchangingToken;
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);

        let redirect_uri = chatgpt_redirect_uri();

        let _ = ctx.spawn(
            async move { exchange_code_for_tokens(&code, &code_verifier, &redirect_uri).await },
            |model, result, ctx| match result {
                Ok(()) => {
                    model.state = ChatGPTAuthState::Connected;
                    ctx.emit(ChatGPTAuthModelEvent::StateChanged);
                }
                Err(error) => {
                    model.fail(&error, ctx);
                }
            },
        );
    }

    fn fail(&mut self, message: &str, ctx: &mut ModelContext<Self>) {
        self.state = ChatGPTAuthState::Failed(message.to_string());
        self.pending_code_verifier = None;
        self.pending_state = None;
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);
    }
}

impl Entity for ChatGPTAuthModel {
    type Event = ChatGPTAuthModelEvent;
}

impl SingletonEntity for ChatGPTAuthModel {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn chatgpt_redirect_uri() -> String {
    format!("{}://chatgpt/oauth2callback", ChannelState::url_scheme())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatGPTAuthCredentials {
    pub(crate) access_token: String,
    pub(crate) account_id: Option<String>,
}

/// Attempts to read tokens from `~/.codex/auth.json` and write them to Rig's auth file.
/// Returns `Ok(())` if credentials were found and successfully imported.
pub(crate) fn import_codex_credentials() -> Result<(), String> {
    let codex_path = codex_auth_file_path().ok_or("Cannot determine codex auth path")?;
    let bytes = std::fs::read(&codex_path).map_err(|e| format!("{e}"))?;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{e}"))?;

    let tokens = doc.get("tokens").ok_or("No tokens object")?;
    let access_token = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("No access_token")?;

    let expires_at = extract_expiration_timestamp(access_token);

    // If the token is expired, don't import stale credentials.
    if let Some(exp) = expires_at {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now >= exp - 60 {
            return Err("Codex access token is expired".to_string());
        }
    }

    let refresh_token = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let id_token = tokens
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let account_id = tokens
        .get("account_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| extract_account_id(id_token.as_deref()))
        .or_else(|| extract_account_id(Some(access_token)));

    let record = AuthRecord {
        access_token: Some(access_token.to_owned()),
        refresh_token,
        id_token,
        expires_at,
        account_id,
    };

    write_auth_file(&record)
}

pub(crate) fn load_or_import_auth_credentials() -> Result<ChatGPTAuthCredentials, String> {
    load_auth_credentials().or_else(|load_error| {
        import_codex_credentials().map_err(|import_error| {
            format!(
                "Could not load ChatGPT credentials ({load_error}) or import Codex credentials ({import_error})."
            )
        })?;
        load_auth_credentials()
    })
}

fn load_auth_credentials() -> Result<ChatGPTAuthCredentials, String> {
    let path = auth_file_path().ok_or("Cannot determine ChatGPT auth file path")?;
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let record: AuthRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))?;
    let access_token = record
        .access_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .ok_or("ChatGPT auth file does not contain an access token")?;

    let expires_at = record
        .expires_at
        .or_else(|| extract_expiration_timestamp(access_token));
    if let Some(expires_at) = expires_at {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        if now >= expires_at - 60 {
            return Err("ChatGPT access token is expired".to_string());
        }
    }

    let account_id = record
        .account_id
        .clone()
        .filter(|account_id| !account_id.trim().is_empty())
        .or_else(|| extract_account_id(record.id_token.as_deref()))
        .or_else(|| extract_account_id(Some(access_token)));

    Ok(ChatGPTAuthCredentials {
        access_token: access_token.to_string(),
        account_id,
    })
}

fn codex_auth_file_path() -> Option<std::path::PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME") {
        return Some(std::path::PathBuf::from(codex_home).join("auth.json"));
    }
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex").join("auth.json"))
}

fn generate_random_string(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

fn compute_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Exchange the authorization code for tokens and write them to Rig's auth file.
async fn exchange_code_for_tokens(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", CHATGPT_CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];

    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();

    let response = client
        .post(CHATGPT_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {status} {body}"));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let access_token = token_response.access_token;
    let refresh_token = token_response.refresh_token;
    let id_token = token_response.id_token;

    let expires_at = extract_expiration_timestamp(&access_token);
    let account_id =
        extract_account_id(id_token.as_deref()).or_else(|| extract_account_id(Some(&access_token)));

    let auth_record = AuthRecord {
        access_token: Some(access_token),
        refresh_token,
        id_token,
        expires_at,
        account_id,
    };

    write_auth_file(&auth_record)?;
    Ok(())
}

fn write_auth_file(record: &AuthRecord) -> Result<(), String> {
    let path = auth_file_path().ok_or("Cannot determine auth file path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create auth directory: {e}"))?;
    }
    let json =
        serde_json::to_vec_pretty(record).map_err(|e| format!("Failed to serialize auth: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write auth file: {e}"))?;
    Ok(())
}

fn auth_file_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|d| {
            std::path::PathBuf::from(d)
                .join("chatgpt")
                .join("auth.json")
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
            .map(|d| d.join("chatgpt").join("auth.json"))
    }
}

fn extract_expiration_timestamp(token: &str) -> Option<i64> {
    decode_jwt_claims(token)
        .get("exp")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
}

fn extract_account_id(token: Option<&str>) -> Option<String> {
    let claims = decode_jwt_claims(token?);
    claims
        .get("https://api.openai.com/auth")
        .and_then(|v| v.as_object())
        .and_then(|map| map.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

fn decode_jwt_claims(token: &str) -> serde_json::Value {
    let payload = token.split('.').nth(1).unwrap_or_default();
    let decoded = URL_SAFE_NO_PAD.decode(payload.as_bytes());
    decoded
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or(serde_json::Value::Null)
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct AuthRecord {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
    account_id: Option<String>,
}
