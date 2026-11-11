//! One portable project-input pattern policy for every execution adapter.
use super::ProjectInputs;
use regex::{RegexSet, RegexSetBuilder};
use std::{
    collections::BTreeSet,
    sync::{LazyLock, Mutex},
};

const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_BYTES: usize = 512;
const MAX_VALIDATED: usize = 32;
static VALIDATED: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(Mutex::default);

pub fn validate_project_inputs(inputs: &ProjectInputs) -> Result<(), String> {
    validate_limits(inputs)?;
    let key = serde_json::to_string(inputs).map_err(|error| error.to_string())?;
    if VALIDATED.lock().unwrap().contains(&key) {
        return Ok(());
    }
    ProjectInputPatterns::new(inputs)?;
    let mut validated = VALIDATED.lock().unwrap();
    validated.insert(key);
    while validated.len() > MAX_VALIDATED {
        validated.pop_first();
    }
    Ok(())
}
fn validate_limits(inputs: &ProjectInputs) -> Result<(), String> {
    if inputs.files.is_empty()
        || inputs.files.len() > MAX_PATTERNS
        || inputs.exclude.len() > MAX_PATTERNS
    {
        return Err("Project inputs require 1–64 file patterns and at most 64 exclusions".into());
    }
    for patterns in [&inputs.files, &inputs.exclude] {
        if patterns.iter().collect::<BTreeSet<_>>().len() != patterns.len() {
            return Err("Project input patterns must not repeat".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ProjectInputPatterns {
    files: RegexSet,
    exclude: RegexSet,
    directories: RegexSet,
}
impl ProjectInputPatterns {
    pub fn new(inputs: &ProjectInputs) -> Result<Self, String> {
        validate_limits(inputs)?;
        fn compile(patterns: &[String]) -> Result<RegexSet, String> {
            let patterns = patterns
                .iter()
                .map(|pattern| expression(pattern))
                .collect::<Result<Vec<_>, _>>()?;
            RegexSetBuilder::new(patterns)
                .dot_matches_new_line(true)
                .size_limit(1024 * 1024)
                .dfa_size_limit(1024 * 1024)
                .build()
                .map_err(|_| "Invalid or oversized project input patterns".into())
        }
        let mut directories = BTreeSet::new();
        for pattern in &inputs.files {
            let parts = pattern.split('/').collect::<Vec<_>>();
            let count = if parts.last() == Some(&"**") {
                parts.len()
            } else {
                parts.len() - 1
            };
            for end in 1..=count {
                directories.insert(parts[..end].join("/"));
            }
        }
        Ok(Self {
            files: compile(&inputs.files)?,
            exclude: compile(&inputs.exclude)?,
            directories: compile(&directories.into_iter().collect::<Vec<_>>())?,
        })
    }
    pub fn includes_file(&self, path: &str) -> bool {
        crate::vfs::workspace_path(path).is_ok()
            && self.files.is_match(path)
            && !self.exclude.is_match(path)
            && !path
                .match_indices('/')
                .any(|(index, _)| self.excludes_directory(&path[..index]))
    }
    pub fn excludes_directory(&self, path: &str) -> bool {
        crate::vfs::workspace_path(path).is_err()
            || self.exclude.is_match(path)
            || self.exclude.is_match(format!("{path}/").as_str())
    }
    pub fn includes_directory(&self, path: &str) -> bool {
        crate::vfs::workspace_path(path).is_ok() && self.directories.is_match(path)
    }
}
fn expression(pattern: &str) -> Result<String, String> {
    if pattern.is_empty()
        || pattern.len() > MAX_PATTERN_BYTES
        || pattern.contains(['\\', ':', '?', '[', ']', '{', '}'])
        || pattern.chars().any(char::is_control)
    {
        return Err("Invalid project input path pattern".into());
    }
    let segments = pattern.split('/').collect::<Vec<_>>();
    let mut expression = String::from("^");
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        if segment.is_empty()
            || matches!(*segment, "." | "..")
            || (segment.contains("**") && *segment != "**")
        {
            return Err(
                "Project input patterns must use normalized relative paths and whole ** segments"
                    .into(),
            );
        }
        if *segment == "**" {
            expression.push_str(if last { ".*" } else { "(?:[^/]+/)*" });
        } else {
            expression.push_str(
                &segment
                    .split('*')
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join("[^/]*"),
            );
            if !last {
                expression.push('/');
            }
        }
    }
    expression.push('$');
    Ok(expression)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_inputs_always_exclude_runtime_recovery_payloads() {
        let patterns = ProjectInputPatterns::new(&ProjectInputs {
            files: vec!["**".into()],
            exclude: vec![],
        })
        .unwrap();
        for path in [
            ".spin",
            ".openwebide/recovery-0123456789abcdef0123456789abcdef",
            "nested/.openwebide/recovery-0123456789abcdef0123456789abcdef.init",
            "nested/.openwebide-create-0123456789abcdef0123456789abcdef-3",
            "nested/.openwebide-proof-0123456789abcdef0123456789abcdef",
        ] {
            assert!(!patterns.includes_directory(path));
            assert!(patterns.excludes_directory(path));
            assert!(!patterns.includes_file(&format!("{path}/settings.json")));
        }
        assert!(patterns.includes_file(".openwebide/recovery-notes/settings.json"));
    }
    #[test]
    fn project_inputs_match_root_nested_unicode_and_explicitly_excluded_trees() {
        let inputs = ProjectInputs {
            files: vec![
                "**/Cargo.toml".into(),
                "**/*.csproj".into(),
                ".cargo/config.toml".into(),
            ],
            exclude: vec!["**/target/**".into(), "vendor/**".into()],
        };
        let compiled = ProjectInputPatterns::new(&inputs).unwrap();
        for path in [
            "Cargo.toml",
            "crates/core/Cargo.toml",
            "App.csproj",
            "文😀/App.csproj",
            ".cargo/config.toml",
        ] {
            assert!(compiled.includes_file(path), "{path}");
        }
        for path in [
            "Cargo.toml.old",
            "crates/core/main.rs",
            "target/fixture/Cargo.toml",
            "src/target/fixture/Cargo.toml",
            "vendor/lib/App.csproj",
        ] {
            assert!(!compiled.includes_file(path), "{path}");
        }
        for path in ["target", "src/target", "vendor", "vendor/lib"] {
            assert!(compiled.excludes_directory(path), "{path}");
        }
        assert!(!compiled.excludes_directory("src"));
        assert!(!compiled.excludes_directory("vendor-other"));
        // A directory exclusion prunes its entire subtree, including when
        // evaluating a reviewed file plan without traversing the filesystem.
        let pruned = ProjectInputPatterns::new(&ProjectInputs {
            files: vec!["**/*.json".into()],
            exclude: vec!["generated".into(), "**/ignored".into()],
        })
        .unwrap();
        for path in [
            "generated/settings.json",
            "generated/nested/settings.json",
            "src/ignored/settings.json",
        ] {
            assert!(!pruned.includes_file(path), "{path}");
        }
        assert!(pruned.includes_file("generated-other/settings.json"));
    }
    #[test]
    fn project_inputs_reject_escapes_ambiguous_patterns_and_unbounded_metadata() {
        for pattern in [
            "",
            "/Cargo.toml",
            "../Cargo.toml",
            "src/../Cargo.toml",
            "src//Cargo.toml",
            "src/./Cargo.toml",
            "src/",
            "src\\Cargo.toml",
            "C:Cargo.toml",
            "src**/Cargo.toml",
            "**.json",
            "*.{js,ts}",
            "file?.json",
            "[ab].json",
            "file\nname",
            "file\n",
            "file\u{85}name",
            &"x".repeat(513),
        ] {
            assert!(
                validate_project_inputs(&ProjectInputs {
                    files: vec![pattern.into()],
                    exclude: vec![]
                })
                .is_err(),
                "{pattern}"
            );
        }
        assert!(validate_project_inputs(&ProjectInputs::default()).is_err());
        assert!(
            validate_project_inputs(&ProjectInputs {
                files: vec!["a".into(), "a".into()],
                exclude: vec![]
            })
            .is_err()
        );
        assert!(
            validate_project_inputs(&ProjectInputs {
                files: (0..65).map(|n| format!("file{n}")).collect(),
                exclude: vec![]
            })
            .is_err()
        );
        assert!(
            validate_project_inputs(&ProjectInputs {
                files: vec!["a".into()],
                exclude: vec!["../b".into()]
            })
            .is_err()
        );
    }
}
