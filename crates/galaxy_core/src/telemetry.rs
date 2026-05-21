use galaxyui::{AppContext, Entity, SingletonEntity};
use serde_json::Value;
use strum::IntoEnumIterator;

#[doc(hidden)]
#[cfg(not(target_family = "wasm"))]
pub use inventory::submit;

use crate::features::FeatureFlag;

pub trait TelemetryEvent: RegisteredTelemetryEvent {
    fn name(&self) -> &'static str;
    fn payload(&self) -> Option<Value>;
    fn description(&self) -> &'static str;
    fn enablement_state(&self) -> EnablementState;
    fn contains_ugc(&self) -> bool;
    fn event_descs() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>>;
}

#[macro_export]
macro_rules! register_telemetry_event {
    ($event:ty) => {
        impl $crate::telemetry::RegisteredTelemetryEvent for $event {}

        #[cfg(not(target_family = "wasm"))]
        $crate::telemetry::submit! {
            $crate::telemetry::TelemetryEventRegistration::<$event>::adapt()
        }
    };
}

pub trait RegisteredTelemetryEvent {}

pub trait TelemetryEventDesc: std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn enablement_state(&self) -> EnablementState;
}

#[doc(hidden)]
pub trait AnyTelemetryEventRegistration: Sync {
    fn events(&self) -> Box<dyn Iterator<Item = Box<dyn TelemetryEventDesc>>>;
}

#[doc(hidden)]
pub struct TelemetryEventRegistration<T: TelemetryEvent + 'static> {
    _marker: std::marker::PhantomData<fn(T) -> T>,
}

impl<T: TelemetryEvent + 'static> TelemetryEventRegistration<T> {
    pub const fn adapt() -> &'static dyn AnyTelemetryEventRegistration {
        &Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: TelemetryEvent + 'static> AnyTelemetryEventRegistration for TelemetryEventRegistration<T> {
    fn events(&self) -> Box<dyn Iterator<Item = Box<dyn TelemetryEventDesc>>> {
        Box::new(T::event_descs())
    }
}

pub fn enum_events<T>() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>>
where
    T: strum::IntoDiscriminant,
    T::Discriminant: strum::IntoEnumIterator + TelemetryEventDesc + 'static,
{
    T::Discriminant::iter()
        .map(|discriminant| Box::new(discriminant) as Box<dyn TelemetryEventDesc>)
}

#[cfg(not(target_family = "wasm"))]
inventory::collect!(&'static dyn AnyTelemetryEventRegistration);

#[cfg(not(target_family = "wasm"))]
pub fn all_events() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>> {
    inventory::iter::<&'static dyn AnyTelemetryEventRegistration>().flat_map(|meta| meta.events())
}

/// No-op: telemetry has been removed from Galaxy.
#[macro_export]
macro_rules! send_telemetry_from_ctx {
    ($event:expr, $ctx:expr) => {
        let _ = &$event;
        let _ = &$ctx;
    };
}

/// No-op: telemetry has been removed from Galaxy.
#[macro_export]
macro_rules! send_telemetry_from_app_ctx {
    ($event:expr, $app_ctx:expr) => {
        let _ = &$event;
        let _ = &$app_ctx;
    };
}

#[derive(Debug)]
pub enum EnablementState {
    Always,
    Flag(FeatureFlag),
    ChannelSpecific { channels: Vec<crate::channel::Channel> },
}

impl EnablementState {
    pub fn is_enabled(&self) -> bool {
        false
    }
}

pub trait TelemetryContextProvider {
    fn user_id(&self, ctx: &AppContext) -> Option<String>;
    fn anonymous_id(&self, ctx: &AppContext) -> String;
}

pub type TelemetryContextModel = Box<dyn TelemetryContextProvider>;

impl Entity for TelemetryContextModel {
    type Event = ();
}

impl SingletonEntity for TelemetryContextModel {}
