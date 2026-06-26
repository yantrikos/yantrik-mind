//! Shared error type for the waist. Modules map their internals into this at the boundary.
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MindError {
    #[error("memory: {0}")]
    Memory(String),
    #[error("inference: {0}")]
    Inference(String),
    #[error("denied: {0}")]
    Denied(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MindError>;
