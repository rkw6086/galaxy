//! ChatGPT subscription OAuth state used by the AI settings page.

use async_channel::unbounded;
use galaxy_agent_rig::{ChatGPTDeviceCode, ChatGPTSubscriptionClient};
use galaxyui::{Entity, ModelContext, SingletonEntity};

/// Current state of the local ChatGPT subscription connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatGPTAuthState {
    NotConnected,
    Connecting,
    AwaitingDeviceCode {
        verification_uri: String,
        user_code: String,
    },
    Connected,
    Failed(String),
}

enum ChatGPTAuthEvent {
    DeviceCode(ChatGPTDeviceCode),
    Completed(Result<(), String>),
}

#[derive(Clone, Debug)]
pub(crate) enum ChatGPTAuthModelEvent {
    StateChanged,
}

/// Coordinates Rig's device authorization flow with Galaxy UI.
pub(crate) struct ChatGPTAuthModel {
    state: ChatGPTAuthState,
}

impl ChatGPTAuthModel {
    pub(crate) fn new() -> Self {
        Self {
            state: ChatGPTAuthState::NotConnected,
        }
    }

    pub(crate) fn state(&self) -> &ChatGPTAuthState {
        &self.state
    }

    pub(crate) fn connect(&mut self, ctx: &mut ModelContext<Self>) {
        if matches!(
            self.state,
            ChatGPTAuthState::Connecting | ChatGPTAuthState::AwaitingDeviceCode { .. }
        ) {
            return;
        }

        self.state = ChatGPTAuthState::Connecting;
        ctx.emit(ChatGPTAuthModelEvent::StateChanged);

        let (event_tx, event_rx) = unbounded();
        let device_code_tx = event_tx.clone();
        let _ = ctx.spawn_stream_local(
            event_rx,
            |model, event, ctx| {
                match event {
                    ChatGPTAuthEvent::DeviceCode(code) => {
                        model.state = ChatGPTAuthState::AwaitingDeviceCode {
                            verification_uri: code.verification_uri,
                            user_code: code.user_code,
                        };
                    }
                    ChatGPTAuthEvent::Completed(result) => {
                        model.state = match result {
                            Ok(()) => ChatGPTAuthState::Connected,
                            Err(error) => ChatGPTAuthState::Failed(error),
                        };
                    }
                }
                ctx.emit(ChatGPTAuthModelEvent::StateChanged);
            },
            |_, _| {},
        );

        let _ = ctx.spawn(
            async move {
                let result =
                    match ChatGPTSubscriptionClient::with_device_code_handler(move |code| {
                        let _ = device_code_tx.try_send(ChatGPTAuthEvent::DeviceCode(code));
                    }) {
                        Ok(client) => client.authorize().await,
                        Err(error) => Err(error),
                    };
                let _ = event_tx.send(ChatGPTAuthEvent::Completed(result)).await;
            },
            |_, _, _| {},
        );
    }
}

impl Entity for ChatGPTAuthModel {
    type Event = ChatGPTAuthModelEvent;
}

impl SingletonEntity for ChatGPTAuthModel {}
