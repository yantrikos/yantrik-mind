//! Shared error type for the waist. Modules map their internals into this at the boundary.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("device not authorized")]
    DeviceNotAuthorized,
}

#[derive(Debug, Error)]
pub enum MindError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("not authorized")]
    NotAuthorized,
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

const MEMORY_WRITE_GATE_DENIAL_PREFIX: &str = "memory write-gate: ";

impl MindError {
    /// Construct the typed waist-level refusal emitted when the memory sole-writer rejects
    /// sensitive content. Keeping this protocol in `mind-types` lets memory producers and
    /// conversation controllers agree without coupling either crate to the other's internals.
    pub fn memory_write_gate_refusal(kind: impl Into<String>) -> Self {
        Self::Denied(format!("{MEMORY_WRITE_GATE_DENIAL_PREFIX}{}", kind.into()))
    }

    /// True only for the structured memory write-gate denial, never for an infrastructure error
    /// that happens to mention a gate. The sensitivity-kind suffix deliberately remains open so a
    /// newly added kind cannot silently lose terminal-refusal handling in downstream controllers.
    pub fn is_memory_write_gate_refusal(&self) -> bool {
        matches!(
            self,
            Self::Denied(reason)
                if reason
                    .strip_prefix(MEMORY_WRITE_GATE_DENIAL_PREFIX)
                    .is_some_and(|kind| !kind.is_empty())
        )
    }
}

/// Typed error returned by memory operations.
pub type MemoryError = MindError;

pub type Result<T> = std::result::Result<T, MindError>;

#[cfg(test)]
mod tests {
    use super::MindError;

    #[test]
    fn memory_write_gate_refusal_is_typed_and_kind_extensible() {
        assert!(
            MindError::memory_write_gate_refusal("future-sensitive-kind")
                .is_memory_write_gate_refusal()
        );
        assert!(
            !MindError::Denied("memory write-gate_events unavailable".into())
                .is_memory_write_gate_refusal()
        );
        assert!(
            !MindError::Memory("memory write-gate: credential-phrase".into())
                .is_memory_write_gate_refusal()
        );
    }
}
