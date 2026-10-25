//! ChatGPT subscription browser OAuth state used by the AI settings page.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures::channel::oneshot;
use futures::StreamExt;
use galaxy_agent_rig::ChatGPTCredentialResolver;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use galaxyui_extras::secure_storage::AppContextExt as _;
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
const AUTH_STORAGE_KEY: &str = "galaxy.chatgpt.oauth";
const SIGN_IN_REQUIRED: &str = "ChatGPT sign-in required. Connect or reconnect ChatGPT in Providers.";
type Reply = oneshot::Sender<Result<ChatGPTAuthCredentials, String>>;
static RESOLVER: OnceLock<Mutex<Option<async_channel::Sender<Reply>>>> = OnceLock::new();

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
    record: Option<AuthRecord>,
    refresh: RefreshCoordinator,
    sign_in_generation: u64,
    sign_in_cancel: Option<Arc<AtomicBool>>,
}

impl ChatGPTAuthModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let (tx, rx) = async_channel::bounded(64);
        *RESOLVER.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(tx);
        ctx.spawn_stream_local(rx, |model, reply, ctx| model.resolve(reply, ctx), |_, _| {});
        let record = ctx.secure_storage().read_value(AUTH_STORAGE_KEY).ok()
            .and_then(|json| serde_json::from_str::<AuthRecord>(&json).ok())
            .filter(|record| record.credentials(now()).is_ok() || record.can_refresh());
        let state = if record.is_some() {
            ChatGPTAuthState::Connected
        } else {
            ChatGPTAuthState::NotConnected
        };
        Self {
            state,
            record,
            refresh: RefreshCoordinator::default(),
            sign_in_generation: 0,
            sign_in_cancel: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self {
            state: ChatGPTAuthState::NotConnected,
            record: None,
            refresh: RefreshCoordinator::default(),
            sign_in_generation: 0,
            sign_in_cancel: None,
        }
    }

    pub(crate) fn state(&self) -> &ChatGPTAuthState {
        &self.state
    }

    fn begin_sign_in(&mut self) -> Option<u64> {
        if matches!(
            self.state,
            ChatGPTAuthState::AwaitingBrowser | ChatGPTAuthState::ExchangingToken
        ) {
            return None;
        }

        self.refresh.invalidate();
        self.sign_in_generation += 1;
        Some(self.sign_in_generation)
    }

    /// Explicit connection always starts a fresh browser OAuth flow with PKCE.
    pub(crate) fn connect(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(generation) = self.begin_sign_in() else {
            return;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let (redirect_uri, callback_rx) = match start_oauth_callback_server(cancelled.clone()) {
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

        self.sign_in_cancel = Some(cancelled);
        self.set_state(ChatGPTAuthState::AwaitingBrowser, ctx);
        ctx.open_url(authorize_url.as_str());

        let _ = ctx.spawn(
            async move {
                callback_rx.await.map_err(|_| {
                    "ChatGPT OAuth callback server stopped unexpectedly.".to_string()
                })?
            },
            move |model, callback, ctx| {
                if model.sign_in_generation != generation {
                    return;
                }
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

                model.set_state(ChatGPTAuthState::ExchangingToken, ctx);
                let _ = ctx.spawn(
                    async move {
                        exchange_code_for_tokens(&callback.code, &code_verifier, &redirect_uri)
                            .await
                    },
                    move |model, result, ctx| {
                        if model.sign_in_generation != generation {
                            return;
                        }
                        match result.and_then(|record| {
                            save_auth_record(&record, ctx)?;
                            Ok(record)
                        }) {
                            Ok(record) => {
                                model.record = Some(record);
                                model.sign_in_cancel = None;
                                model.set_state(ChatGPTAuthState::Connected, ctx);
                            }
                            Err(error) => model.fail(error, ctx),
                        }
                    },
                );
            },
        );
    }

    fn fail(&mut self, error: String, ctx: &mut ModelContext<Self>) {
        if let Some(cancelled) = self.sign_in_cancel.take() {
            cancelled.store(true, Ordering::SeqCst);
        }
        self.set_state(ChatGPTAuthState::Failed(error), ctx);
    }

    fn set_state(&mut self, state: ChatGPTAuthState, ctx: &mut ModelContext<Self>) {
        if self.state != state {
            self.state = state;
            ctx.emit(ChatGPTAuthModelEvent::StateChanged);
        }
    }

    pub(crate) fn cancel_sign_in(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(cancelled) = self.sign_in_cancel.take() {
            cancelled.store(true, Ordering::SeqCst);
            self.sign_in_generation += 1;
            self.refresh.invalidate();
            self.set_state(ChatGPTAuthState::NotConnected, ctx);
        }
    }

    fn resolve(&mut self, reply: Reply, ctx: &mut ModelContext<Self>) {
        if reply.is_canceled() {
            return;
        }
        if matches!(self.state, ChatGPTAuthState::AwaitingBrowser | ChatGPTAuthState::ExchangingToken) {
            let _ = reply.send(Err("ChatGPT sign-in is in progress.".to_owned()));
            return;
        }
        let Some(record) = self.record.as_ref() else {
            let _ = reply.send(Err(SIGN_IN_REQUIRED.to_owned()));
            return;
        };
        if let Ok(credentials) = record.credentials(now()) {
            let _ = reply.send(Ok(credentials));
            return;
        }
        if !record.can_refresh() {
            let _ = reply.send(Err(SIGN_IN_REQUIRED.to_owned()));
            self.fail(SIGN_IN_REQUIRED.to_owned(), ctx);
            return;
        }
        let record = record.clone();
        let Some(generation) = self.refresh.begin(reply) else {
            return;
        };
        let _ = ctx.spawn(
            async move { refresh_tokens(&record).await },
            move |model, result, ctx| {
                let completed = model.refresh.complete(generation, result, |record| {
                    save_auth_record(record, ctx)
                });
                match completed {
                    None => {}
                    Some(Ok(record)) => {
                        model.record = Some(record);
                        model.set_state(ChatGPTAuthState::Connected, ctx);
                    }
                    Some(Err(error)) => {
                        if error.reauthenticate {
                            model.record = None;
                            if save_auth_record(&AuthRecord::default(), ctx).is_err() {
                                log::warn!("[chatgpt/auth] Could not persist reconnect-required state");
                            }
                        }
                        model.fail(error.message, ctx);
                    }
                }
            },
        );
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
    cancelled: Arc<AtomicBool>,
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
                if cancelled.load(Ordering::SeqCst) {
                    let _ = callback_tx.send(Err("ChatGPT sign-in was cancelled.".to_owned()));
                    return;
                }
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
    let mut oauth_error = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if !value.is_empty() => code = Some(value.into_owned()),
            "state" if !value.is_empty() => state = Some(value.into_owned()),
            "error" if !value.is_empty() => oauth_error = true,
            "code" | "state" | "error" | "error_description" => {}
            _ => {}
        }
    }

    if oauth_error {
        return Err("ChatGPT browser authorization failed. Please try signing in again.".to_owned());
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

pub(crate) use galaxy_agent_rig::ChatGPTSubscriptionCredentials as ChatGPTAuthCredentials;

pub(crate) fn credential_resolver() -> ChatGPTCredentialResolver {
    Arc::new(|| Box::pin(load_or_refresh_auth_credentials()))
}

pub(crate) async fn load_or_refresh_auth_credentials() -> Result<ChatGPTAuthCredentials, String> {
    let (tx, rx) = oneshot::channel();
    RESOLVER.get_or_init(|| Mutex::new(None)).lock().unwrap()
        .as_ref().ok_or("ChatGPT authorization service is unavailable.")?
        .try_send(tx).map_err(|_| "ChatGPT authorization service is busy or unavailable.")?;
    match futures::future::select(
        Box::pin(rx),
        Box::pin(async_io::Timer::after(Duration::from_secs(35))),
    ).await {
        futures::future::Either::Left((result, _)) => {
            result.map_err(|_| "ChatGPT authorization service stopped.".to_owned())?
        }
        futures::future::Either::Right(_) => {
            Err("ChatGPT credential refresh timed out. Retry or sign in again.".to_owned())
        }
    }
}

#[derive(Clone, Debug)]
struct AuthFailure {
    message: String,
    reauthenticate: bool,
}

#[derive(Default)]
struct RefreshCoordinator {
    generation: u64,
    active: Option<(u64, Vec<Reply>)>,
}

impl RefreshCoordinator {
    fn begin(&mut self, reply: Reply) -> Option<u64> {
        if let Some((_, replies)) = &mut self.active {
            replies.push(reply);
            return None;
        }
        self.generation += 1;
        self.active = Some((self.generation, vec![reply]));
        Some(self.generation)
    }

    fn invalidate(&mut self) {
        self.generation += 1;
        if let Some((_, replies)) = self.active.take() {
            for reply in replies {
                let _ = reply.send(Err("ChatGPT session was replaced. Please sign in again.".to_owned()));
            }
        }
    }

    fn complete(
        &mut self,
        generation: u64,
        result: Result<AuthRecord, AuthFailure>,
        persist: impl FnOnce(&AuthRecord) -> Result<(), String>,
    ) -> Option<Result<AuthRecord, AuthFailure>> {
        if self.active.as_ref().is_none_or(|(active, _)| *active != generation) {
            return None;
        }
        let (_, replies) = self.active.take()?;
        let result = result.and_then(|record| {
            let credentials = record.access_context().map_err(|message| AuthFailure {
                message, reauthenticate: true,
            })?;
            persist(&record).map_err(|_| AuthFailure {
                message: "Could not securely save refreshed ChatGPT credentials. Please sign in again.".to_owned(),
                reauthenticate: true,
            })?;
            Ok((record, credentials))
        });
        for reply in replies {
            let response = result.as_ref().map(|(_, credentials)| credentials.clone())
                .map_err(|error| error.message.clone());
            let _ = reply.send(response);
        }
        Some(result.map(|(record, _)| record))
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64).unwrap_or_default()
}

async fn exchange_code_for_tokens(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<AuthRecord, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CHATGPT_CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_verifier", code_verifier)
        .finish();
    let tokens = token_request(body).await.map_err(|error| error.message)?;
    record_from_tokens(tokens, None, now()).map_err(|error| error.message)
}

async fn refresh_tokens(previous: &AuthRecord) -> Result<AuthRecord, AuthFailure> {
    let refresh_token = previous.refresh_token.as_deref().filter(|token| !token.trim().is_empty())
        .ok_or_else(|| AuthFailure {
            message: SIGN_IN_REQUIRED.to_owned(),
            reauthenticate: true,
        })?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", CHATGPT_CLIENT_ID)
        .append_pair("refresh_token", refresh_token)
        .finish();
    let tokens = token_request(body).await?;
    record_from_tokens(tokens, Some(previous), now())
}

async fn token_request(body: String) -> Result<TokenResponse, AuthFailure> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build().map_err(|_| AuthFailure {
            message: "Could not create the ChatGPT authorization client.".to_owned(),
            reauthenticate: false,
        })?;
    let response = client
        .post(CHATGPT_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|_| AuthFailure {
            message: "ChatGPT authorization request failed. Retry the connection.".to_owned(),
            reauthenticate: false,
        })?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AuthFailure {
            message: "Could not read the ChatGPT authorization response.".to_owned(),
            reauthenticate: (200..300).contains(&status),
        })?;
        if bytes.len().saturating_add(chunk.len()) > 64 * 1024 {
            return Err(AuthFailure {
                message: "ChatGPT authorization returned an oversized response.".to_owned(),
                reauthenticate: (200..300).contains(&status),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_token_response(status, &bytes)
}

fn parse_token_response(status: u16, bytes: &[u8]) -> Result<TokenResponse, AuthFailure> {
    if (200..300).contains(&status) {
        return serde_json::from_slice(bytes).map_err(|_| AuthFailure {
            message: "ChatGPT returned invalid credentials. Please sign in again.".to_owned(),
            reauthenticate: true,
        });
    }
    let body = serde_json::from_slice::<serde_json::Value>(bytes).unwrap_or_default();
    let code = body.get("error").and_then(|error| {
        error.as_str().or_else(|| error.get("code").and_then(|code| code.as_str()))
    });
    let reauthenticate = matches!(status, 401 | 403)
        || (status == 400 && matches!(code,
            Some("invalid_grant" | "refresh_token_reused" | "refresh_token_expired" | "refresh_token_invalid")
        ));
    let message = if reauthenticate {
        format!("ChatGPT authorization expired or was rejected (HTTP {status}). Please sign in again.")
    } else {
        format!("ChatGPT authorization failed (HTTP {status}). Retry the connection.")
    };
    Err(AuthFailure { message, reauthenticate })
}

fn record_from_tokens(
    tokens: TokenResponse,
    previous: Option<&AuthRecord>,
    now: i64,
) -> Result<AuthRecord, AuthFailure> {
    let expires_at = extract_expiration_timestamp(&tokens.access_token).or_else(|| {
        tokens.expires_in.and_then(|expires| i64::try_from(expires).ok())
            .map(|expires| now.saturating_add(expires))
    });
    let account_id = extract_account_id(tokens.id_token.as_deref())
        .or_else(|| extract_account_id(Some(&tokens.access_token)))
        .or_else(|| previous.and_then(|record| record.account_id.clone()));
    let record = AuthRecord {
        access_token: Some(tokens.access_token),
        refresh_token: tokens.refresh_token.or_else(|| previous.and_then(|record| record.refresh_token.clone())),
        id_token: tokens.id_token.or_else(|| previous.and_then(|record| record.id_token.clone())),
        expires_at,
        account_id,
    };
    record.credentials(now).map_err(|_| AuthFailure {
        message: "ChatGPT returned invalid or expired credentials. Please sign in again.".to_owned(),
        reauthenticate: true,
    })?;
    Ok(record)
}

fn save_auth_record(record: &AuthRecord, ctx: &galaxyui::AppContext) -> Result<(), String> {
    let json = serde_json::to_string(record)
        .map_err(|_| "Could not encode ChatGPT credentials.".to_owned())?;
    ctx.secure_storage().write_value_with_owner_only_fallback(AUTH_STORAGE_KEY, &json)
        .map_err(|_| "Could not securely save ChatGPT credentials. Please sign in again.".to_owned())
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

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
struct AuthRecord {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
    account_id: Option<String>,
}

impl std::fmt::Debug for AuthRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGPTAuthRecord")
            .field("credentials", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AuthRecord {
    fn can_refresh(&self) -> bool {
        self.refresh_token.as_deref().is_some_and(|token| !token.trim().is_empty())
    }

    fn access_context(&self) -> Result<ChatGPTAuthCredentials, String> {
        let access_token = self.access_token.as_deref().filter(|token| !token.trim().is_empty())
            .ok_or_else(|| SIGN_IN_REQUIRED.to_owned())?;
        let account_id = self.account_id.clone().filter(|id| !id.trim().is_empty())
            .or_else(|| extract_account_id(self.id_token.as_deref()))
            .or_else(|| extract_account_id(Some(access_token)));
        Ok(ChatGPTAuthCredentials { access_token: access_token.to_owned(), account_id })
    }

    fn credentials(&self, now: i64) -> Result<ChatGPTAuthCredentials, String> {
        let expiry = self.expires_at.or_else(|| {
            self.access_token.as_deref().and_then(extract_expiration_timestamp)
        });
        if expiry.is_none_or(|expiry| now >= expiry.saturating_sub(60)) {
            return Err(SIGN_IN_REQUIRED.to_owned());
        }
        self.access_context()
    }
}

#[cfg(test)]
#[path = "chatgpt_auth_tests.rs"]
mod tests;
