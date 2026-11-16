//! OS-owner CLI for native records whose original app issuer cannot recover them.
//! No HTTP endpoint, backend credential or user-selected namespace is accepted.
const HELP: &str = "Native workspace recovery (run as the bridge's OS account)\n\n\
Usage: openwebide-bridge recovery inspect\n\
       openwebide-bridge recovery retire <ID> --expected <DIGEST> --note <REVIEW>\n\
       openwebide-bridge recovery upgrade inspect\n\
       openwebide-bridge recovery upgrade --expected <DIGEST> --note <REVIEW>\n\
       openwebide-bridge recovery upgrade cancel --expected <DIGEST> --note <REVIEW>\n\n\
Inspect the original record and review its file and command effects first.\n\
Retirement releases only its native claim and permanently fences that transaction.\n\
It does not undo files, invent process cleanup, or settle original backend records.\n\
Active folder owners/watchers and unconfirmed command cleanup prevent retirement.\n\
Use normal app recovery when the original project and issuer remain available.\n\
Upgrade inspection reads legacy volume identities without changing their state.\n";
#[derive(Debug, PartialEq, Eq)]
enum Request {
    Help,
    Inspect,
    InspectUpgrade,
    Upgrade {
        expected: String,
        note: String,
        cancel: bool,
    },
    Retire {
        id: String,
        expected: String,
        note: String,
    },
}
fn parse(args: &[String]) -> Result<Request, String> {
    match args {
        [command] if matches!(command.as_str(), "--help" | "-h") => Ok(Request::Help),
        [command] if command == "inspect" => Ok(Request::Inspect),
        [command, action] if command == "upgrade" && action == "inspect" => {
            Ok(Request::InspectUpgrade)
        }
        [command, id, rest @ ..] if command == "retire" => {
            let (expected, note) = review_args(rest)?;
            Ok(Request::Retire {
                id: id.clone(),
                expected,
                note,
            })
        }
        [command, rest @ ..] if command == "upgrade" => {
            let (cancel, rest) = match rest {
                [action, rest @ ..] if action == "cancel" => (true, rest),
                _ => (false, rest),
            };
            let (expected, note) = review_args(rest)?;
            Ok(Request::Upgrade {
                expected,
                note,
                cancel,
            })
        }
        _ => Err(HELP.into()),
    }
}
fn review_args(args: &[String]) -> Result<(String, String), String> {
    let mut expected = None;
    let mut note = None;
    for pair in args.chunks(2) {
        match pair {
            [flag, value] if flag == "--expected" && expected.is_none() => {
                expected = Some(value.clone());
            }
            [flag, value] if flag == "--note" && note.is_none() => note = Some(value.clone()),
            _ => return Err(HELP.into()),
        }
    }
    Ok((expected.ok_or(HELP)?, note.ok_or(HELP)?))
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn execute(
    request: Request,
    home: impl FnOnce() -> Result<std::path::PathBuf, String>,
) -> Result<String, String> {
    use crate::native_recovery::{Namespace, administration};
    if request == Request::Help {
        return Ok(HELP.into());
    }
    let home = home()?;
    if request == Request::InspectUpgrade {
        #[cfg(target_os = "macos")]
        return serde_json::to_string_pretty(&crate::native_recovery::upgrade::inspect(&home)?)
            .map_err(|error| error.to_string());
        #[cfg(not(target_os = "macos"))]
        return Err("Legacy volume-identity upgrades are only supported on macOS".into());
    }
    if let Request::Upgrade {
        ref expected,
        ref note,
        cancel,
    } = request
    {
        #[cfg(target_os = "macos")]
        return serde_json::to_string_pretty(&if cancel {
            crate::native_recovery::upgrade::cancel(&home, expected, note)?
        } else {
            crate::native_recovery::upgrade::execute(&home, expected, note)?
        })
        .map_err(|error| error.to_string());
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (expected, note, cancel);
            return Err("Legacy volume-identity upgrades are only supported on macOS".into());
        }
    }
    let namespace = Namespace::open_existing(&home)?;
    let value = match request {
        Request::Inspect => administration::inspect(&namespace)?,
        Request::Retire { id, expected, note } => {
            administration::retire(&namespace, &id, &expected, &note)?
        }
        Request::Help | Request::InspectUpgrade | Request::Upgrade { .. } => unreachable!(),
    };
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}
/// Exercise the CLI contract against a disposable native home in integration tests.
/// Normal distributions have no namespace-selection argument or equivalent API.
#[cfg(all(
    feature = "test-support",
    any(target_os = "linux", target_os = "macos")
))]
pub fn execute_in_test_home(home: &std::path::Path, args: &[String]) -> Result<String, String> {
    execute(parse(args)?, || Ok(home.into()))
}
/// Handle only the explicit recovery subcommand, before daemon initialization.
/// The installed native account selects its namespace through the normal policy.
#[must_use]
pub fn run_if_requested() -> Option<Result<(), String>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("recovery")) {
        return None;
    }
    Some((|| {
        let args = args
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| "Recovery arguments must be valid UTF-8".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request = parse(&args)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        println!("{}", execute(request, crate::native_recovery::home)?);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            if request != Request::Help {
                return Err(
                    "Native recovery administration is not supported on this platform".into(),
                );
            }
            println!("{HELP}");
        }
        Ok(())
    })())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retirement_requires_an_exact_snapshot_and_review_without_namespace_overrides() {
        for args in [
            vec!["upgrade"],
            vec!["upgrade", "cancel"],
            vec!["upgrade", "--expected", "digest"],
            vec!["retire", "id"],
            vec!["inspect", "--home", "/tmp/other"],
            vec![
                "retire",
                "id",
                "--expected",
                "digest",
                "--note",
                "review",
                "--expected",
                "again",
            ],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(parse(&["inspect".into()]).unwrap(), Request::Inspect);
        assert_eq!(
            parse(&["upgrade".into(), "inspect".into()]).unwrap(),
            Request::InspectUpgrade
        );
        assert_eq!(
            parse(&["upgrade", "--expected", "digest", "--note", "review"].map(String::from))
                .unwrap(),
            Request::Upgrade {
                expected: "digest".into(),
                note: "review".into(),
                cancel: false
            }
        );
        assert_eq!(
            parse(
                &[
                    "upgrade",
                    "cancel",
                    "--note",
                    "review",
                    "--expected",
                    "digest"
                ]
                .map(String::from)
            )
            .unwrap(),
            Request::Upgrade {
                expected: "digest".into(),
                note: "review".into(),
                cancel: true
            }
        );
        assert!(
            parse(&[
                "upgrade".into(),
                "inspect".into(),
                "--home".into(),
                "/tmp/other".into()
            ])
            .is_err()
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn inspection_does_not_initialize_an_absent_namespace() {
        let home = tempfile::tempdir().unwrap();
        assert!(execute(Request::Inspect, || Ok(home.path().into())).is_err());
        assert!(execute(Request::InspectUpgrade, || Ok(home.path().into())).is_err());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
