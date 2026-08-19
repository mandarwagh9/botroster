//! Model providers. Each is a translation layer between [`crate::model`] and a
//! vendor protocol, with no behaviour of its own, so the agent loop stays
//! testable.

pub mod http;
pub mod scripted;

pub use http::{Dialect, HttpModel, HttpModelConfig};
pub use scripted::Scripted;
