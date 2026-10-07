use std::fmt;

/// A semantic version. Build metadata is retained for exact registry/lock keys,
/// but does not affect version precedence or constraint matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: semver::Prerelease,
    pub build: semver::BuildMetadata,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: semver::Prerelease::EMPTY,
            build: semver::BuildMetadata::EMPTY,
        }
    }

    pub fn parse(source: &str) -> Option<Self> {
        let parsed = semver::Version::parse(source).ok()?;
        Some(Self {
            major: parsed.major.try_into().ok()?,
            minor: parsed.minor.try_into().ok()?,
            patch: parsed.patch.try_into().ok()?,
            pre: parsed.pre,
            build: parsed.build,
        })
    }

    pub(crate) fn semantic(&self) -> semver::Version {
        semver::Version {
            major: self.major.into(),
            minor: self.minor.into(),
            patch: self.patch.into(),
            pre: self.pre.clone(),
            build: self.build.clone(),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.semantic().fmt(f)
    }
}

/// Dependency constraints retain their semantic operators, not source spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionConstraint {
    Exact(Version),
    Caret(Version),
    Tilde(Version),
    Range(semver::VersionReq),
}

impl VersionConstraint {
    pub fn parse(source: &str) -> Option<Self> {
        let source = source.trim();
        if let Some(version) = Version::parse(source.strip_prefix('=').unwrap_or(source).trim()) {
            return Some(Self::Exact(version));
        }
        if let Some(version) = source.strip_prefix('^').and_then(Version::parse) {
            return Some(Self::Caret(version));
        }
        if let Some(version) = source.strip_prefix('~').and_then(Version::parse) {
            return Some(Self::Tilde(version));
        }
        let mut request = semver::VersionReq::parse(source).ok()?;
        request.comparators.sort_by_key(comparator_key);
        request.comparators.dedup();
        Some(Self::Range(request))
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.request().matches(&version.semantic())
    }

    pub(crate) fn request(&self) -> semver::VersionReq {
        match self {
            Self::Range(request) => request.clone(),
            Self::Exact(version) | Self::Caret(version) | Self::Tilde(version) => {
                let op = match self {
                    Self::Exact(_) => semver::Op::Exact,
                    Self::Caret(_) => semver::Op::Caret,
                    _ => semver::Op::Tilde,
                };
                semver::VersionReq {
                    comparators: vec![semver::Comparator {
                        op,
                        major: version.major.into(),
                        minor: Some(version.minor.into()),
                        patch: Some(version.patch.into()),
                        pre: version.pre.clone(),
                    }],
                }
            }
        }
    }

    pub(crate) fn canonical(&self) -> Vec<Vec<u8>> {
        let request = self.request();
        // Partial bounds can differ on prereleases admitted by another
        // comparator. Only normalize stable-only conjunctions, where semver's
        // global prerelease gate makes the omitted zero components equivalent.
        let stable_only = request
            .comparators
            .iter()
            .all(|comparator| comparator.pre.is_empty());
        let mut encoded = request
            .comparators
            .iter()
            .map(|comparator| encode_comparator(comparator, stable_only))
            .collect::<Vec<_>>();
        encoded.sort();
        encoded.dedup();
        encoded
    }
}

fn comparator_key(comparator: &semver::Comparator) -> String {
    comparator.to_string()
}

fn normalize_comparator(comparator: &mut semver::Comparator) {
    match comparator.op {
        semver::Op::GreaterEq | semver::Op::Less => {
            comparator.minor.get_or_insert(0);
            comparator.patch.get_or_insert(0);
        }
        semver::Op::Caret => {
            if comparator.major > 0 {
                comparator.minor.get_or_insert(0);
            }
            if comparator.major > 0 || comparator.minor.is_some_and(|minor| minor > 0) {
                comparator.patch.get_or_insert(0);
            }
        }
        semver::Op::Tilde if comparator.minor.is_some() => {
            comparator.patch.get_or_insert(0);
        }
        _ => {}
    }
}

fn encode_comparator(comparator: &semver::Comparator, stable_only: bool) -> Vec<u8> {
    let mut comparator = comparator.clone();
    if stable_only {
        normalize_comparator(&mut comparator);
    }
    let op = match comparator.op {
        semver::Op::Exact => 1,
        semver::Op::Greater => 2,
        semver::Op::GreaterEq => 3,
        semver::Op::Less => 4,
        semver::Op::LessEq => 5,
        semver::Op::Tilde => 6,
        semver::Op::Caret => 7,
        semver::Op::Wildcard => 8,
        _ => unreachable!("semver dependency uses only the documented comparator variants"),
    };
    let mut bytes = vec![op];
    crate::identity::number(comparator.major, &mut bytes);
    for component in [comparator.minor, comparator.patch] {
        bytes.push(u8::from(component.is_some()));
        if let Some(component) = component {
            crate::identity::number(component, &mut bytes);
        }
    }
    crate::identity::string(comparator.pre.as_str(), &mut bytes);
    bytes
}

impl fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(version) => write!(f, "={version}"),
            Self::Caret(version) => write!(f, "^{version}"),
            Self::Tilde(version) => write!(f, "~{version}"),
            Self::Range(request) => request.fmt(f),
        }
    }
}
