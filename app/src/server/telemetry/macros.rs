/// No-op: telemetry has been removed from Galaxy.
#[macro_export]
macro_rules! send_telemetry_sync_from_ctx {
    ($event:expr, $ctx:expr) => {
        let _ = &$event;
        let _ = &$ctx;
    };
}

/// No-op: telemetry has been removed from Galaxy.
#[macro_export]
macro_rules! send_telemetry_sync_from_app_ctx {
    ($event:expr, $app_ctx:expr) => {
        let _ = &$event;
        let _ = &$app_ctx;
    };
}

/// No-op: telemetry has been removed from Galaxy.
#[macro_export]
macro_rules! send_telemetry_on_executor {
    ($auth_state:expr, $event:expr, $executor:expr) => {
        let _ = &$auth_state;
        let _ = &$event;
        let _ = &$executor;
    };
}
