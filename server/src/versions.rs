use semver::{Version, VersionReq};

/// Decide whether a dependency constraint is outdated with respect to the
/// latest published version. Returns None when the constraint is unparseable,
/// unbounded (`any`, missing) or already admits the latest version — callers
/// emit a diagnostic only on Some.
pub fn is_outdated(constraint: Option<&str>, latest: &str) -> Option<bool> {
    let latest = Version::parse(latest).ok()?;
    let raw = constraint?.trim().trim_matches(|c| c == '"' || c == '\'');
    if raw.is_empty() || raw == "any" {
        return None;
    }

    // Dart's exact constraint `1.2.3` must not be parsed with the semver
    // crate's caret-default semantics.
    if let Ok(exact) = Version::parse(raw) {
        return Some(latest > exact);
    }

    // Dart separates comparators with spaces (`>=1.0.0 <2.0.0`); the semver
    // crate wants commas. `^x.y.z` caret semantics match Dart's.
    let req = VersionReq::parse(&raw.split_whitespace().collect::<Vec<_>>().join(", ")).ok()?;
    if req.matches(&latest) {
        return Some(false);
    }

    // The constraint rejects the latest version. Only call it "outdated" when
    // the constraint sits *below* latest — a constraint like `^9.0.0` when
    // latest is `1.0.0` is wrong, but not outdated.
    let floor = req
        .comparators
        .iter()
        .map(|c| Version::new(c.major, c.minor.unwrap_or(0), c.patch.unwrap_or(0)))
        .min()?;
    Some(latest > floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_constraints() {
        assert_eq!(is_outdated(Some("^1.2.0"), "1.6.0"), Some(false));
        assert_eq!(is_outdated(Some("^0.13.0"), "1.6.0"), Some(true));
        assert_eq!(is_outdated(Some("^1.2.0"), "2.0.0"), Some(true));
        // 0.x caret: ^0.1.2 admits 0.1.9 but not 0.2.0
        assert_eq!(is_outdated(Some("^0.1.2"), "0.1.9"), Some(false));
        assert_eq!(is_outdated(Some("^0.1.2"), "0.2.0"), Some(true));
    }

    #[test]
    fn exact_constraints() {
        assert_eq!(is_outdated(Some("1.5.0"), "1.6.0"), Some(true));
        assert_eq!(is_outdated(Some("1.6.0"), "1.6.0"), Some(false));
        assert_eq!(is_outdated(Some("1.6.0+1"), "1.6.0+1"), Some(false));
    }

    #[test]
    fn range_constraints() {
        assert_eq!(is_outdated(Some(">=1.0.0 <2.0.0"), "1.9.0"), Some(false));
        assert_eq!(is_outdated(Some(">=1.0.0 <2.0.0"), "2.1.0"), Some(true));
        assert_eq!(is_outdated(Some(">=0.9.0"), "1.6.0"), Some(false));
    }

    #[test]
    fn unbounded_or_missing_is_never_outdated() {
        assert_eq!(is_outdated(Some("any"), "1.6.0"), None);
        assert_eq!(is_outdated(Some(""), "1.6.0"), None);
        assert_eq!(is_outdated(None, "1.6.0"), None);
    }

    #[test]
    fn unparseable_is_skipped() {
        assert_eq!(is_outdated(Some("banana"), "1.6.0"), None);
        assert_eq!(is_outdated(Some("^1.2.0"), "not-a-version"), None);
    }

    #[test]
    fn constraint_above_latest_is_not_outdated() {
        assert_eq!(is_outdated(Some("^9.0.0"), "1.6.0"), Some(false));
    }

    #[test]
    fn quoted_constraints() {
        assert_eq!(is_outdated(Some("\"^1.2.0\""), "2.0.0"), Some(true));
    }
}
