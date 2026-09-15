//! One lazy local classifier worker shared by all terminal panes.
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use futures::channel::oneshot;
use input_classifier::InputType;
use instant::Instant;
use local_inference::{
    CancellationToken, Device, InferenceEngine, InputCategory, InputClassificationInput,
    InputClassificationTask,
};

struct Request {
    input: InputClassificationInput,
    cancel: CancellationToken,
    reply: oneshot::Sender<Option<InputType>>,
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

static WORKER: OnceLock<Option<mpsc::SyncSender<Request>>> = OnceLock::new();

fn start_worker() -> Option<mpsc::SyncSender<Request>> {
    // Bound queued work across panes. Superseded requests are skipped by the worker.
    let (sender, receiver) = mpsc::sync_channel::<Request>(16);
    let result = std::thread::Builder::new()
        .name("local-input-classifier".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::warn!("Could not initialize local classifier runtime: {error}");
                    return;
                }
            };
            let mut engine = None;
            let mut retry_after = None;
            let mut cached: Option<(InputClassificationInput, InputType)> = None;
            for request in receiver {
                if request.cancel.is_cancelled() || request.reply.is_canceled() {
                    continue;
                }
                if let Some((input, result)) = &cached {
                    if input == &request.input {
                        log::info!("Local input classification: category={result:?}, cached=true");
                        let _ = request.reply.send(Some(*result));
                        continue;
                    }
                }
                if engine.is_none() {
                    if retry_after.is_some_and(|deadline| Instant::now() < deadline) {
                        let _ = request.reply.send(None);
                        continue;
                    }
                    match runtime.block_on(InferenceEngine::new(Device::best_available())) {
                        Ok(loaded) => engine = Some(loaded),
                        Err(error) => {
                            log::warn!("Local input classifier unavailable; preserving input mode: {error:#}");
                            retry_after = Some(Instant::now() + Duration::from_secs(60));
                            let _ = request.reply.send(None);
                            continue;
                        }
                    }
                }
                if request.cancel.is_cancelled() || request.reply.is_canceled() {
                    continue;
                }
                let started = Instant::now();
                let result = runtime.block_on(InputClassificationTask.run_cancellable(
                    engine.as_ref().expect("engine was initialized"),
                    request.input.clone(),
                    &request.cancel,
                ));
                let classification = match result {
                    Ok(result) => {
                        let input_type = match result.category {
                            InputCategory::Shell => InputType::Shell,
                            InputCategory::AgentPrompt => InputType::AI,
                        };
                        log::info!("Local input classification: category={input_type:?}, label_probability={:.3}, elapsed_ms={}", result.confidence, started.elapsed().as_millis());
                        cached = Some((request.input, input_type));
                        Some(input_type)
                    }
                    Err(error) => {
                        if !request.cancel.is_cancelled() {
                            log::warn!("Local classifier returned no decision; preserving input mode: {error:#}");
                        }
                        None
                    }
                };
                let _ = request.reply.send(classification);
            }
        });
    match result {
        Ok(_) => Some(sender),
        Err(error) => {
            log::warn!("Could not start local classifier: {error}");
            None
        }
    }
}

pub(crate) async fn classify(
    user_input: String,
    recent_commands: &[String],
    is_follow_up: bool,
) -> Option<InputType> {
    // Do not classify a truncated script or document as though it were the full input.
    if user_input.trim().is_empty() || user_input.len() > 2048 {
        return None;
    }
    let sender = WORKER.get_or_init(start_worker).as_ref()?;
    let cancel = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let (reply, receiver) = oneshot::channel();
    sender
        .try_send(Request {
            input: InputClassificationInput {
                user_input,
                recent_commands: recent_commands
                    .iter()
                    .rev()
                    .take(3)
                    .map(|command| command.chars().take(120).collect())
                    .collect(),
                is_follow_up,
            },
            cancel,
            reply,
        })
        .ok()?;
    let timeout = galaxyui::r#async::Timer::after(Duration::from_secs(2));
    match futures::future::select(receiver, Box::pin(timeout)).await {
        futures::future::Either::Left((result, _)) => result.ok().flatten(),
        futures::future::Either::Right(_) => {
            log::info!(
                "Local input classification timed out after 2 seconds; preserving input mode"
            );
            None
        }
    }
}
