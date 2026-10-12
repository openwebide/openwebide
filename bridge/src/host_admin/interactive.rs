//! Ephemeral terminal input registry. User replies never enter chat or database records.
use crate::{
    exec::{SpawnSpec, proc::Signal},
    terminals::session::Session,
};
use openwebide_core::{
    BridgeServerMessage, CommandOutcome,
    host_admin::{
        HostCommandContext, HostInput, HostInputKey, HostInputKeyRequest, HostProcessEvent,
        HostProcessStream,
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

struct Entry {
    user: i64,
    session: i64,
    operation: i64,
    terminal: Arc<Session>,
    secrets: Mutex<Vec<String>>,
    recipient: Mutex<openwebide_core::host_admin::private_input::PrivateInputRecipient>,
}
#[derive(Default)]
pub struct InputRegistry {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}
pub fn registry() -> &'static InputRegistry {
    static REGISTRY: OnceLock<InputRegistry> = OnceLock::new();
    REGISTRY.get_or_init(InputRegistry::default)
}
impl InputRegistry {
    fn entry(
        &self,
        user: i64,
        session: i64,
        prompt: &HostInputKeyRequest,
    ) -> Result<Arc<Entry>, String> {
        prompt.validate()?;
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&prompt.token)
            .cloned()
            .ok_or("This host prompt is no longer active.")?;
        if entry.user != user || entry.session != session || entry.operation != prompt.operation {
            return Err("This input belongs to another host session.".into());
        }
        if !entry
            .terminal
            .running
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err("This host prompt has ended.".into());
        }
        Ok(entry)
    }
    pub fn key(
        &self,
        user: i64,
        session: i64,
        prompt: &HostInputKeyRequest,
    ) -> Result<HostInputKey, String> {
        let entry = self.entry(user, session, prompt)?;
        let key = entry.recipient.lock().unwrap().public_key();
        Ok(key)
    }
    pub fn input(&self, user: i64, session: i64, input: HostInput) -> Result<(), String> {
        let entry = self.entry(user, session, &input.prompt)?;
        // Serialize authentication, replay consumption, redaction registration and delivery.
        let mut recipient = entry.recipient.lock().unwrap();
        let data = recipient.open(user, session, &input)?;
        let secret = data.trim_end_matches(['\r', '\n']).to_owned();
        if !secret.is_empty() && !secret.chars().all(char::is_control) {
            let mut secrets = entry.secrets.lock().unwrap();
            if secrets.len() >= 128 {
                return Err("Too many replies for this host command.".into());
            }
            secrets.push(secret);
        }
        entry.terminal.try_send_input(data).map_err(str::to_owned)
    }
}
struct Guard {
    token: String,
    terminal: Arc<Session>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        registry().entries.lock().unwrap().remove(&self.token);
        if self
            .terminal
            .running
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.terminal.try_kill(Signal::Term);
        }
    }
}
pub fn start(
    execution: Arc<dyn crate::exec::ToolExecution>,
    spec: SpawnSpec,
    context: HostCommandContext,
) -> Result<HostProcessStream, String> {
    let timeout = spec.timeout;
    let token = format!("{}-{:032x}", context.operation, rand::random::<u128>());
    let terminal = execution
        .start_terminal(token.clone(), spec)
        .map_err(|error| error.to_string())?;
    let entry = Arc::new(Entry {
        user: context.user,
        session: context.session,
        operation: context.operation,
        terminal: terminal.clone(),
        secrets: Mutex::new(Vec::new()),
        recipient: Mutex::new(
            openwebide_core::host_admin::private_input::PrivateInputRecipient::new(
                context.user,
                context.session,
                HostInputKeyRequest {
                    operation: context.operation,
                    token: token.clone(),
                },
                rand::random(),
            ),
        ),
    });
    registry()
        .entries
        .lock()
        .unwrap()
        .insert(token.clone(), entry.clone());
    let guard = Guard {
        token: token.clone(),
        terminal: terminal.clone(),
    };
    let state = (
        guard,
        entry,
        0_u64,
        String::new(),
        Some(token),
        tokio::time::Instant::now() + timeout,
        false,
    );
    Ok(Box::pin(futures::stream::unfold(
        state,
        |state| async move {
            let (guard, entry, mut cursor, mut output, ready, deadline, finished) = state;
            if finished {
                return None;
            }
            if let Some(token) = ready {
                return Some((
                    Ok(HostProcessEvent::InputReady(token)),
                    (guard, entry, cursor, output, None, deadline, false),
                ));
            }
            loop {
                if tokio::time::Instant::now() >= deadline {
                    entry.terminal.try_kill(Signal::Term);
                    return Some((
                        Err("Interactive host command timed out.".into()),
                        (guard, entry, cursor, output, None, deadline, true),
                    ));
                }
                let (events, _) = entry.terminal.ring.lock().unwrap().read_after(cursor, 128);
                let mut code = None;
                let mut exited = false;
                let mut changed = false;
                for (seq, event) in events {
                    cursor = seq;
                    match event {
                        BridgeServerMessage::Output { data, .. } => {
                            output.push_str(&data);
                            changed = true;
                        }
                        BridgeServerMessage::Exited { exit_code, .. } => {
                            code = exit_code;
                            exited = true;
                        }
                        BridgeServerMessage::Error { message, .. } => {
                            output.push_str(&message);
                            changed = true;
                        }
                        _ => {}
                    }
                }
                let secrets = entry.secrets.lock().unwrap().clone();
                // Replace complete replies before bounding the buffer so truncation cannot
                // expose a suffix of a reply on the next poll.
                for secret in &secrets {
                    output = output.replace(secret, "[input hidden]");
                }
                let mut safe = output.clone();
                // Withhold only an unfinished secret suffix, including across PTY chunks.
                {
                    let tail = secrets
                        .iter()
                        .flat_map(|secret| {
                            secret
                                .char_indices()
                                .skip(1)
                                .map(move |(end, _)| &secret[..end])
                        })
                        .filter(|prefix| safe.ends_with(prefix))
                        .map(str::len)
                        .max()
                        .unwrap_or(0);
                    safe.truncate(safe.len() - tail);
                }

                if output.len() > 16384 {
                    let mut start = output.len() - 8192;
                    while !output.is_char_boundary(start) {
                        start += 1;
                    }
                    output = output[start..].to_owned();
                }
                if exited {
                    return Some((
                        Ok(HostProcessEvent::Finished(CommandOutcome {
                            exit_code: code,
                            stdout: safe,
                            stderr: String::new(),
                        })),
                        (guard, entry, cursor, output, None, deadline, true),
                    ));
                }
                if changed {
                    return Some((
                        Ok(HostProcessEvent::Output(safe)),
                        (guard, entry, cursor, output, None, deadline, false),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures::StreamExt;
    #[tokio::test]
    async fn private_prompt_input_is_scoped_redacted_and_invalidated_after_exit() {
        let spec=SpawnSpec::shell("stty -echo; printf 'Password: '; IFS= read -r answer; printf '\\nreceived: %s\\n' \"$answer\"".into(),std::env::temp_dir(),30,std::future::pending());
        let mut stream = start(
            Arc::new(crate::exec::HostExecution),
            spec,
            HostCommandContext {
                user: 7,
                session: 8,
                operation: 9,
                index: 0,
                verification: false,
            },
        )
        .unwrap();
        let Some(Ok(HostProcessEvent::InputReady(token))) = stream.next().await else {
            panic!("Expected an interactive input token")
        };
        let prompt = HostInputKeyRequest {
            operation: 9,
            token: token.clone(),
        };
        assert!(registry().key(6, 8, &prompt).is_err());
        let key = registry().key(7, 8, &prompt).unwrap();
        let encrypt = |data: &str| {
            openwebide_core::host_admin::private_input::encrypt_reply(
                &key,
                7,
                8,
                &prompt,
                data,
                rand::random(),
            )
            .unwrap()
        };
        assert!(registry().input(6, 8, encrypt("forbidden\n")).is_err());
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let HostProcessEvent::Output(output) = event
                && output.contains("Password:")
            {
                break;
            }
        }
        let reply = encrypt("sensitive-value\n");
        registry().input(7, 8, reply.clone()).unwrap();
        assert!(registry().input(7, 8, reply).is_err());
        let mut finished = false;
        while let Some(event) = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .unwrap()
        {
            match event.unwrap() {
                HostProcessEvent::Output(output) => {
                    assert!(!output.contains("sensitive"));
                }
                HostProcessEvent::Finished(outcome) => {
                    assert_eq!(outcome.exit_code, Some(0));
                    assert!(outcome.stdout.contains("[input hidden]"));
                    assert!(!outcome.stdout.contains("sensitive-value"));
                    finished = true;
                }
                HostProcessEvent::InputReady(_) => {}
            }
        }
        assert!(finished);
        assert!(registry().key(7, 8, &prompt).is_err());
        assert!(registry().input(7, 8, encrypt("late\n")).is_err());
    }
}
