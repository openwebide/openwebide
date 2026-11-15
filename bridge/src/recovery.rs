//! OS-owner CLI for native records whose original app issuer cannot recover them.
//! No HTTP endpoint, backend credential or user-selected namespace is accepted.
const HELP: &str = "Native workspace recovery (run as the bridge's OS account)\n\n\
Usage: openwebide-bridge recovery inspect\n\
       openwebide-bridge recovery retire <ID> --expected <DIGEST> --note <REVIEW>\n\n\
Inspect the original record and review its file and command effects first.\n\
Retirement releases only its native claim and permanently fences that transaction.\n\
It does not undo files, invent process cleanup, or settle original backend records.\n\
Active folder owners/watchers and unconfirmed command cleanup prevent retirement.\n\
Use normal app recovery when the original project and issuer remain available.\n";
#[derive(Debug, PartialEq, Eq)]
enum Request {
    Help,
    Inspect,
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
        [command, id, rest @ ..] if command == "retire" => {
            let mut expected = None;
            let mut note = None;
            for pair in rest.chunks(2) {
                match pair {
                    [flag, value] if flag == "--expected" && expected.is_none() => {
                        expected = Some(value.clone());
                    }
                    [flag, value] if flag == "--note" && note.is_none() => {
                        note = Some(value.clone());
                    }
                    _ => return Err(HELP.into()),
                }
            }
            Ok(Request::Retire {
                id: id.clone(),
                expected: expected.ok_or(HELP)?,
                note: note.ok_or(HELP)?,
            })
        }
        _ => Err(HELP.into()),
    }
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
    let namespace = Namespace::open_existing(&home()?)?;
    let value = match request {
        Request::Inspect => administration::inspect(&namespace)?,
        Request::Retire { id, expected, note } => {
            administration::retire(&namespace, &id, &expected, &note)?
        }
        Request::Help => unreachable!(),
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
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn inspection_does_not_initialize_an_absent_namespace() {
        let home = tempfile::tempdir().unwrap();
        assert!(execute(Request::Inspect, || Ok(home.path().into())).is_err());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
