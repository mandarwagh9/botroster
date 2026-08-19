//! The openbot agent harness.
//!
//! Drives a model against tools reached through the Computer Hub. The model
//! sits behind a trait, so the loop runs deterministically in CI against a
//! scripted provider and against a real vendor in production without the loop
//! knowing the difference.

#![forbid(unsafe_code)]

pub mod agent;
pub mod hub_client;
pub mod model;
pub mod providers;
pub mod transient;

pub use agent::{Agent, AgentConfig, AgentEvent, AgentOutcome, FinishReason};
pub use hub_client::{AllowAll, ApprovalHandler, DenyAll, HubClient, HubError};
pub use model::{Content, Message, Model, ModelError, Role, StopReason, ToolUseId};
pub use transient::is_transient;
