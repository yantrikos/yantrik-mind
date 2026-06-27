//! Shared safety primitives used by BOTH the harm-gate (outward actions) and the memory write-gate
//! (inward writes to the typed moat). One source of truth so a secret can neither LEAVE via an
//! action nor ENTER the cognitive graph.

/// Substrings that mark a secret/credential. Matched case-insensitively against any text that would
/// cross a trust boundary (an outward payload OR a write into the cognitive moat).
pub const SECRET_MARKERS: &[&str] = &[
    "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", // GitHub tokens
    "glpat-",                                       // GitLab
    "akia", "asia",                                 // AWS access keys
    "-----begin", "private key",                    // PEM private keys
    "app password", "app-password",                 // mail app passwords
    "xoxb-", "xoxp-",                               // Slack
    "sk-",                                          // OpenAI-style
];

/// Does this text carry a secret/credential marker? (Case-insensitive.)
pub fn contains_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    SECRET_MARKERS.iter().any(|m| lower.contains(m))
}

/// Where a memory write came from — the trust category. Human/system intent is trusted; everything
/// machine-derived is not. Stored on every Observation so belief revision can weight by independence
/// (e.g. never promote to high confidence from a single human-independent source category).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceCategory {
    /// The operator or a trusted system turn — may carry intent/policy.
    Human,
    /// Output of a sandboxed code skill.
    SandboxedSkill,
    /// Output of a tool (email/github/etc.).
    ToolResult,
    /// A sub-agent's synthesized claim.
    SubAgent,
    /// Fetched web content (attacker-controllable).
    WebContent,
    /// Raw LLM generation with no external grounding.
    LlmInference,
}

impl ProvenanceCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::SandboxedSkill => "sandboxed_skill",
            Self::ToolResult => "tool_result",
            Self::SubAgent => "sub_agent",
            Self::WebContent => "web_content",
            Self::LlmInference => "llm_inference",
        }
    }

    /// Human/system intent is the only trusted category — only it may author skill intent/policy.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Human)
    }

    /// True for machine-derived sources (none of which alone may raise a belief to high confidence).
    pub fn is_human_independent(&self) -> bool {
        !matches!(self, Self::Human)
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "sandboxed_skill" => Self::SandboxedSkill,
            "tool_result" => Self::ToolResult,
            "sub_agent" => Self::SubAgent,
            "web_content" => Self::WebContent,
            "llm_inference" => Self::LlmInference,
            _ => Self::Human,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_secret_markers_case_insensitive() {
        assert!(contains_secret("here is ghp_ABCDEFG1234567890"));
        assert!(contains_secret("My App Password is hunter2"));
        assert!(contains_secret("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(!contains_secret("a perfectly normal sentence about github"));
    }

    #[test]
    fn provenance_trust_and_independence() {
        assert!(ProvenanceCategory::Human.is_trusted());
        assert!(!ProvenanceCategory::SandboxedSkill.is_trusted());
        assert!(ProvenanceCategory::SubAgent.is_human_independent());
        assert!(!ProvenanceCategory::Human.is_human_independent());
        assert_eq!(ProvenanceCategory::from_str(ProvenanceCategory::WebContent.as_str()), ProvenanceCategory::WebContent);
    }
}
