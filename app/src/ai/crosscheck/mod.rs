//! Crosscheck Work Experiment
//!
//! This module implements the "Crosscheck Work" experiment: after the main
//! agent finishes a turn (no pending tool calls), a reviewer sub-agent is
//! spawned to critique the output. If the reviewer does not respond with
//! "LGTM!", its feedback is injected as a synthetic user query to the main
//! agent, which must act on it. This loop continues until the reviewer is
//! satisfied or the maximum iteration count is reached.
//!
//! The reviewer is a lightweight, no-tool agent that only receives the
//! conversation context and produces text feedback.

mod prompt;
mod reviewer;

pub use reviewer::{CrosscheckReviewer, CrosscheckReviewerEvent, ReviewOutcome};