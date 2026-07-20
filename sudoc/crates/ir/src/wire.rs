//! JSON wire encoding of finished IR modules (spec/protocol.md §3).
//!
//! Encoding rules live on the IR types themselves (i64-as-string, non-finite
//! floats, external enum tagging, [`crate::Ty::Infer`] rejection). This module
//! is the public entry point used by the emit protocol and wire-trip tests.

use crate::IrModule;

/// Error from encoding or decoding the IR wire format.
#[derive(Debug)]
pub enum WireError {
    /// serde_json failed to encode or decode.
    Json(serde_json::Error),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Json(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Json(e) => Some(e),
        }
    }
}

impl From<serde_json::Error> for WireError {
    fn from(e: serde_json::Error) -> Self {
        WireError::Json(e)
    }
}

/// Serialize modules to the protocol wire JSON (compact).
pub fn to_wire_json(modules: &[IrModule]) -> Result<String, WireError> {
    Ok(serde_json::to_string(modules)?)
}

/// Parse modules from protocol wire JSON.
pub fn from_wire_json(s: &str) -> Result<Vec<IrModule>, WireError> {
    Ok(serde_json::from_str(s)?)
}
