use std::cell::Cell;
use std::fmt::{self, Write as _};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{self, Interest};
use tracing::{Event, Metadata, Subscriber};

const MAX_ERROR_EVENT_BYTES: usize = 16 * 1024;

thread_local! {
    // Keep the bridge one-way even if a dependency installs a log-to-tracing
    // adapter. The app logger is responsible for remote delivery.
    static FORWARDING_EVENT: Cell<bool> = const { Cell::new(false) };
}

/// Retain tracing errors through the normal app logger without collecting
/// spans or enabling high-volume informational/debug instrumentation.
struct ErrorLogSubscriber;

impl Subscriber for ErrorLogSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= tracing::Level::WARN
    }

    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if self.enabled(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::WARN)
    }

    // Spans are disabled by enabled/register_callsite; these hooks retain no
    // state and perform no per-span work.
    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::ERROR => log::Level::Error,
            tracing::Level::WARN => log::Level::Warn,
            _ => return,
        };
        if !log::log_enabled!(target: metadata.target(), level) {
            return;
        }

        let _ = FORWARDING_EVENT.try_with(|forwarding| {
            if forwarding.replace(true) {
                return;
            }
            struct ResetForwarding<'a>(&'a Cell<bool>);
            impl Drop for ResetForwarding<'_> {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _reset = ResetForwarding(forwarding);

            let mut fields = ErrorEventVisitor::default();
            event.record(&mut fields);
            if fields.text.is_empty() {
                let _ = fields.write_str(metadata.name());
            }
            if fields.truncated {
                fields.text.push_str(" [truncated]");
            }

            log::logger().log(
                &log::Record::builder()
                    .args(format_args!("{}", fields.text))
                    .level(level)
                    .target(metadata.target())
                    .module_path(metadata.module_path())
                    .file(metadata.file())
                    .line(metadata.line())
                    .build(),
            );
        });
    }
}

#[derive(Default)]
struct ErrorEventVisitor {
    text: String,
    truncated: bool,
}

impl fmt::Write for ErrorEventVisitor {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let remaining = MAX_ERROR_EVENT_BYTES.saturating_sub(self.text.len());
        if value.len() <= remaining {
            self.text.push_str(value);
            return Ok(());
        }
        let mut boundary = remaining;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        self.text.push_str(&value[..boundary]);
        self.truncated = true;
        Err(fmt::Error)
    }
}

impl Visit for ErrorEventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if self.truncated {
            return;
        }
        if !self.text.is_empty() {
            let _ = self.write_str(" ");
        }
        if field.name() == "message" {
            let _ = write!(self, "{value:?}");
        } else {
            let _ = write!(self, "{}={value:?}", field.name());
        }
    }
}

pub fn init() -> anyhow::Result<Initialization> {
    subscriber::set_global_default(ErrorLogSubscriber)?;
    Ok(Initialization::default())
}

#[cfg(not(target_family = "wasm"))]
pub fn start_auth_refresh(
    _client: std::sync::Arc<dyn warp_managed_secrets::client::ManagedSecretsClient>,
    _ctx: &mut warpui::AppContext,
) {
}

#[derive(Default)]
pub struct Initialization {
    initialization_warning: Option<anyhow::Error>,
}

impl Initialization {
    pub fn log_initialization_warning(&mut self) {
        if let Some(err) = self.initialization_warning.take() {
            log::warn!("Tracing initialization warning: {err:#}");
        }
    }

    pub(crate) fn shutdown(&mut self) {}
}

impl Drop for Initialization {
    fn drop(&mut self) {
        self.shutdown();
    }
}
