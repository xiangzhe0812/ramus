//! Telling the common ways a remote says no apart, so the UI can offer the right fix.

/// Why a network operation failed, as far as its message shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The remote has commits the local branch lacks, so a push was refused.
    Rejected,
    /// Git couldn't sign in.
    Authentication,
    Other,
}

impl FailureKind {
    /// Classifies git's own error text. English only: git is run with its default locale.
    pub fn of(message: &str) -> Self {
        let text = message.to_lowercase();
        let has = |needle: &str| text.contains(needle);
        if has("[rejected]") || has("non-fast-forward") || has("fetch first") {
            Self::Rejected
        } else if has("authentication failed")
            || has("could not read username")
            || has("could not read password")
            || has("permission denied (publickey")
            || has("terminal prompts disabled")
        {
            Self::Authentication
        } else {
            Self::Other
        }
    }
}
