//! How a failure is worded in the banner above the panels. Pure, so the wording is tested.

use git_core::FailureKind;

/// What the banner says about a failure.
#[derive(Debug, PartialEq, Eq)]
pub struct Presentation {
    pub kind: FailureKind,
    /// One line: what went wrong.
    pub title: String,
    /// A second line: what to do about it.
    pub hint: Option<String>,
    /// Whether the full text says more than the title, so "Details" is worth offering.
    pub has_details: bool,
}

pub fn present(message: &str) -> Presentation {
    let kind = FailureKind::of(message);
    let first_line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Something went wrong")
        .to_owned();
    let (title, hint) = match kind {
        FailureKind::Rejected => (
            "Push rejected: the remote has commits you don't have".to_owned(),
            Some("Bring them in, then push again.".to_owned()),
        ),
        FailureKind::Authentication => (
            "Authentication failed".to_owned(),
            Some(
                "Git couldn't sign in. Check your credential helper or ssh-agent; Ramus never \
                 asks for a password itself."
                    .to_owned(),
            ),
        ),
        FailureKind::Other => (first_line, None),
    };
    let has_details = message.trim() != title;
    Presentation {
        kind,
        title,
        hint,
        has_details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_push_gets_the_plain_explanation_and_keeps_the_raw_text() {
        let p = present("git push failed: ! [rejected] main -> main (fetch first)");
        assert_eq!(p.kind, FailureKind::Rejected);
        assert!(p.title.starts_with("Push rejected"));
        assert!(p.hint.is_some());
        assert!(p.has_details);
    }

    #[test]
    fn an_authentication_failure_says_how_to_fix_it() {
        let p = present("fatal: Authentication failed for 'https://x/y.git/'");
        assert_eq!(p.kind, FailureKind::Authentication);
        assert!(p.hint.unwrap().contains("credential helper"));
    }

    #[test]
    fn any_other_failure_leads_with_its_first_line() {
        let p = present("\n  git commit failed: lint failed\nsecond line\n");
        assert_eq!(p.kind, FailureKind::Other);
        assert_eq!(p.title, "git commit failed: lint failed");
        assert!(p.has_details);
    }

    #[test]
    fn a_one_line_message_has_no_details_to_show() {
        let p = present("Trust this repository to commit: its hooks would run.");
        assert!(!p.has_details);
        assert_eq!(p.hint, None);
    }

    #[test]
    fn an_empty_message_still_says_something() {
        assert_eq!(present("").title, "Something went wrong");
    }
}
