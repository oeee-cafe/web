pub mod archive;
pub mod db;
pub mod http_handlers;
pub mod messages;
pub mod preview;
pub mod protocol;
pub mod redis_messages;
pub mod redis_state;
pub mod room_fanout;
pub mod types;
pub mod utils;
pub mod websocket;

// Re-export the public interface
pub use http_handlers::*;
pub use preview::{
    claim_session_preview, report_session_diagnostics, serve_session_preview,
    upload_session_preview,
};
pub use types::*;
pub use websocket::websocket_collaborate_handler;

#[cfg(test)]
mod handler_integration_tests;
#[cfg(test)]
pub(crate) mod protocol_integration_tests;
