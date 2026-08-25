//! BOTROSTER control plane: hub routing, the bot registry, the approval engine,
//! the credential broker, hooks, skills, and connectors.
//!
//! See `docs/SPEC.md` §3 for the component map.

#![forbid(unsafe_code)]

pub mod boot;
pub mod bot_tools;
pub mod connector;
pub mod hooks;
pub mod hub;
pub mod internal;
pub mod policy;
pub mod secrets;
pub mod server;
pub mod skills;
