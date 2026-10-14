//! HTTP forwarding on already admitted connections.
//!
//! Plaintext HTTP/1 and inspected HTTP have distinct protocol entry points.
//! Their origin connection, request sanitation, and policy binding are private
//! to this module; TCP orchestration only starts a session and consumes its result.

mod binding;
mod error;
mod inspected;
mod plaintext;
mod routing_headers;
mod upstream;

pub(super) use binding::HostBinding;
pub(super) use error::HttpError;
pub(super) use inspected::serve as serve_inspected;
pub(super) use plaintext::{Failure as PlaintextError, serve as serve_plaintext};
