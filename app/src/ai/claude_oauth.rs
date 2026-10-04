//! Pi-compatible Claude subscription OAuth. Secrets are never logged or serialized into settings.
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub(crate) access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[cfg(not(target_family = "wasm"))]
pub(crate) use desktop::*;

#[cfg(not(target_family = "wasm"))]
mod desktop {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use futures::channel::oneshot;
    use futures::StreamExt;
    use galaxyui::{Entity, ModelContext, SingletonEntity};
    use galaxyui_extras::secure_storage::AppContextExt as _;
    use instant::Instant;
    use rand::RngCore;
    use settings::Setting;
    use sha2::{Digest, Sha256};
    use url::Url;

    use super::Credentials;
    use crate::ai::claude_subscription::{is_stored_credential, OAUTH_REFERENCE_PREFIX};
    use crate::settings::AISettings;

    // Pi's browser flow, checked 2026-10-04:
    // https://github.com/earendil-works/pi/blob/main/packages/ai/src/auth/oauth/anthropic.ts
    const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
    const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
    const REDIRECT_URI: &str = "http://localhost:53692/callback";
    const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
    const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
    type Reply = oneshot::Sender<Result<String, String>>;
    enum Command {
        Resolve(String, Reply),
        Invalidate(String),
        DiscardDraft(String),
    }
    static SENDER: OnceLock<Mutex<Option<async_channel::Sender<Command>>>> = OnceLock::new();

    fn send(command: Command) -> Result<(), String> {
        SENDER
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap()
            .as_ref()
            .ok_or("Claude authorization service is unavailable.")?
            .try_send(command)
            .map_err(|_| "Claude authorization service is busy or unavailable.".to_string())
    }

    pub(crate) fn invalidate(reference: &str) {
        let _ = send(Command::Invalidate(reference.to_string()));
    }

    pub(crate) fn discard_draft(reference: &str) {
        if reference.starts_with(OAUTH_REFERENCE_PREFIX) {
            let _ = send(Command::DiscardDraft(reference.to_string()));
        }
    }

    pub(crate) async fn load_credential(reference: String) -> Result<String, String> {
        if !is_stored_credential(&reference) {
            return Ok(reference);
        }
        let (tx, rx) = oneshot::channel();
        send(Command::Resolve(reference, tx))?;
        let result = futures::future::select(
            Box::pin(rx),
            Box::pin(async_io::Timer::after(Duration::from_secs(35))),
        )
        .await;
        match result {
            futures::future::Either::Left((result, _)) => {
                result.map_err(|_| "Claude authorization service stopped.".to_string())?
            }
            futures::future::Either::Right(_) => {
                Err("Claude token refresh timed out. Try again or sign in again.".to_string())
            }
        }
    }

    struct Refresh {
        id: uuid::Uuid,
        replies: Vec<Reply>,
    }

    /// Marshals secure-storage access to the UI thread and coalesces concurrent refreshes.
    pub(crate) struct ClaudeOAuthModel {
        refreshing: HashMap<String, Refresh>,
    }
    impl Entity for ClaudeOAuthModel {
        type Event = ();
    }
    impl SingletonEntity for ClaudeOAuthModel {}

    impl ClaudeOAuthModel {
        pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
            let (tx, rx) = async_channel::bounded(64);
            *SENDER.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(tx);
            ctx.spawn_stream_local(
                rx,
                |me, command, ctx| match command {
                    Command::Resolve(reference, reply) => me.resolve(reference, reply, ctx),
                    Command::Invalidate(reference) => me.invalidate(&reference),
                    Command::DiscardDraft(reference) => {
                        let saved = AISettings::as_ref(ctx)
                            .openai_providers
                            .value()
                            .iter()
                            .any(|provider| provider.api_key.as_deref() == Some(&reference));
                        if !saved {
                            me.invalidate(&reference);
                            let _ = ctx.secure_storage().remove_value(&reference);
                        }
                    }
                },
                |_, _| {},
            );
            Self {
                refreshing: HashMap::new(),
            }
        }

        fn invalidate(&mut self, reference: &str) {
            if let Some(refresh) = self.refreshing.remove(reference) {
                for reply in refresh.replies {
                    let _ = reply.send(Err("Claude subscription was signed out.".to_string()));
                }
            }
        }

        fn resolve(&mut self, reference: String, reply: Reply, ctx: &mut ModelContext<Self>) {
            if reply.is_canceled() {
                return;
            }
            if let Some(refresh) = self.refreshing.get_mut(&reference) {
                refresh.replies.push(reply);
                return;
            }
            let value = match ctx.secure_storage().read_value(&reference) {
                Ok(value) => value,
                Err(_) => {
                    let _ = reply.send(Err(
                        "Claude subscription is signed out. Sign in again.".to_string()
                    ));
                    return;
                }
            };
            if !reference.starts_with(OAUTH_REFERENCE_PREFIX) {
                let _ = reply.send(Ok(value));
                return;
            }
            let credentials: Credentials = match serde_json::from_str(&value) {
                Ok(credentials) => credentials,
                Err(_) => {
                    let _ = reply.send(Err(
                        "Claude credentials are invalid. Sign in again.".to_string()
                    ));
                    return;
                }
            };
            if credentials.expires_at > now() {
                let _ = reply.send(Ok(credentials.access_token));
                return;
            }
            let id = uuid::Uuid::new_v4();
            self.refreshing.insert(
                reference.clone(),
                Refresh {
                    id,
                    replies: vec![reply],
                },
            );
            ctx.spawn(
                async move {
                    token_request(serde_json::json!({
                        "grant_type": "refresh_token", "client_id": CLIENT_ID,
                        "refresh_token": credentials.refresh_token,
                    }))
                    .await
                },
                move |me, result, ctx| {
                    if me
                        .refreshing
                        .get(&reference)
                        .is_none_or(|refresh| refresh.id != id)
                    {
                        return;
                    }
                    let refresh = me.refreshing.remove(&reference).unwrap();
                    let result = result.and_then(|credentials| {
                        // Signing out deletes the store entry before this callback can commit.
                        if ctx.secure_storage().read_value(&reference).is_err() {
                            return Err("Claude subscription was signed out.".to_string());
                        }
                        save_credentials(&reference, &credentials, ctx)?;
                        Ok(credentials.access_token)
                    });
                    for reply in refresh.replies {
                        let _ = reply.send(result.clone());
                    }
                },
            );
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub(crate) fn save_credentials(
        reference: &str,
        credentials: &Credentials,
        ctx: &galaxyui::AppContext,
    ) -> Result<(), String> {
        let json = serde_json::to_string(credentials)
            .map_err(|_| "Could not encode Claude credentials.".to_string())?;
        ctx.secure_storage()
            .write_value_with_owner_only_fallback(reference, &json)
            .map_err(|_| {
                "Could not securely save Claude credentials. Sign-in was not saved.".to_string()
            })
    }

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    async fn token_request(body: serde_json::Value) -> Result<Credentials, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not create Claude authorization client.".to_string())?;
        let response = client
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                "Claude authorization request failed. Try signing in again.".to_string()
            })?;
        if !response.status().is_success() {
            // Never expose response bodies: providers may echo authorization codes or tokens.
            return Err(format!(
                "Claude authorization failed (HTTP {}). Sign in again.",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > 64 * 1024)
        {
            return Err("Claude authorization returned an oversized response.".to_string());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Claude authorization response failed.".to_string())?;
            if bytes.len().saturating_add(chunk.len()) > 64 * 1024 {
                return Err("Claude authorization returned an oversized response.".to_string());
            }
            bytes.extend_from_slice(&chunk);
        }
        let tokens: TokenResponse = serde_json::from_slice(&bytes)
            .map_err(|_| "Claude authorization returned invalid credentials.".to_string())?;
        credentials_from_response(tokens)
    }

    fn credentials_from_response(tokens: TokenResponse) -> Result<Credentials, String> {
        if !tokens.access_token.contains("sk-ant-oat")
            || tokens.refresh_token.is_empty()
            || tokens.expires_in == 0
        {
            return Err("Claude authorization returned incomplete credentials.".to_string());
        }
        Ok(Credentials {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: now()
                .saturating_add(tokens.expires_in)
                .saturating_sub(5 * 60),
        })
    }

    pub(crate) struct BrowserLogin {
        pub(crate) url: Url,
        verifier: String,
        callback: oneshot::Receiver<Result<String, String>>,
        cancelled: Arc<AtomicBool>,
    }
    impl Drop for BrowserLogin {
        fn drop(&mut self) {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
    impl BrowserLogin {
        pub(crate) fn start() -> Result<Self, String> {
            let listener = TcpListener::bind(("127.0.0.1", 53692))
                .map_err(|_| "Claude sign-in needs localhost port 53692. Close another sign-in attempt and try again.".to_string())?;
            listener
                .set_nonblocking(true)
                .map_err(|_| "Could not configure Claude sign-in callback.".to_string())?;
            let mut bytes = [0; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            let verifier = URL_SAFE_NO_PAD.encode(bytes);
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            let url = authorize_url(&verifier, &challenge)?;
            let cancelled = Arc::new(AtomicBool::new(false));
            let stop = cancelled.clone();
            let state = verifier.clone();
            let (tx, callback) = oneshot::channel();
            std::thread::Builder::new()
                .name("claude-oauth-callback".to_string())
                .spawn(move || {
                    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
                    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if let Some(code) = read_callback(stream, &state) {
                                    let _ = tx.send(Ok(code));
                                    return;
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(50))
                            }
                            Err(_) => {
                                let _ = tx.send(Err("Claude sign-in callback failed.".to_string()));
                                return;
                            }
                        }
                    }
                    let _ = tx.send(Err(
                        "Claude browser sign-in expired or was cancelled.".to_string()
                    ));
                })
                .map_err(|_| "Could not start Claude sign-in callback.".to_string())?;
            Ok(Self {
                url,
                verifier,
                callback,
                cancelled,
            })
        }

        pub(crate) async fn finish(mut self) -> Result<Credentials, String> {
            let code = (&mut self.callback)
                .await
                .map_err(|_| "Claude sign-in callback stopped.".to_string())??;
            token_request(serde_json::json!({
                "grant_type": "authorization_code", "client_id": CLIENT_ID, "code": code,
                "state": self.verifier, "redirect_uri": REDIRECT_URI, "code_verifier": self.verifier,
            })).await
        }
    }

    pub(super) fn authorize_url(verifier: &str, challenge: &str) -> Result<Url, String> {
        let mut url = Url::parse(AUTHORIZE_URL)
            .map_err(|_| "Invalid Claude authorization URL.".to_string())?;
        url.query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", verifier);
        Ok(url)
    }

    pub(super) fn callback_code(request: &str, state: &str) -> Option<String> {
        let mut parts = request.split_whitespace();
        if parts.next()? != "GET" {
            return None;
        }
        let path = parts.next()?;
        if !path.starts_with('/') || path.starts_with("//") {
            return None;
        }
        let url = Url::parse(&format!("http://localhost:53692{path}")).ok()?;
        if url.path() != "/callback" {
            return None;
        }
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        if params.get("state")?.as_str() != state {
            return None;
        }
        params.get("code").filter(|code| !code.is_empty()).cloned()
    }

    fn read_callback(mut stream: TcpStream, state: &str) -> Option<String> {
        stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .ok()?;
        let mut request = String::new();
        // Bound the request line independently of whether a newline is supplied.
        std::io::Read::take(BufReader::new(&mut stream), 8192)
            .read_line(&mut request)
            .ok()?;
        let code = callback_code(&request, state);
        let (status, message) = if code.is_some() {
            (
                "200 OK",
                "Signed in to Claude. You can close this tab and return to Galaxy.",
            )
        } else {
            ("400 Bad Request", "This callback did not match the active Galaxy sign-in. Return to Galaxy and try again.")
        };
        let body = format!("<!doctype html><title>Galaxy — Claude sign-in</title><p>{message}</p>");
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len());
        let _ = stream.write_all(response.as_bytes());
        code
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
#[path = "claude_oauth_tests.rs"]
mod tests;
