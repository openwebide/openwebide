#![cfg(unix)]
use futures::StreamExt;
use openwebide_bridge::{
    exec::HostExecution,
    host_admin::{interactive::registry, ssh::SshHost},
};
use openwebide_core::{CommandOutcome, host_admin::*};
use std::{sync::Arc, time::Duration};

fn command(program: &str, args: &[&str]) -> HostCommand {
    HostCommand {
        program: program.into(),
        args: args.iter().map(|arg| (*arg).into()).collect(),
        cwd: "/home/admin".into(),
        elevated: false,
        interactive: false,
        timeout_seconds: 30,
    }
}
#[tokio::test]
#[ignore = "Run docker/host-admin-test/check.sh to provision an isolated SSH target and agent"]
async fn ssh_administration_inspects_and_changes_the_target_and_handles_interactive_sudo() {
    let host = SshHost::new(
        HostConnection {
            destination: "admin@127.0.0.1".into(),
            port: std::env::var("OPENWEBIDE_TEST_SSH_PORT")
                .unwrap()
                .parse()
                .unwrap(),
            known_hosts: std::fs::read_to_string(
                std::env::var("OPENWEBIDE_TEST_SSH_KNOWN_HOSTS").unwrap(),
            )
            .unwrap(),
            ..Default::default()
        },
        Arc::new(HostExecution),
    )
    .unwrap();
    let environment = inspect_host(&host).await.unwrap();
    assert!(environment.target.starts_with("administration-fixture@"));
    assert!(environment.info.os.as_deref().unwrap().contains("Debian"));
    assert!(environment.info.scope.contains("/home/admin"));
    assert!(
        environment.resources.iter().any(
            |resource| resource.kind == ResourceKind::Process && resource.name.contains("sshd")
        )
    );
    assert!(
        environment
            .resources
            .iter()
            .any(|resource| resource.kind == ResourceKind::Mount)
    );
    assert!(
        environment
            .relationships
            .iter()
            .any(|link| link.description == "stored on")
    );
    let changed=host.command(&command("sh", &["-c", "mkdir -p .config/openwebide-host-test; printf '%s' 'configuration from SSH' > .config/openwebide-host-test/config"])).await.unwrap();
    assert_eq!(changed.exit_code, Some(0));
    let verified = host
        .command(&command("cat", &[".config/openwebide-host-test/config"]))
        .await
        .unwrap();
    assert_eq!(verified.stdout, "configuration from SSH");
    assert_eq!(
        host.command(&command("rm", &["-rf", ".config/openwebide-host-test"]))
            .await
            .unwrap()
            .exit_code,
        Some(0)
    );
    let mut sudo = command("id", &["-u"]);
    sudo.elevated = true;
    sudo.interactive = true;
    let id = i64::from(rand::random::<u32>());
    let mut process = host
        .interactive(
            &sudo,
            HostCommandContext {
                user: 7,
                session: 8,
                operation: id,
                index: 0,
                verification: false,
            },
        )
        .unwrap();
    let Some(Ok(HostProcessEvent::InputReady(token))) = process.next().await else {
        panic!("Missing private terminal token")
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match process.next().await.unwrap().unwrap() {
                HostProcessEvent::Output(output) if output.to_lowercase().contains("password") => {
                    break;
                }
                HostProcessEvent::Finished(outcome) => {
                    panic!("Sudo exited without a prompt: {}", outcome.to_model_text())
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let prompt = HostInputKeyRequest {
        operation: id,
        token: token.clone(),
    };
    let key = registry().key(7, 8, &prompt).unwrap();
    let encrypted = |data: &str| {
        private_input::encrypt_reply(&key, 7, 8, &prompt, data, rand::random()).unwrap()
    };
    registry()
        .input(7, 8, encrypted("fixture-password\n"))
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let HostProcessEvent::Finished(outcome) = process.next().await.unwrap().unwrap() {
                break outcome;
            }
        }
    })
    .await
    .unwrap();
    let CommandOutcome {
        exit_code, stdout, ..
    } = outcome;
    assert_eq!(exit_code, Some(0));
    assert!(stdout.lines().any(|line| line.trim() == "0"));
    assert!(!stdout.contains("fixture-password"));
    drop(process);
    assert!(registry().input(7, 8, encrypted("late\n")).is_err());
}

#[tokio::test]
#[ignore = "Run docker/host-admin-test/check.sh for the isolated SSH updater fixture"]
async fn ssh_update_adapter_pins_both_deployments_and_survives_ssh_disconnect() {
    use openwebide_bridge::host_admin::updates::SshUpdates;
    use openwebide_core::host_admin::updates::*;
    let host = SshHost::new(
        HostConnection {
            destination: "admin@127.0.0.1".into(),
            port: std::env::var("OPENWEBIDE_TEST_SSH_PORT")
                .unwrap()
                .parse()
                .unwrap(),
            known_hosts: std::fs::read_to_string(
                std::env::var("OPENWEBIDE_TEST_SSH_KNOWN_HOSTS").unwrap(),
            )
            .unwrap(),
            ..HostConnection::default()
        },
        Arc::new(HostExecution),
    )
    .unwrap();
    host.command(&command("sh", &["-c", "mkdir -p /home/admin/update-fixture; printf 'KEEP=original\nOPENWEBIDE_IMAGE=old\n' > /home/admin/update-fixture/.env; printf '[Image]\nImage=old\n' > /home/admin/update-fixture/openwebide.image; touch /home/admin/update-fixture/compose.yml"])).await.unwrap();
    let adapter = SshUpdates(host.clone());
    let running = Build {
        version: "1.0.0-alpha.6".into(),
        commit: "a".repeat(40),
        schema: 58,
    };
    for (index, deployment) in [
        Deployment::Compose {
            directory: "/home/admin/update-fixture".into(),
            files: vec!["/home/admin/update-fixture/compose.yml".into()],
            service: "openwebide".into(),
        },
        Deployment::Quadlet {
            image_file: "/home/admin/update-fixture/openwebide.image".into(),
            service: "openwebide.service".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let settings = UpdateSettings {
            deployment,
            ..UpdateSettings::default()
        };
        let check = check(&adapter, settings.clone(), running.clone())
            .await
            .unwrap();
        assert!(check.available);
        let identity = if index == 0 { "d" } else { "e" }.repeat(32);
        let job = UpdateJob {
            id: identity.clone(),
            candidate: check.candidate,
            phase: UpdatePhase::Starting,
            detail: String::new(),
            settings_revision: 0,
            connection_revision: 0,
            created_at: 1,
        };
        start(&adapter, &settings, &running, &job).await.unwrap();
        // Each poll is a new SSH connection. The launching connection has already exited.
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let receipt = adapter.status(&identity).await.unwrap();
                assert_ne!(receipt.phase, UpdatePhase::Failed, "{}", receipt.detail);
                if receipt.phase == UpdatePhase::Succeeded {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        let path = if index == 0 {
            "/home/admin/update-fixture/.env"
        } else {
            "/home/admin/update-fixture/openwebide.image"
        };
        let updated = host.command(&command("cat", &[path])).await.unwrap().stdout;
        assert!(updated.contains(&job.candidate.image()));
        if index == 0 {
            assert!(updated.contains("KEEP=original"));
        }
        assert!(start(&adapter, &settings, &running, &job).await.is_err());
    }
}
