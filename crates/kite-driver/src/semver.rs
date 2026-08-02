//! Versions, and what a dependency may ask of one.
//!
//! A version is `major.minor.patch`, with an optional pre-release after a
//! hyphen: `1.4.0`, `2.0.0-rc.1`. Components order numerically, and a
//! pre-release sorts *before* the release it precedes — `2.0.0-rc.1` is not
//! `2.0.0`, it is the last thing tried on the way there.
//!
//! A requirement is what a manifest writes in `version = "…"`. The bare form
//! `"1.2"` means **compatible**: at least the version written, with the same
//! major — or, while the major is 0, the same minor, because a 0.x line has
//! not promised anything yet. This is the one place the notation means more
//! than it spells, so the rule is stated here rather than assumed, it is the
//! whole rule (`0.0.3` is compatible with `0.0.9` — there is no third guess
//! for a second zero), and `^` may be written in front to say it out loud.
//! Everything else is spelled: `=1.2.3` exactly, `>=1.2`, `<2.0.0`, a
//! comma-separated conjunction of those, or `*` for anything released.
//!
//! Two deliberate refusals. Build metadata (`1.0.0+abc`) is an error rather
//! than something silently dropped: metadata does not order, and resolution
//! is ordering. And a pre-release is never chosen by a requirement that did
//! not name it — picking `2.0.0-rc.1` because someone wrote `>=1.0` would be
//! a decision nobody made.

use std::cmp::Ordering;
use std::fmt;

/// `major.minor.patch`, plus an optional pre-release.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The dot-separated identifiers after the hyphen, if any: `rc.1`.
    pub pre: Option<String>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version { major, minor, patch, pre: None }
    }

    /// Read `1.2.3` or `1.2.3-rc.1`. All three components are required here:
    /// a *version* is a fact about a package, and facts are written out.
    /// (A *requirement* may abbreviate — see [`Requirement::parse`].)
    pub fn parse(text: &str) -> Result<Version, String> {
        let version = parse_loose(text, false)?;
        Ok(version)
    }

    fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.triple().cmp(&other.triple()).then_with(|| match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            // The pre-release comes before the release it precedes.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => compare_pre(a, b),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match &self.pre {
            Some(pre) => write!(f, "-{}", pre),
            None => Ok(()),
        }
    }
}

/// Pre-release identifiers compare one at a time: numerically when both are
/// numbers, textually otherwise, and a number sorts before text — so
/// `alpha.2` precedes `alpha.10`, which a plain string comparison would get
/// backwards. When one list is a prefix of the other, the shorter comes
/// first: `1.0.0-rc` precedes `1.0.0-rc.1`.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ordering = match (as_number(x), as_number(y)) {
                    (Some(m), Some(n)) => m.cmp(&n),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => x.cmp(y),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

fn as_number(text: &str) -> Option<u64> {
    // `str::parse` accepts a leading `+`, which is not a digit.
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Read a version, filling missing components with zero when `partial` — the
/// requirement forms abbreviate, a declared version may not.
fn parse_loose(text: &str, partial: bool) -> Result<Version, String> {
    let text = text.trim();
    if text.contains('+') {
        return Err(format!(
            "`{}` carries build metadata, which is refused rather than ignored: metadata does \
             not order, and resolution is ordering",
            text
        ));
    }
    let (base, pre) = match text.split_once('-') {
        Some((base, pre)) => (base, Some(pre)),
        None => (text, None),
    };
    let complaint = || {
        format!(
            "`{}` is not a version: a version is `major.minor.patch`, like `1.4.0`{}",
            text,
            if partial { ", and a requirement may leave trailing components off" } else { "" }
        )
    };

    let mut parts = base.split('.');
    let mut next = |required: bool| -> Result<Option<u64>, String> {
        match parts.next() {
            Some(part) => as_number(part).map(Some).ok_or_else(complaint),
            None if required || !partial => Err(complaint()),
            None => Ok(None),
        }
    };
    let major = next(true)?.unwrap_or(0);
    let minor = next(false)?.unwrap_or(0);
    let patch = next(false)?.unwrap_or(0);
    if parts.next().is_some() {
        return Err(complaint());
    }

    let pre = match pre {
        None => None,
        Some(pre) => {
            let well_formed = !pre.is_empty()
                && pre.split('.').all(|id| {
                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
            if !well_formed {
                return Err(format!(
                    "`{}` has a malformed pre-release: identifiers are dot-separated \
                     letters, digits and hyphens, like `rc.1`",
                    text
                ));
            }
            Some(pre.to_string())
        }
    };
    Ok(Version { major, minor, patch, pre })
}

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

/// What a manifest asks of a dependency's version — a conjunction of clauses,
/// all of which must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    clauses: Vec<Clause>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Clause {
    /// `*` — anything released.
    Any,
    /// `1.2` or `^1.2` — at least this, same major (same minor under major 0).
    Compatible(Version),
    /// `=1.2.3` — this and nothing else.
    Exact(Version),
    Greater(Version),
    GreaterEq(Version),
    Less(Version),
    LessEq(Version),
}

impl Requirement {
    /// The requirement that any released version satisfies — what a
    /// dependency without a `version` field asks, since its candidate set is
    /// already narrowed by its source.
    pub fn any() -> Requirement {
        Requirement { clauses: vec![Clause::Any] }
    }

    /// Read `1.2`, `^1.2`, `=1.2.3`, `>=1.2, <2.0`, `*`, and conjunctions of
    /// those separated by commas. A missing trailing component is zero:
    /// `>1.2` means `>1.2.0`, exactly as if it had been written.
    pub fn parse(text: &str) -> Result<Requirement, String> {
        let mut clauses = Vec::new();
        for piece in text.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                return Err(format!(
                    "`{}` has an empty clause: a requirement is comma-separated, \
                     like `>=1.2, <2.0`",
                    text.trim()
                ));
            }
            let clause = if piece == "*" {
                Clause::Any
            } else if let Some(rest) = piece.strip_prefix(">=") {
                Clause::GreaterEq(parse_loose(rest, true)?)
            } else if let Some(rest) = piece.strip_prefix("<=") {
                Clause::LessEq(parse_loose(rest, true)?)
            } else if let Some(rest) = piece.strip_prefix('>') {
                Clause::Greater(parse_loose(rest, true)?)
            } else if let Some(rest) = piece.strip_prefix('<') {
                Clause::Less(parse_loose(rest, true)?)
            } else if let Some(rest) = piece.strip_prefix('=') {
                Clause::Exact(parse_loose(rest, true)?)
            } else if let Some(rest) = piece.strip_prefix('^') {
                Clause::Compatible(parse_loose(rest, true)?)
            } else if piece.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                Clause::Compatible(parse_loose(piece, true)?)
            } else {
                return Err(format!(
                    "`{}` is not a version requirement: write `1.2`, `=1.2.3`, `>=1.2, <2.0`, \
                     or `*`",
                    piece
                ));
            };
            clauses.push(clause);
        }
        Ok(Requirement { clauses })
    }

    /// Whether `version` satisfies every clause.
    pub fn matches(&self, version: &Version) -> bool {
        if version.pre.is_some() && !self.asks_for_pre(version) {
            return false;
        }
        self.clauses.iter().all(|clause| clause.matches(version))
    }

    /// The requirement satisfied exactly when both are — a conjunction.
    /// Nothing is simplified here: whether the result can be satisfied at all
    /// is a question about which versions *exist*, and it is answered where
    /// the candidate list is, with the names on hand to report if the answer
    /// is no.
    pub fn intersect(&self, other: &Requirement) -> Requirement {
        let mut clauses = self.clauses.clone();
        for clause in &other.clauses {
            if !clauses.contains(clause) {
                clauses.push(clause.clone());
            }
        }
        // `*` adds nothing beside anything else.
        if clauses.len() > 1 {
            clauses.retain(|clause| *clause != Clause::Any);
        }
        Requirement { clauses }
    }

    /// Whether `version` is a pre-release nothing asked for whose *numbers*
    /// are in range — judged on its release form, because `2.0.0-rc.1` sits
    /// below `2.0.0` in the ordering and would otherwise never register as
    /// "in range" for `>=2.0.0`, which is exactly when a reader wonders why
    /// it was passed over. A conflict report uses this to say so.
    pub fn refused_only_for_pre(&self, version: &Version) -> bool {
        if version.pre.is_none() || self.asks_for_pre(version) {
            return false;
        }
        let released = Version::new(version.major, version.minor, version.patch);
        self.clauses.iter().all(|clause| clause.matches(&released))
    }

    /// A pre-release is only offered to a requirement that names a
    /// pre-release of the same `major.minor.patch` — writing `=2.0.0-rc.1` or
    /// `>=2.0.0-rc` is opting in, writing `>=1.0` is not.
    fn asks_for_pre(&self, version: &Version) -> bool {
        self.clauses.iter().any(|clause| match clause.version() {
            Some(named) => named.pre.is_some() && named.triple() == version.triple(),
            None => false,
        })
    }
}

impl Clause {
    fn matches(&self, version: &Version) -> bool {
        match self {
            Clause::Any => true,
            Clause::Compatible(base) => {
                let same_line = if base.major > 0 {
                    version.major == base.major
                } else {
                    version.major == 0 && version.minor == base.minor
                };
                version >= base && same_line
            }
            Clause::Exact(base) => version == base,
            Clause::Greater(base) => version > base,
            Clause::GreaterEq(base) => version >= base,
            Clause::Less(base) => version < base,
            Clause::LessEq(base) => version <= base,
        }
    }

    fn version(&self) -> Option<&Version> {
        match self {
            Clause::Any => None,
            Clause::Compatible(v)
            | Clause::Exact(v)
            | Clause::Greater(v)
            | Clause::GreaterEq(v)
            | Clause::Less(v)
            | Clause::LessEq(v) => Some(v),
        }
    }
}

/// Rendered with its operator spelled, even where the manifest left it bare:
/// a diagnostic quoting `^1.2.0` says what was *meant*, which is what the
/// reader is trying to find out.
impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, clause) in self.clauses.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            match clause {
                Clause::Any => f.write_str("*")?,
                Clause::Compatible(v) => write!(f, "^{}", v)?,
                Clause::Exact(v) => write!(f, "={}", v)?,
                Clause::Greater(v) => write!(f, ">{}", v)?,
                Clause::GreaterEq(v) => write!(f, ">={}", v)?,
                Clause::Less(v) => write!(f, "<{}", v)?,
                Clause::LessEq(v) => write!(f, "<={}", v)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect(text)
    }

    fn req(text: &str) -> Requirement {
        Requirement::parse(text).expect(text)
    }

    #[test]
    fn versions_order_numerically_not_textually() {
        assert!(v("1.10.0") > v("1.9.0"));
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert_eq!(v("1.2.3"), v("1.2.3"));
    }

    #[test]
    fn a_pre_release_sorts_before_its_release() {
        assert!(v("2.0.0-rc.1") < v("2.0.0"));
        assert!(v("2.0.0-rc.1") > v("1.9.0"));
    }

    #[test]
    fn pre_release_identifiers_compare_numerically_when_numeric() {
        assert!(v("1.0.0-alpha.2") < v("1.0.0-alpha.10"));
        // A number sorts before text.
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        // A prefix sorts before what extends it.
        assert!(v("1.0.0-rc") < v("1.0.0-rc.1"));
        assert!(v("1.0.0-alpha") < v("1.0.0-beta"));
    }

    #[test]
    fn a_declared_version_is_written_in_full() {
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("1.two.3").is_err());
    }

    #[test]
    fn build_metadata_is_refused_rather_than_ignored() {
        let err = Version::parse("1.0.0+abc").expect_err("metadata");
        assert!(err.contains("metadata"), "{}", err);
    }

    #[test]
    fn a_version_displays_as_it_parses() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("2.0.0-rc.1").to_string(), "2.0.0-rc.1");
    }

    #[test]
    fn a_bare_requirement_means_compatible() {
        let r = req("1.2.3");
        assert!(r.matches(&v("1.2.3")));
        assert!(r.matches(&v("1.9.0")));
        assert!(!r.matches(&v("1.2.2")));
        assert!(!r.matches(&v("2.0.0")));
        // `^` spells the same thing out loud.
        assert_eq!(req("^1.2.3"), r);
    }

    #[test]
    fn compatible_means_same_minor_while_major_is_zero() {
        let r = req("0.3.1");
        assert!(r.matches(&v("0.3.9")));
        assert!(!r.matches(&v("0.4.0")));
        assert!(!r.matches(&v("1.3.1")));
        // The rule is the stated one even for a second zero: same minor.
        assert!(req("0.0.3").matches(&v("0.0.9")));
    }

    #[test]
    fn operators_and_conjunctions_parse() {
        assert!(req("=1.2.3").matches(&v("1.2.3")));
        assert!(!req("=1.2.3").matches(&v("1.2.4")));
        assert!(req(">=1.2.3").matches(&v("1.2.3")));
        assert!(!req(">1.2.3").matches(&v("1.2.3")));
        assert!(req("<2.0.0").matches(&v("1.9.9")));
        assert!(!req("<2.0.0").matches(&v("2.0.0")));
        assert!(req("<=2.1.0").matches(&v("2.1.0")));
        let both = req(">=1.0.0, <2.0.0");
        assert!(both.matches(&v("1.5.0")));
        assert!(!both.matches(&v("2.0.0")));
        assert!(!both.matches(&v("0.9.0")));
    }

    #[test]
    fn a_partial_version_fills_with_zeroes() {
        assert!(req(">1.0").matches(&v("1.0.1")));
        assert!(!req(">1.0").matches(&v("1.0.0")));
        assert!(req("<=2.1").matches(&v("2.1.0")));
        assert!(!req("<=2.1").matches(&v("2.1.1")));
        assert!(req("=1.2").matches(&v("1.2.0")));
    }

    #[test]
    fn star_matches_everything_released() {
        assert!(req("*").matches(&v("0.0.1")));
        assert!(req("*").matches(&v("99.0.0")));
        // Released: a pre-release still needs asking for.
        assert!(!req("*").matches(&v("1.0.0-rc.1")));
    }

    #[test]
    fn a_pre_release_needs_asking_for() {
        assert!(!req(">=1.0.0").matches(&v("2.0.0-rc.1")));
        assert!(req("=2.0.0-rc.1").matches(&v("2.0.0-rc.1")));
        assert!(req(">=2.0.0-rc").matches(&v("2.0.0-rc.1")));
        // Naming one pre-release does not opt into every other version's.
        assert!(!req(">=2.0.0-rc").matches(&v("2.1.0-rc.1")));
        assert!(req(">=1.0.0").refused_only_for_pre(&v("2.0.0-rc.1")));
        // Judged on the release form: `2.0.0-rc.1` reads as in range for
        // `>=2.0.0` even though it orders below `2.0.0`.
        assert!(req(">=2.0.0").refused_only_for_pre(&v("2.0.0-rc.1")));
        assert!(!req(">=3.0.0").refused_only_for_pre(&v("2.0.0-rc.1")));
    }

    #[test]
    fn intersection_is_conjunction() {
        let merged = req(">=1.0.0").intersect(&req("<2.0.0"));
        assert!(merged.matches(&v("1.5.0")));
        assert!(!merged.matches(&v("2.0.0")));
        assert!(!merged.matches(&v("0.9.0")));
        // `*` adds nothing beside a real clause.
        assert_eq!(req("*").intersect(&req("^1.2")), req("^1.2"));
        // Merging a requirement with itself changes nothing.
        assert_eq!(req("^1.2").intersect(&req("^1.2")), req("^1.2"));
    }

    #[test]
    fn a_requirement_displays_with_its_operators_spelled() {
        assert_eq!(req("1.2").to_string(), "^1.2.0");
        assert_eq!(req(">=1.0, <2.0").to_string(), ">=1.0.0, <2.0.0");
        assert_eq!(req("*").to_string(), "*");
    }

    #[test]
    fn a_requirement_that_is_not_one_says_what_would_be() {
        let err = Requirement::parse("about 1.2").expect_err("nonsense");
        assert!(err.contains("`about 1.2`"), "{}", err);
        assert!(Requirement::parse("1.2,").is_err());
        assert!(Requirement::parse("").is_err());
    }
}
