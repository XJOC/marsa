//! Version constraints, such as `^8.2` in `composer.json` or `>=18 <23` in
//! `package.json`, and the best installed version for them.
//!
//! A constraint is a list of alternatives joined by `||` (OR). Each
//! alternative is a list of conditions (AND). Parsing turns every condition
//! into a comparison with one full version, so matching is only `Ord`.
//!
//! This is our own matcher, not `semver::VersionReq`. `VersionReq` has no
//! `||` and no hyphen ranges, and Cargo, Composer, and npm give different
//! meanings to the same text: `8.3.4`, `~8.3`, `>8.1`, and `<=8.3`.

use std::cmp::Ordering;

use semver::{Prerelease, Version};

use super::{Tool, number};

/// The rules a constraint follows. PHP uses Composer's, Node uses npm's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Composer,
    Npm,
}

impl Flavor {
    fn of(tool: Tool) -> Flavor {
        match tool {
            Tool::Php => Flavor::Composer,
            Tool::Node => Flavor::Npm,
        }
    }
}

/// The text does not follow the constraint rules of its tool.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a valid version constraint")]
pub(super) struct InvalidConstraint;

impl InvalidConstraint {
    /// The same code as any other version text that does not parse.
    pub(super) fn code(&self) -> &'static str {
        "invalid_version"
    }
}

/// A parsed constraint: alternatives (OR) of conditions (AND).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Constraint {
    alternatives: Vec<Vec<Condition>>,
}

/// One comparison with a full version, such as `>=8.2.0`.
#[derive(Debug, PartialEq, Eq)]
struct Condition {
    op: Op,
    version: Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

/// The operator written before a version. `=` means the same as no operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    Bare,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    Caret,
    Tilde,
}

/// Two-character operators come first, so `>=` is not read as `>`.
const PREFIXES: [(&str, Prefix); 7] = [
    (">=", Prefix::GreaterEq),
    ("<=", Prefix::LessEq),
    (">", Prefix::Greater),
    ("<", Prefix::Less),
    ("=", Prefix::Bare),
    ("^", Prefix::Caret),
    ("~", Prefix::Tilde),
];

impl Constraint {
    /// Parses `text` with Composer's rules for PHP and npm's rules for Node.
    ///
    /// Returns `Ok(None)` when the constraint allows any version: `*`, `x`,
    /// or empty text in npm. The caller then skips it and uses the global
    /// default, so installing a newer version does not change the choice.
    pub(super) fn parse(text: &str, tool: Tool) -> Result<Option<Constraint>, InvalidConstraint> {
        let flavor = Flavor::of(tool);
        let text = text.trim();
        if text.is_empty() {
            return match flavor {
                Flavor::Npm => Ok(None),
                Flavor::Composer => Err(InvalidConstraint),
            };
        }

        let mut alternatives = Vec::new();
        let mut any = false;
        // Composer also accepts a single `|`. In npm, a single `|` fails later.
        let single_bar = |c: char| flavor == Flavor::Composer && c == '|';
        for alternative in text.split("||").flat_map(|text| text.split(single_bar)) {
            let conditions = parse_alternative(alternative, flavor)?;
            // An alternative with no conditions, such as `*`, allows anything.
            any |= conditions.is_empty();
            alternatives.push(conditions);
        }
        Ok((!any).then_some(Constraint { alternatives }))
    }

    /// The highest installed version that satisfies one of the alternatives.
    pub(super) fn best<'a>(&self, installed: &'a [Version]) -> Option<&'a Version> {
        installed
            .iter()
            .filter(|version| self.alternatives.iter().any(|alt| allows(alt, version)))
            .max()
    }
}

/// True when `version` meets every condition of one alternative.
///
/// A pre-release such as 8.5.0-rc1 counts only when a condition of the same
/// alternative names a pre-release of 8.5.0. This is npm's rule, and the
/// `semver` crate's. So `^8.2` never picks a release candidate by surprise.
fn allows(conditions: &[Condition], version: &Version) -> bool {
    let same_numbers = |other: &Version| {
        (other.major, other.minor, other.patch) == (version.major, version.minor, version.patch)
    };
    conditions.iter().all(|condition| condition.allows(version))
        && (version.pre.is_empty()
            || conditions
                .iter()
                .any(|c| !c.version.pre.is_empty() && same_numbers(&c.version)))
}

impl Condition {
    fn allows(&self, version: &Version) -> bool {
        let order = version.cmp(&self.version);
        match self.op {
            Op::Exact => order == Ordering::Equal,
            Op::Greater => order == Ordering::Greater,
            Op::GreaterEq => order != Ordering::Less,
            Op::Less => order == Ordering::Less,
            Op::LessEq => order != Ordering::Greater,
        }
    }
}

fn condition(op: Op, version: Version) -> Condition {
    Condition { op, version }
}

/// One alternative: conditions split by spaces (and by commas in Composer),
/// or a hyphen range such as `8.1 - 8.3`.
fn parse_alternative(text: &str, flavor: Flavor) -> Result<Vec<Condition>, InvalidConstraint> {
    let mut words = Vec::new();
    for group in text.split(|c: char| flavor == Flavor::Composer && c == ',') {
        let before = words.len();
        words.extend(group.split_whitespace());
        // An empty alternative (`^8 ||`) or an empty group (`>=8.1,,<8.4`).
        if words.len() == before {
            return Err(InvalidConstraint);
        }
    }

    if let [low, "-", high] = words[..] {
        let low = Partial::parse(low, flavor)?;
        let high = Partial::parse(high, flavor)?;
        if low.given == 0 || high.given == 0 {
            return Err(InvalidConstraint);
        }
        // A short upper end covers all of it: `8.1 - 8.3` allows 8.3.9.
        let upper = match high.given {
            3 => condition(Op::LessEq, high.floor()),
            _ => condition(Op::Less, high.ceiling()),
        };
        return Ok(vec![condition(Op::GreaterEq, low.floor()), upper]);
    }

    let mut conditions = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        let (prefix, rest) = split_prefix(word);
        // `>= 8.1`: the version is the next word.
        let rest = match rest {
            "" => words.next().ok_or(InvalidConstraint)?,
            rest => rest,
        };
        let partial = Partial::parse(rest, flavor)?;
        conditions.extend(to_conditions(prefix, partial, flavor)?);
    }
    Ok(conditions)
}

fn split_prefix(word: &str) -> (Prefix, &str) {
    PREFIXES
        .iter()
        .find_map(|&(text, prefix)| word.strip_prefix(text).map(|rest| (prefix, rest)))
        .unwrap_or((Prefix::Bare, word))
}

/// Turns one written condition into conditions on full versions.
fn to_conditions(
    prefix: Prefix,
    partial: Partial,
    flavor: Flavor,
) -> Result<Vec<Condition>, InvalidConstraint> {
    if partial.given == 0 {
        // `*` alone allows anything. `>*` and `^*` mean nothing useful.
        return match prefix {
            Prefix::Bare => Ok(Vec::new()),
            _ => Err(InvalidConstraint),
        };
    }
    // A short version is a range in npm (`18` is any 18.x). In Composer it is
    // a range only with a wildcard: `8.2.*` is any 8.2.x, but `8.2` is 8.2.0.
    let range = partial.given < 3 && (flavor == Flavor::Npm || partial.wildcard);

    let conditions = match prefix {
        Prefix::Bare if range => {
            let last = partial.given - 1;
            between(partial, last)
        }
        Prefix::Bare => vec![condition(Op::Exact, partial.floor())],
        Prefix::GreaterEq => vec![condition(Op::GreaterEq, partial.floor())],
        Prefix::Less => vec![condition(Op::Less, partial.floor())],
        // npm: `>18` is 19.0.0 and up. Composer: `>8.1` allows 8.1.5.
        Prefix::Greater if range => vec![condition(Op::GreaterEq, partial.ceiling())],
        Prefix::Greater => vec![condition(Op::Greater, partial.floor())],
        // npm: `<=22` allows all of 22.x. Composer: `<=8.3` stops at 8.3.0.
        Prefix::LessEq if range => vec![condition(Op::Less, partial.ceiling())],
        Prefix::LessEq => vec![condition(Op::LessEq, partial.floor())],
        Prefix::Caret => {
            // The first number that is not 0 may not change: `^8.2` stays
            // below 9.0.0, `^0.3` below 0.4.0.
            let written = &partial.numbers[..partial.given];
            let index = written.iter().position(|&n| n != 0);
            let index = index.unwrap_or(partial.given - 1);
            between(partial, index)
        }
        Prefix::Tilde => {
            // npm: the patch may change (`~20.1` stays below 20.2.0).
            // Composer: only the last written number may change
            // (`~8.3` below 9.0.0, `~8.3.0` below 8.4.0).
            let index = match flavor {
                Flavor::Npm if partial.given >= 2 => 1,
                Flavor::Npm => 0,
                Flavor::Composer => partial.given.saturating_sub(2),
            };
            between(partial, index)
        }
    };
    Ok(conditions)
}

/// From the written version up to, not including, `bump(index)`.
fn between(partial: Partial, index: usize) -> Vec<Condition> {
    let upper = partial.bump(index);
    vec![
        condition(Op::GreaterEq, partial.floor()),
        condition(Op::Less, upper),
    ]
}

/// A version as written in a constraint: `8`, `8.2`, `8.2.*`, `20.x.x`,
/// `v8.3.4`, or `8.5.0-rc1`.
#[derive(Debug)]
struct Partial {
    /// A number that is not written is 0.
    numbers: [u64; 3],
    /// How many numbers are written: 0 for `*`, 3 for a full version.
    given: usize,
    /// True when the missing numbers are written as `*`, `x`, or `X`.
    wildcard: bool,
    /// Allowed only after three numbers.
    pre: Prerelease,
}

impl Partial {
    fn parse(text: &str, flavor: Flavor) -> Result<Partial, InvalidConstraint> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            None => (text, Prerelease::EMPTY),
            Some((_, "")) => return Err(InvalidConstraint),
            Some((core, pre)) => (core, Prerelease::new(pre).map_err(|_| InvalidConstraint)?),
        };

        let mut partial = Partial {
            numbers: [0; 3],
            given: 0,
            wildcard: false,
            pre,
        };
        for (index, part) in core.split('.').enumerate() {
            if index == 3 {
                return Err(InvalidConstraint);
            }
            if matches!(part, "*" | "x" | "X") {
                partial.wildcard = true;
            } else if partial.wildcard {
                // A number after a wildcard, such as `8.*.1`.
                return Err(InvalidConstraint);
            } else {
                partial.numbers[index] = number(part).ok_or(InvalidConstraint)?;
                partial.given += 1;
            }
        }

        let short_pre = !partial.pre.is_empty() && partial.given < 3;
        // `-dev` is a Composer stability flag, not a version.
        let dev = flavor == Flavor::Composer && partial.pre.as_str().eq_ignore_ascii_case("dev");
        if short_pre || dev {
            return Err(InvalidConstraint);
        }
        Ok(partial)
    }

    /// The written version with missing numbers set to 0: `8.2` is 8.2.0.
    fn floor(self) -> Version {
        let [major, minor, patch] = self.numbers;
        let mut version = Version::new(major, minor, patch);
        version.pre = self.pre;
        version
    }

    /// The first version above everything the short version covers:
    /// `8.2` gives 8.3.0.
    fn ceiling(&self) -> Version {
        self.bump(self.given.saturating_sub(1))
    }

    /// Adds 1 to number `index` and sets the numbers after it to 0.
    /// `bump(0)` on 8.2.0 gives 9.0.0.
    fn bump(&self, index: usize) -> Version {
        let mut numbers = self.numbers;
        numbers[index] = numbers[index].saturating_add(1);
        numbers[index + 1..].fill(0);
        let [major, minor, patch] = numbers;
        Version::new(major, minor, patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allows_version(text: &str, tool: Tool, version: &str) -> bool {
        let constraint = Constraint::parse(text, tool).unwrap().unwrap();
        let version = Version::parse(version).unwrap();
        constraint.best(std::slice::from_ref(&version)).is_some()
    }

    /// Each row: the constraint, versions it allows, versions it does not.
    fn check(tool: Tool, rows: &[(&str, &[&str], &[&str])]) {
        for (text, yes, no) in rows {
            for version in *yes {
                assert!(
                    allows_version(text, tool, version),
                    "{text} allows {version}"
                );
            }
            for version in *no {
                assert!(
                    !allows_version(text, tool, version),
                    "{text} rejects {version}"
                );
            }
        }
    }

    fn best(text: &str, tool: Tool, installed: &[&str]) -> Option<Version> {
        let installed: Vec<Version> = installed
            .iter()
            .map(|v| Version::parse(v).unwrap())
            .collect();
        let constraint = Constraint::parse(text, tool).unwrap().unwrap();
        constraint.best(&installed).cloned()
    }

    #[test]
    fn composer_syntax() {
        let and: (&[&str], &[&str]) = (&["8.1.0", "8.3.9"], &["8.0.9", "8.4.0"]);
        let or: (&[&str], &[&str]) = (&["7.4.0", "8.1.0", "8.9.0"], &["7.3.9", "8.0.0", "9.0.0"]);
        check(
            Tool::Php,
            &[
                ("^8.2", &["8.2.0", "8.9.9"], &["8.1.99", "9.0.0"]),
                ("^8.2.3", &["8.2.3", "8.9.0"], &["8.2.2", "9.0.0"]),
                ("~8.3", &["8.3.0", "8.9.0"], &["8.2.9", "9.0.0"]),
                ("~8.3.0", &["8.3.0", "8.3.9"], &["8.2.9", "8.4.0"]),
                ("8.2.*", &["8.2.0", "8.2.9"], &["8.1.9", "8.3.0"]),
                ("8.*", &["8.0.0", "8.9.9"], &["7.4.0", "9.0.0"]),
                ("8.3.4", &["8.3.4"], &["8.3.3", "8.3.5"]),
                ("v8.3.4", &["8.3.4"], &["8.3.5"]),
                ("8.2", &["8.2.0"], &["8.2.1", "8.3.0"]),
                (">=8.1", &["8.1.0", "9.0.0"], &["8.0.9"]),
                (">8.1", &["8.1.5", "8.2.0"], &["8.1.0", "8.0.0"]),
                ("<8.4", &["8.3.9"], &["8.4.0", "8.4.1"]),
                ("<=8.3", &["8.2.9", "8.3.0"], &["8.3.5"]),
                (">=8.1 <8.4", and.0, and.1),
                (">=8.1,<8.4", and.0, and.1),
                (">=8.1, <8.4", and.0, and.1),
                (">= 8.1", &["8.1.0"], &["8.0.9"]),
                ("^7.4 || ^8.1", or.0, or.1),
                ("^7.4 | ^8.1", or.0, or.1),
                ("8.1 - 8.3", &["8.1.0", "8.3.9"], &["8.0.9", "8.4.0"]),
                ("^0.3", &["0.3.9"], &["0.4.0"]),
            ],
        );
    }

    #[test]
    fn npm_syntax() {
        let eighteen: (&[&str], &[&str]) = (&["18.0.0", "18.20.4"], &["17.9.9", "19.0.0"]);
        let exact: (&[&str], &[&str]) = (&["20.1.0"], &["20.0.9", "20.1.1"]);
        check(
            Tool::Node,
            &[
                (">=18", &["18.0.0", "23.0.0"], &["17.9.9"]),
                (">18", &["19.0.0"], &["18.0.0", "18.5.0"]),
                ("<23", &["22.9.9"], &["23.0.0"]),
                ("<=22", &["22.9.9"], &["23.0.0"]),
                ("^20 || ^22", &["20.1.0", "22.0.0"], &["21.0.0", "23.0.0"]),
                ("18", eighteen.0, eighteen.1),
                ("18.x", eighteen.0, eighteen.1),
                ("18.*", eighteen.0, eighteen.1),
                ("20.x.x", &["20.0.0", "20.9.9"], &["19.9.9", "21.0.0"]),
                ("~20.1", &["20.1.0", "20.1.9"], &["20.0.9", "20.2.0"]),
                (">=18 <23", &["18.0.0", "22.11.0"], &["17.0.0", "23.0.0"]),
                ("18 - 22", &["18.0.0", "22.9.9"], &["17.9.9", "23.0.0"]),
                (
                    "18.2.1 - 22.1",
                    &["18.2.1", "22.1.9"],
                    &["18.2.0", "22.2.0"],
                ),
                ("20.1.0", exact.0, exact.1),
                ("v20.1.0", exact.0, exact.1),
                ("=20.1.0", exact.0, exact.1),
            ],
        );
    }

    #[test]
    fn same_text_differs_between_composer_and_npm() {
        for (text, version, composer, npm) in [
            ("~8.3", "8.9.0", true, false),
            (">8.1", "8.1.5", true, false),
            ("<=8.3", "8.3.5", false, true),
            ("8.2", "8.2.1", false, true),
        ] {
            assert_eq!(allows_version(text, Tool::Php, version), composer, "{text}");
            assert_eq!(allows_version(text, Tool::Node, version), npm, "{text}");
        }
    }

    #[test]
    fn any_version_is_no_constraint() {
        for text in ["*", " * ", "^8.2 || *"] {
            assert_eq!(Constraint::parse(text, Tool::Php), Ok(None), "{text:?}");
        }
        for text in ["*", "x", "", "  ", "^20 || x"] {
            assert_eq!(Constraint::parse(text, Tool::Node), Ok(None), "{text:?}");
        }
    }

    #[test]
    fn rejected_syntax() {
        let both = [
            "!=8.1",
            "<>8.1",
            "lts/*",
            "latest",
            "8.2.0.1",
            "^8 ||",
            "|| ^8",
            "^8 || || ^9",
            ">=",
            ">*",
            "8.*.1",
            "8.2-rc1",
            "8.2.0-",
            "abc",
            "8.1 - ",
            "- 8.3",
            "8.1 - 8.3 - 9",
        ];
        let composer = [
            "",
            "8.2@dev",
            "dev-master",
            "8.2.0-dev",
            "8.2 as 8.3",
            ">=8.1,,<8.4",
            ">=8.1,",
        ];
        let npm = [">=18,<23", "^18 | ^20"];
        let cases = both
            .iter()
            .map(|text| (Tool::Php, text))
            .chain(both.iter().map(|text| (Tool::Node, text)))
            .chain(composer.iter().map(|text| (Tool::Php, text)))
            .chain(npm.iter().map(|text| (Tool::Node, text)));
        for (tool, text) in cases {
            let err = Constraint::parse(text, tool).unwrap_err();
            assert_eq!(err.code(), "invalid_version", "{tool} {text:?}");
        }
    }

    // "Choosing the version" table in the task file.

    #[test]
    fn picks_the_highest_satisfying_version() {
        let v = |text: &str| Some(Version::parse(text).unwrap());
        let php = ["8.1.31", "8.3.14", "8.4.3"];
        assert_eq!(best("^8.2", Tool::Php, &php), v("8.4.3"));
        assert_eq!(best("~8.3.0", Tool::Php, &["8.3.14", "8.4.3"]), v("8.3.14"));
        assert_eq!(best(">=8.1 <8.4", Tool::Php, &php), v("8.3.14"));
        assert_eq!(
            best("^7.4 || ^8.0", Tool::Php, &["7.4.33", "8.4.3"]),
            v("8.4.3")
        );
        assert_eq!(best("^8.2", Tool::Php, &["8.4.3", "8.5.0-rc1"]), v("8.4.3"));
        assert_eq!(
            best(">=8.5.0-rc1", Tool::Php, &["8.4.3", "8.5.0-rc1"]),
            v("8.5.0-rc1")
        );
        assert_eq!(best("^8.2", Tool::Php, &["7.4.33", "8.1.31"]), None);
        let node = ["18.20.4", "22.11.0", "23.3.0"];
        assert_eq!(best(">=18 <23", Tool::Node, &node), v("22.11.0"));
    }

    #[test]
    fn pre_release_needs_the_same_numbers_in_the_same_alternative() {
        // Named for 9.0.0, not 8.5.0.
        assert_eq!(
            best(">=9.0.0-beta1 || ^8.4", Tool::Php, &["8.5.0-rc1"]),
            None
        );
        // Named for 22.0.0, not 23.0.0.
        assert_eq!(best(">=22.0.0-rc.1", Tool::Node, &["23.0.0-rc.1"]), None);
        assert_eq!(
            best(">=22.0.0-rc.1", Tool::Node, &["22.0.0-rc.2"]),
            Some(Version::parse("22.0.0-rc.2").unwrap())
        );
    }
}
