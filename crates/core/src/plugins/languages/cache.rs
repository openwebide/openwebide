//! Declarative cache-path policy, shared by all installed providers/hosts.
use super::{LanguageServer, text};
use std::collections::BTreeMap;

const TOKEN: &str = "${cache}";

fn reference(value: &str, argument: bool) -> Result<bool, String> {
    let Some((prefix, suffix)) = value.split_once(TOKEN) else {
        return Ok(false);
    };
    let flag = prefix
        .strip_prefix('-')
        .and_then(|value| value.strip_suffix('='));
    let valid_prefix = prefix.is_empty()
        || (argument
            && flag.is_some_and(|flag| {
                !flag.is_empty()
                    && flag
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
            }));
    if !valid_prefix
        || value.len() > 4096
        || suffix.contains(TOKEN)
        || (!suffix.is_empty()
            && (!suffix.starts_with('/')
                || suffix[1..].split('/').any(|part| {
                    part.is_empty()
                        || part == "."
                        || part == ".."
                        || !part.bytes().all(|ch| {
                            ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.')
                        })
                })))
    {
        return Err(
            "Cache references require one ${cache} token followed by normal relative path segments"
                .into(),
        );
    }
    Ok(true)
}
fn visit(
    value: &serde_json::Value,
    check: &mut impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    let mut pending = vec![(value, 0)];
    let mut count = 0;
    while let Some((value, depth)) = pending.pop() {
        count += 1;
        if count > 4096 || depth > 32 {
            return Err("Cache-bound server settings exceed their structure limit".into());
        }
        match value {
            serde_json::Value::String(value) => check(value)?,
            serde_json::Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => (),
        }
    }
    Ok(())
}
pub fn validate_server_cache(server: &LanguageServer) -> Result<(), String> {
    let Some(cache) = &server.cache else {
        return Ok(());
    };
    if cache.version_args.is_empty()
        || cache.version_args.len() > 16
        || cache
            .version_args
            .iter()
            .any(|arg| !text(arg, 4096) || arg.contains(TOKEN))
        || cache.environment.len() > 32
    {
        return Err("Invalid language-server cache version probe or environment".into());
    }
    let mut references = 0;
    let mut names = std::collections::BTreeSet::new();
    for (name, value) in &cache.environment {
        if name.is_empty()
            || name.len() > 128
            || !(name.as_bytes()[0].is_ascii_alphabetic() || name.starts_with('_'))
            || !name
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
            || !names.insert(name.to_ascii_uppercase())
            || !value.starts_with(TOKEN)
            || !reference(value, false)?
        {
            return Err(
                "Cache environment variables require valid names and ${cache} paths".into(),
            );
        }
        references += 1;
    }
    for arg in &server.args {
        if reference(arg, true)? {
            references += 1;
        }
    }
    let mut check = |value: &str| {
        if reference(value, false)? {
            references += 1;
        }
        Ok(())
    };
    visit(&server.initialization_options, &mut check)?;
    visit(&server.settings, &mut check)?;
    if references == 0 {
        return Err("A cache declaration must reference its directory".into());
    }
    Ok(())
}
pub struct BoundServer {
    pub server: LanguageServer,
    pub environment: BTreeMap<String, String>,
}
/// Resolve data only. The native adapter creates/owns the directory; no shell
/// expansion or arbitrary environment-variable interpolation is performed.
pub fn bind_server_cache(server: &LanguageServer, directory: &str) -> Result<BoundServer, String> {
    validate_server_cache(server)?;
    if directory.is_empty() || directory.chars().any(char::is_control) {
        return Err("The native cache path cannot be represented in server configuration".into());
    }
    let mut bound = server.clone();
    let Some(cache) = bound.cache.take() else {
        return Ok(BoundServer {
            server: bound,
            environment: BTreeMap::new(),
        });
    };
    for arg in &mut bound.args {
        *arg = arg.replace(TOKEN, directory);
    }
    fn replace(value: &mut serde_json::Value, directory: &str) {
        match value {
            serde_json::Value::String(value) => *value = value.replace(TOKEN, directory),
            serde_json::Value::Array(values) => {
                for value in values {
                    replace(value, directory);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values_mut() {
                    replace(value, directory);
                }
            }
            _ => (),
        }
    }
    replace(&mut bound.initialization_options, directory);
    replace(&mut bound.settings, directory);
    Ok(BoundServer {
        server: bound,
        environment: cache
            .environment
            .into_iter()
            .map(|(key, value)| (key, value.replace(TOKEN, directory)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cache_paths_bind_only_declared_fields_and_reject_traversal_or_unbounded_templates() {
        let server:LanguageServer=serde_json::from_value(json!({"command":"example","args":["--store=${cache}/index"],"cache":{"versionArgs":["--version"],"environment":{"EXAMPLE_CACHE":"${cache}/data"}},"initializationOptions":{"cacheDir":"${cache}"},"settings":{"nested":[{"path":"${cache}/state"}]}})).unwrap();
        let bound = bind_server_cache(&server, "/outside/project cache").unwrap();
        assert!(bound.server.cache.is_none());
        assert_eq!(bound.server.args[0], "--store=/outside/project cache/index");
        assert_eq!(
            bound.environment["EXAMPLE_CACHE"],
            "/outside/project cache/data"
        );
        assert_eq!(
            bound.server.initialization_options["cacheDir"],
            "/outside/project cache"
        );
        assert_eq!(
            bound.server.settings["nested"][0]["path"],
            "/outside/project cache/state"
        );
        for bad in [
            "${cache}/../source",
            "${cache}//x",
            "${cache}/x/./y",
            "${cache}suffix",
            "${cache}/x\\y",
            "${cache}/${cache}",
        ] {
            let mut invalid = server.clone();
            invalid.args[0] = bad.into();
            assert!(validate_server_cache(&invalid).is_err(), "{bad}");
        }
        let mut invalid = server.clone();
        invalid.cache.as_mut().unwrap().version_args.clear();
        assert!(validate_server_cache(&invalid).is_err());
        let mut invalid = server.clone();
        invalid
            .cache
            .as_mut()
            .unwrap()
            .environment
            .insert("bad=name".into(), "${cache}".into());
        assert!(validate_server_cache(&invalid).is_err());
    }
}
