//! OPENBOT: the desktop client for openbot.
//!
//! The name comes from `docs/SPEC.md` §9: a Tauri desktop client for the
//! things a web page cannot do. Local command execution under the three-state
//! policy, file transfer to and from `/workspace`, OS notifications, and
//! (later) WebAuthn/CTAP forwarding.
//!
//! This crate is the engine the UI is a shell over. It drives the runtime the
//! same way every other openbot client does, through the ACP adapter that
//! `openbot acp` speaks, using the reference `agent-client-protocol` SDK rather
//! than hand-rolled wire bytes. Nothing here assumes a window system: the
//! engine is a pure library over stdio, tested against the shipped binary on a
//! live stack.

pub mod attach;
pub mod engine;
pub mod hub;
pub mod policy;
pub mod roster;
pub mod secrets;
pub mod settings;
pub mod skills;
pub mod viewer;
