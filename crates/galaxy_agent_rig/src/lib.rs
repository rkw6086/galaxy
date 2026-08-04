//! Rig-backed implementations of Galaxy's provider-neutral agent runtime.

mod bedrock;
mod openai_compatible;
mod request;
mod stream;

pub use bedrock::*;
pub use openai_compatible::*;
