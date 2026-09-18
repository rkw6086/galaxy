//! ChatGPT subscription browser OAuth state used by the AI settings page.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures::channel::oneshot;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use instant::Instant;
use rand::RngCore;
use sha2::{Digest, Sha256};
use url::Url;

const CHATGPT_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CHATGPT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CHATGPT_SCOPES: &str = "openid profile email offline_access";
const CALLBACK_HOST: &str = "localhost";
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2 * 60);

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

struct OAuthCallback {
    code: String,
    state: String,
}

/// Coordinates browser-based OAuth authorization for ChatGPT subscriptions.
pub(crate) struct ChatGPTAuthModel {
    state: ChatGPTAuthState,
}

impl ChatGPTAuthModel {
    pub(crate) fn new() -> Self {
        let state = match has_or_import_auth_credentials() {
            Ok(()) => ChatGPTAuthState::Connected,
            Err(error) => {
                log::debug!(
                    "[chatgpt/auth] No usable persisted ChatGPT credentials at startup: {error}"
                );
                ChatGPTAuthState::NotConnected
            }
        };
        Self { state }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self {
            state: ChatGPTAuthState::NotConnected,
        }
    }

    pub(crate) fn state(&self) -> &ChatGPTAuthState {
        &self.state
    }

    /// Reuses persisted credentials when possible, otherwise starts browser OAuth with PKCE.
    pub(crate) fn connect(&mut self, ctx: &mut ModelContext<Self>) {
        if matches!(
            self.state,
            ChatGPTAuthState::AwaitingBrowser | ChatGPTAuthState::ExchangingToken
        ) {
            return;
        }

        if has_or_import_auth_credentials().is_ok() {
            self.state = ChatGPTAuthState::Connected;
            ctx.emit(ChatGPTAuthModelEvent::StateChanged);
            return;
        }

        let (redirect_uri, callback_rx) = match start_oauth_callback_server() {
            Ok(server) => server,
            Err(error) => {
                self.fail(error, ctx);
                return;
            }
        };
        let code_verifier = random_urlsafe_string(32);
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let oauth_state = random_hex_string(16);
        let authorize_url = match build_authorize_url(&redirect_uri, &code_challenge, &oauth_state)
        {
            Ok(url) => url,
            Err(error) => {
                self.fail(error, ctx);
                return;
            }
        };

        self.state = ChatGPTAuthState::AwaitingBrowser;
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);
        ctx.open_url(authorize_url.as_str());

        let _ = ctx.spawn(
            async move {
                callback_rx.await.map_err(|_| {
                    "ChatGPT OAuth callback server stopped unexpectedly.".to_string()
                })?
            },
            move |model, callback, ctx| {
                let callback = match callback {
                    Ok(callback) => callback,
                    Err(error) => {
                        model.fail(error, ctx);
                        return;
                    }
                };
                if callback.state != oauth_state {
                    model.fail("ChatGPT OAuth state mismatch.".to_string(), ctx);
                    return;
                }

                model.state = ChatGPTAuthState::ExchangingToken;
                ctx.emit(ChatGPTAuthModelEvent::StateChanged);
                let _ = ctx.spawn(
                    async move {
                        exchange_code_for_tokens(&callback.code, &code_verifier, &redirect_uri)
                            .await
                    },
                    |model, result, ctx| match result {
                        Ok(()) => {
                            model.state = ChatGPTAuthState::Connected;
                            ctx.emit(ChatGPTAuthModelEvent::StateChanged);
                        }
                        Err(error) => model.fail(error, ctx),
                    },
                );
            },
        );
    }

    fn fail(&mut self, error: String, ctx: &mut ModelContext<Self>) {
        self.state = ChatGPTAuthState::Failed(error);
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);
    }
}

impl Entity for ChatGPTAuthModel {
    type Event = ChatGPTAuthModelEvent;
}

impl SingletonEntity for ChatGPTAuthModel {}

fn build_authorize_url(
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> Result<Url, String> {
    let mut url = Url::parse(CHATGPT_AUTHORIZE_URL)
        .map_err(|error| format!("Could not build the ChatGPT authorization URL: {error}"))?;
    url.query_pairs_mut()
        .append_pair("client_id", CHATGPT_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", CHATGPT_SCOPES)
        .append_pair("response_type", "code")
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("state", state)
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "galaxy");
    Ok(url)
}

fn random_urlsafe_string(byte_count: usize) -> String {
    let mut bytes = vec![0; byte_count];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_hex_string(byte_count: usize) -> String {
    let mut bytes = vec![0; byte_count];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn start_oauth_callback_server(
) -> Result<(String, oneshot::Receiver<Result<OAuthCallback, String>>), String> {
    let mut bind_errors = Vec::new();
    let mut bound = None;
    for port in CALLBACK_PORTS {
        match TcpListener::bind((CALLBACK_HOST, port)) {
            Ok(listener) => {
                bound = Some((listener, port));
                break;
            }
            Err(error) => bind_errors.push(format!("{CALLBACK_HOST}:{port}: {error}")),
        }
    }
    let (listener, port) = bound.ok_or_else(|| {
        format!(
            "Could not start the ChatGPT OAuth callback server on ports 1455 or 1457: {}",
            bind_errors.join("; ")
        )
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        format!("Could not configure the ChatGPT OAuth callback server: {error}")
    })?;

    let (callback_tx, callback_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("chatgpt-oauth-callback".to_string())
        .spawn(move || {
            let deadline = Instant::now() + CALLBACK_TIMEOUT;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = callback_tx.send(handle_callback_connection(stream));
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            let _ = callback_tx.send(Err(
                                "Timed out waiting for ChatGPT authorization.".to_string(),
                            ));
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(error) => {
                        let _ = callback_tx.send(Err(format!(
                            "ChatGPT OAuth callback server failed: {error}"
                        )));
                        return;
                    }
                }
            }
        })
        .map_err(|error| format!("Could not start the ChatGPT OAuth callback thread: {error}"))?;

    Ok((
        format!("http://{CALLBACK_HOST}:{port}{CALLBACK_PATH}"),
        callback_rx,
    ))
}

fn handle_callback_connection(mut stream: TcpStream) -> Result<OAuthCallback, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("Could not configure the OAuth callback connection: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("Could not configure the OAuth callback connection: {error}"))?;

    let read_stream = stream
        .try_clone()
        .map_err(|error| format!("Could not read the OAuth callback: {error}"))?;
    let mut reader = BufReader::new(read_stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("Could not read the OAuth callback request: {error}"))?;
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|error| format!("Could not read the OAuth callback headers: {error}"))?;
        if header == "\r\n" || header == "\n" || header.is_empty() {
            break;
        }
    }

    let result = parse_callback_request_line(&request_line);
    let (status, title, message) = if result.is_ok() {
        (
            "200 OK",
            "Authorization Successful",
            "You can close this tab and return to Galaxy.",
        )
    } else {
        (
            "400 Bad Request",
            "Authorization Failed",
            "Something went wrong. Return to Galaxy and try again.",
        )
    };
    let body = callback_response_page(title, message, result.is_err());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if let Err(error) = stream.write_all(response.as_bytes()) {
        log::warn!("[chatgpt/auth] Could not write the OAuth callback response: {error}");
    }
    result
}

fn parse_callback_request_line(request_line: &str) -> Result<OAuthCallback, String> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" || target.is_empty() {
        return Err("Invalid ChatGPT OAuth callback request.".to_string());
    }

    let url = Url::parse(&format!("http://{CALLBACK_HOST}{target}"))
        .map_err(|error| format!("Malformed ChatGPT OAuth callback: {error}"))?;
    if url.path() != CALLBACK_PATH {
        return Err(format!(
            "Unexpected ChatGPT OAuth callback path: {}",
            url.path()
        ));
    }

    let mut code = None;
    let mut state = None;
    let mut oauth_error = None;
    let mut error_description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if !value.is_empty() => code = Some(value.into_owned()),
            "state" if !value.is_empty() => state = Some(value.into_owned()),
            "error" if !value.is_empty() => oauth_error = Some(value.into_owned()),
            "error_description" if !value.is_empty() => {
                error_description = Some(value.into_owned())
            }
            "code" | "state" | "error" | "error_description" => {}
            _ => {}
        }
    }

    if let Some(error) = oauth_error {
        return Err(format!(
            "ChatGPT authorization failed: {error} ({})",
            error_description.as_deref().unwrap_or("no description")
        ));
    }
    Ok(OAuthCallback {
        code: code.ok_or("ChatGPT OAuth callback did not include an authorization code.")?,
        state: state.ok_or("ChatGPT OAuth callback did not include state.")?,
    })
}

fn callback_response_page(title: &str, message: &str, is_error: bool) -> String {
    let accent = if is_error { "#ff6b6b" } else { "#6ee7b7" };
    r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>__TITLE__ - Galaxy</title><style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#10141b;color:#f2f5f7;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}main{max-width:420px;margin:24px;padding:36px;border:1px solid #303846;border-radius:14px;background:#181e28;text-align:center;box-shadow:0 18px 60px #0008}i{display:block;width:48px;height:48px;margin:0 auto 22px;border-radius:50%;background:__ACCENT__}h1{font-size:22px;margin:0 0 12px}p{margin:0;color:#aeb8c6;line-height:1.55}small{display:block;margin-top:24px;color:#697587;letter-spacing:.08em;text-transform:uppercase}</style></head><body><main><i></i><h1>__TITLE__</h1><p>__MESSAGE__</p><small>Galaxy</small></main></body></html>"#
        .replace("__TITLE__", title)
        .replace("__MESSAGE__", message)
        .replace("__ACCENT__", accent)
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
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty());
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned);
    if access_token.is_none() && refresh_token.is_none() {
        return Err("Codex auth file does not contain an access or refresh token".to_string());
    }
    let expires_at = access_token.and_then(extract_expiration_timestamp);
    let id_token = tokens
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let account_id = tokens
        .get("account_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| extract_account_id(id_token.as_deref()))
        .or_else(|| extract_account_id(access_token));

    let record = AuthRecord {
        access_token: access_token.map(ToOwned::to_owned),
        refresh_token,
        id_token,
        expires_at,
        account_id,
    };

    write_auth_file(&record)
}

pub(crate) async fn load_or_refresh_auth_credentials() -> Result<ChatGPTAuthCredentials, String> {
    if let Ok(credentials) = load_auth_credentials() {
        return Ok(credentials);
    }

    let _ = import_codex_credentials();
    if let Ok(credentials) = load_auth_credentials() {
        return Ok(credentials);
    }

    galaxy_agent_rig::refresh_chatgpt_subscription_credentials().await?;
    load_auth_credentials()
}

fn has_or_import_auth_credentials() -> Result<(), String> {
    if has_auth_credentials() {
        return Ok(());
    }
    import_codex_credentials()?;
    if has_auth_credentials() {
        Ok(())
    } else {
        Err("ChatGPT credentials are missing an access or refresh token.".to_string())
    }
}

fn has_auth_credentials() -> bool {
    let Some(path) = auth_file_path() else {
        return false;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(record) = serde_json::from_slice::<AuthRecord>(&bytes) else {
        return false;
    };
    record
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
        || record.access_token.as_deref().is_some_and(|token| {
            !token.trim().is_empty()
                && !token_is_expired(
                    record
                        .expires_at
                        .or_else(|| extract_expiration_timestamp(token)),
                )
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
    if token_is_expired(expires_at) {
        return Err("ChatGPT access token is expired".to_string());
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

fn token_is_expired(expires_at: Option<i64>) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    expires_at.is_none_or(|expires_at| now >= expires_at - 60)
}

async fn exchange_code_for_tokens(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<(), String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CHATGPT_CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_verifier", code_verifier)
        .finish();
    let response = reqwest::Client::new()
        .post(CHATGPT_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|error| format!("ChatGPT token exchange request failed: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("ChatGPT token exchange failed: {status} {body}"));
    }
    let tokens: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Could not parse the ChatGPT token response: {error}"))?;
    let expires_at = extract_expiration_timestamp(&tokens.access_token).or_else(|| {
        tokens.expires_in.map(|expires_in| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or_default()
                + expires_in as i64
        })
    });
    let account_id = extract_account_id(tokens.id_token.as_deref())
        .or_else(|| extract_account_id(Some(&tokens.access_token)));
    write_auth_file(&AuthRecord {
        access_token: Some(tokens.access_token),
        refresh_token: tokens.refresh_token,
        id_token: tokens.id_token,
        expires_at,
        account_id,
    })
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
        .get("chatgpt_account_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(|value| value.as_array())
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(|value| value.as_str())
        })
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
    expires_in: Option<u64>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct AuthRecord {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
    account_id: Option<String>,
}
