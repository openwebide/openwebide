//! Runtime names shared by filesystem views and Git path selection.
const PROOF_PREFIX: &str = ".openwebide-proof-";
const CREATE_PREFIX: &str = ".openwebide-create-";
const RECOVERY_PREFIX: &str = "recovery-";
const RECOVERY_SUFFIXES: &[&str] = &["", ".init", ".owner", ".owner.part"];

/// Native recovery payloads and unpublished creations are private runtime
/// objects, not project files. Match complete scoped names, preserving ordinary
/// folders such as `.openwebide/recovery-notes`.
pub fn private_workspace_path(path: &str) -> bool {
    fn prefix<'a>(value: &'a str, expected: &str) -> Option<&'a str> {
        value
            .get(..expected.len())
            .filter(|part| part.eq_ignore_ascii_case(expected))?;
        value.get(expected.len()..)
    }
    fn id(value: &str) -> bool {
        value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
    fn id_suffix(value: &str, suffix: impl FnOnce(&str) -> bool) -> bool {
        value.get(..32).is_some_and(id) && value.get(32..).is_some_and(suffix)
    }
    let mut previous = "";
    for part in path.split('/') {
        // U+017F folds to ASCII s on case-folding filesystems, including APFS.
        if part.eq_ignore_ascii_case(".spin") || part.eq_ignore_ascii_case(".ſpin") {
            return true;
        }
        if prefix(part, PROOF_PREFIX).is_some_and(id) {
            return true;
        }
        if prefix(part, CREATE_PREFIX).is_some_and(|value| {
            id_suffix(value, |suffix| {
                suffix
                    .strip_prefix('-')
                    .and_then(|number| {
                        number
                            .parse::<usize>()
                            .ok()
                            .map(|ordinal| (number, ordinal))
                    })
                    .is_some_and(|(number, ordinal)| {
                        ordinal < crate::workspace_transaction::MAX_RESOURCE_STEPS
                            && ordinal.to_string() == number
                    })
            })
        }) {
            return true;
        }
        if previous.eq_ignore_ascii_case(".openwebide")
            && prefix(part, RECOVERY_PREFIX).is_some_and(|value| {
                id_suffix(value, |suffix| {
                    RECOVERY_SUFFIXES
                        .iter()
                        .any(|expected| suffix.eq_ignore_ascii_case(expected))
                })
            })
        {
            return true;
        }
        previous = part;
    }
    false
}

/// Git's `glob` pathspec dialect for the same private-name policy. Patterns
/// match at any repository depth, including descendants of private directories.
/// Apply them with Git's `icase` magic so reserved case aliases stay private.
/// Decimal ranges stay compact rather than using one argument per creation.
pub fn private_workspace_globs() -> Vec<String> {
    let id = "[0-9a-f]".repeat(32);
    let mut names = vec![
        // Git 2.39 otherwise suppresses public entries in nested projects for
        // these top-level exclusions with literal tail names.
        // It still matches exactly one dot, for both files and directories.
        "[.]spin".to_string(),
        "[.]ſpin".to_string(),
        format!("{PROOF_PREFIX}{id}"),
    ];
    names.extend(
        RECOVERY_SUFFIXES
            .iter()
            .map(|suffix| format!(".openwebide/{RECOVERY_PREFIX}{id}{suffix}")),
    );
    for ordinal in ordinal_globs(crate::workspace_transaction::MAX_RESOURCE_STEPS) {
        names.push(format!("{CREATE_PREFIX}{id}-{ordinal}"));
    }
    names
        .into_iter()
        .flat_map(|name| [format!("**/{name}"), format!("**/{name}/**")])
        .collect()
}

fn ordinal_globs(limit: usize) -> Vec<String> {
    fn range(prefix: String, low: usize, high: usize, digits: u32, out: &mut Vec<String>) {
        if digits == 0 {
            out.push(prefix);
        } else if low == 0 && high == 10usize.pow(digits) - 1 {
            out.push(prefix + &"[0-9]".repeat(digits as usize));
        } else {
            let unit = 10usize.pow(digits - 1);
            let mut first = low / unit;
            let mut last = high / unit;
            if first == last {
                range(
                    format!("{prefix}{first}"),
                    low % unit,
                    high % unit,
                    digits - 1,
                    out,
                );
                return;
            }
            if !low.is_multiple_of(unit) {
                range(
                    format!("{prefix}{first}"),
                    low % unit,
                    unit - 1,
                    digits - 1,
                    out,
                );
                first += 1;
            }
            if high % unit != unit - 1 {
                range(format!("{prefix}{last}"), 0, high % unit, digits - 1, out);
                last -= 1;
            }
            if first <= last {
                let digit = if first == last {
                    first.to_string()
                } else {
                    format!("[{first}-{last}]")
                };
                out.push(format!(
                    "{prefix}{digit}{}",
                    "[0-9]".repeat((digits - 1) as usize)
                ));
            }
        }
    }
    let mut out = Vec::new();
    if limit > 0 {
        out.push("0".into());
    }
    let mut first = 1usize;
    let mut digits = 1;
    while first < limit {
        let last = first.saturating_mul(10).saturating_sub(1).min(limit - 1);
        range(String::new(), first, last, digits, &mut out);
        let Some(next) = first.checked_mul(10) else {
            break;
        };
        first = next;
        digits += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_decimal_ranges_are_canonical() {
        // This uses the same wildcard dialect as Git, checked against Git in
        // the native contract as well as all nearby ordinal boundaries here.
        for limit in [0, 1, 9, 10, 11, 99, 100, 101, 1024] {
            let patterns = ordinal_globs(limit);
            for number in 0..=limit + 1 {
                for candidate in [
                    number.to_string(),
                    format!("0{number}"),
                    format!("{number}x"),
                ] {
                    let expected = number < limit && candidate == number.to_string();
                    let found = patterns.iter().any(|pattern| {
                        let mut tokens = pattern.bytes();
                        let mut candidate = candidate.bytes();
                        while let Some(token) = tokens.next() {
                            let Some(byte) = candidate.next() else {
                                return false;
                            };
                            if token == b'[' {
                                let low = tokens.next().unwrap();
                                assert_eq!(tokens.next(), Some(b'-'));
                                let high = tokens.next().unwrap();
                                assert_eq!(tokens.next(), Some(b']'));
                                if !(low..=high).contains(&byte) {
                                    return false;
                                }
                            } else if token != byte {
                                return false;
                            }
                        }
                        candidate.next().is_none()
                    });
                    assert_eq!(
                        found, expected,
                        "limit {limit}, candidate {candidate}, {patterns:?}"
                    );
                }
            }
        }
        assert!(
            private_workspace_globs()
                .iter()
                .map(String::len)
                .sum::<usize>()
                < 16 * 1024
        );
    }
}

#[cfg(test)]
mod aliases {
    use super::*;
    #[test]
    fn reserved_names_include_filesystem_case_aliases_without_hiding_ordinary_notes() {
        let id = "ABCDEF0123456789ABCDEF0123456789";
        for path in [
            ".SPIN/secret".to_string(),
            ".ſpin/secret".to_string(),
            format!("nested/.OPENWEBIDE-PROOF-{id}"),
            format!("nested/.OPENWEBIDE-CREATE-{id}-1023"),
            format!(".OpenWebIde/ReCoVeRy-{id}.OwNeR.PaRt/objects/original"),
        ] {
            assert!(private_workspace_path(&path), "{path}");
            assert!(crate::vfs::workspace_path(&path).is_err(), "{path}");
        }
        for path in [
            ".OpenWebIde/RECOVERY-NOTES".to_string(),
            format!(".OPENWEBIDE-CREATE-{id}-01"),
            format!(".OPENWEBIDE-CREATE-{id}-1024"),
        ] {
            assert!(!private_workspace_path(&path), "{path}");
            assert!(crate::vfs::workspace_path(&path).is_ok(), "{path}");
        }
    }
}
