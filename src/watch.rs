//! Watching a whole owner instead of listing repositories one by one.
//!
//! A watch is a rule, not a list: it says "everything under this owner on this
//! connection", and it is expanded fresh on every run. A repository added to
//! the forge on Tuesday is covered on Wednesday without anyone touching
//! configuration.
//!
//! Four things narrow it, and all four are reported rather than applied
//! quietly — a rule you cannot see the effect of is a rule you cannot trust:
//!
//! - **Archived repositories**, which are read-only. A sync would do the whole
//!   job and then fail at the push, every run.
//! - **Pull mirrors**, which the forge overwrites from upstream on its own
//!   schedule. One looks exactly like a well-configured fork and is the worst
//!   thing here to sync: whatever a sync landed would be silently discarded.
//! - **Exceptions**, which you write. Names or simple `*` patterns.
//! - **Explicit forks**, which win. Watching an owner and also configuring one
//!   of its repositories by hand is how you keep `keep_removed` or a different
//!   upstream branch on that one; the hand-written row is used and the watch
//!   steps aside.
//! - **Repositories with no recorded upstream**, which are skipped because
//!   there is nothing to sync them *from*. On Gitea that is anything not
//!   migrated from somewhere else — including repositories that are simply
//!   yours rather than forks of anything.

/// Whether a name is covered by one of the exception patterns.
///
/// `*` matches any run of characters; everything else is literal. Deliberately
/// not a full glob: the names being matched are repository names, and the
/// question is nearly always "this one" or "everything ending in this".
pub fn excluded(name: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| matches(name, pattern))
}

fn matches(name: &str, pattern: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return false;
    };
    if !name.starts_with(first) {
        return false;
    }
    // No wildcard at all: the whole thing has to be the name, not a prefix.
    if !pattern.contains('*') {
        return name == pattern;
    }

    let mut rest = &name[first.len()..];
    let segments: Vec<&str> = segments.collect();
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            // A trailing `*` matches whatever is left.
            continue;
        }
        let last = index == segments.len() - 1;
        if last && !pattern.ends_with('*') {
            return rest.ends_with(segment);
        }
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }
    true
}

/// Why a repository under a watched owner is not being synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// Matched one of the exception patterns.
    Excepted(String),
    /// Configured by hand, so that row is used instead.
    ConfiguredSeparately,
    /// The forge does not record where it came from, so there is no upstream.
    NoUpstream,
    /// Read-only on the forge. A sync would build a branch it cannot push.
    Archived,
    /// A pull mirror, which the forge overwrites from upstream on its own
    /// schedule. Anything a sync landed there would be discarded, quietly.
    Mirror,
}

impl Skipped {
    pub fn reason(&self) -> String {
        match self {
            Skipped::Excepted(pattern) => format!("excepted by {pattern:?}"),
            Skipped::ConfiguredSeparately => "configured as its own fork".to_string(),
            Skipped::NoUpstream => "no upstream recorded on the forge".to_string(),
            Skipped::Archived => "archived on the forge".to_string(),
            Skipped::Mirror => "a pull mirror, which the forge overwrites".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_exact_name_matches_only_itself() {
        let rules = patterns(&["codex"]);
        assert!(excluded("codex", &rules));
        assert!(!excluded("codex-fork", &rules), "not a prefix match");
        assert!(!excluded("my-codex", &rules));
    }

    #[test]
    fn a_trailing_star_matches_a_prefix() {
        let rules = patterns(&["test-*"]);
        assert!(excluded("test-one", &rules));
        assert!(excluded("test-", &rules));
        assert!(!excluded("prod-one", &rules));
    }

    #[test]
    fn a_leading_star_matches_a_suffix() {
        let rules = patterns(&["*-archive"]);
        assert!(excluded("old-archive", &rules));
        assert!(!excluded("archive-old", &rules));
    }

    #[test]
    fn a_star_in_the_middle_matches_across() {
        let rules = patterns(&["dokploy*staging"]);
        assert!(excluded("dokploy-eu-staging", &rules));
        assert!(excluded("dokploystaging", &rules));
        assert!(!excluded("dokploy-eu-prod", &rules));
    }

    #[test]
    fn a_bare_star_matches_everything_which_is_a_way_to_pause_a_watch() {
        assert!(excluded("anything", &patterns(&["*"])));
    }

    #[test]
    fn blank_lines_are_ignored_rather_than_matching_everything() {
        let rules = patterns(&["", "   ", "codex"]);
        assert!(excluded("codex", &rules));
        assert!(
            !excluded("dokploy", &rules),
            "an empty pattern must not swallow the whole owner"
        );
    }

    #[test]
    fn no_patterns_excludes_nothing() {
        assert!(!excluded("codex", &[]));
    }

    #[test]
    fn any_one_pattern_is_enough() {
        let rules = patterns(&["dokploy", "test-*"]);
        assert!(excluded("dokploy", &rules));
        assert!(excluded("test-two", &rules));
        assert!(!excluded("codex", &rules));
    }

    #[test]
    fn every_skip_says_why_in_words_rather_than_a_code() {
        assert_eq!(Skipped::Archived.reason(), "archived on the forge");
        assert_eq!(
            Skipped::Mirror.reason(),
            "a pull mirror, which the forge overwrites"
        );
        assert_eq!(
            Skipped::Excepted("test-*".into()).reason(),
            "excepted by \"test-*\""
        );
        assert_eq!(
            Skipped::ConfiguredSeparately.reason(),
            "configured as its own fork"
        );
        assert_eq!(
            Skipped::NoUpstream.reason(),
            "no upstream recorded on the forge"
        );
    }
}
