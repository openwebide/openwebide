//! Saved prompts and scheduling policy, shared by every execution host.
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schedule {
    Once {
        at: i64,
    },
    Cron {
        expression: String,
        timezone: String,
    },
}
impl Schedule {
    pub fn next_after(&self, after: i64) -> Result<Option<i64>, String> {
        match self {
            Self::Once { at } => Ok((*at > after).then_some(*at)),
            Self::Cron {
                expression,
                timezone,
            } => {
                if expression.len() > 128 || expression.split_whitespace().count() != 5 {
                    return Err(
                        "Use a five-field cron expression (minute hour day month weekday).".into(),
                    );
                }
                let zone = Tz::from_str(timezone).map_err(|_| "Choose a valid timezone.")?;
                let start = DateTime::<Utc>::from_timestamp(after, 0)
                    .ok_or("Invalid schedule timestamp.")?
                    .with_timezone(&zone);
                let cron = croner::Cron::from_str(expression)
                    .map_err(|error| format!("Invalid schedule: {error}"))?;
                let next = cron
                    .find_next_occurrence(&start, false)
                    .map_err(|error| format!("Schedule has no next occurrence: {error}"))?;
                Ok(Some(next.timestamp()))
            }
        }
    }
    pub fn validate(&self, now: i64) -> Result<(), String> {
        if self.next_after(now)?.is_none() {
            return Err("Choose a future date and time.".into());
        }
        Ok(())
    }
}
/// Session policy is resolved when an occurrence becomes due, not when saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTarget {
    #[default]
    Existing,
    New,
    Latest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDraft {
    #[serde(default)]
    pub session_target: SessionTarget,
    #[serde(default)]
    pub auto_title: bool,
    #[serde(default)]
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub session_id: i64,
    /// None resolves the session’s current model when the task runs.
    #[serde(default)]
    pub model: Option<crate::ModelSelection>,
    pub schedule: Schedule,
    pub enabled: bool,
}
impl TaskDraft {
    pub fn validate(&self, now: i64) -> Result<(), String> {
        if (!self.auto_title && self.title.trim().is_empty()) || self.title.chars().count() > 120 {
            return Err("Task titles need 1–120 characters.".into());
        }
        if self.prompt.trim().is_empty() || self.prompt.len() > 32 * 1024 {
            return Err("Task prompts need 1–32 KiB of text.".into());
        }
        if self.session_target == SessionTarget::Existing && self.session_id <= 0 {
            return Err("Choose a session for this task.".into());
        }
        if let Some(model) = &self.model
            && (model.server_id <= 0
                || model.model.trim().is_empty()
                || model.model.len() > 256
                || model.model.chars().any(char::is_control))
        {
            return Err("Choose a valid server and model for this task.".into());
        }
        self.schedule.validate(now)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskCommand {
    List,
    Monitor {
        #[serde(default)]
        session_id: i64,
        command: MonitorCommand,
    },
    Create {
        draft: TaskDraft,
    },
    Update {
        id: i64,
        revision: i64,
        draft: TaskDraft,
    },
    SetEnabled {
        id: i64,
        revision: i64,
        enabled: bool,
    },
    Delete {
        id: i64,
        revision: i64,
    },
}
impl TaskCommand {
    /// Translate UI intent into the public tool shape; source owns validation and behavior.
    pub fn plugin_call(&self) -> Result<crate::ToolCall, String> {
        let (name, arguments) = match self {
            Self::List => ("schedule_list", serde_json::json!({})),
            Self::Create { draft } => ("schedule_create", serde_json::json!({"draft":draft})),
            Self::Update {
                id,
                revision,
                draft,
            } => (
                "schedule_update",
                serde_json::json!({"id":id,"revision":revision,"draft":draft}),
            ),
            Self::SetEnabled {
                id,
                revision,
                enabled,
            } => (
                "schedule_set_enabled",
                serde_json::json!({"id":id,"revision":revision,"enabled":enabled}),
            ),
            Self::Delete { id, revision } => (
                "schedule_delete",
                serde_json::json!({"id":id,"revision":revision}),
            ),
            Self::Monitor {
                session_id,
                command,
            } => {
                let mut arguments =
                    serde_json::to_value(command).map_err(|error| error.to_string())?;
                arguments
                    .as_object_mut()
                    .ok_or("Invalid monitor command")?
                    .insert("session_id".into(), serde_json::json!(session_id));
                ("monitor", arguments)
            }
        };
        Ok(crate::ToolCall {
            id: "tasks-ui".into(),
            name: name.into(),
            arguments: arguments.to_string(),
        })
    }
}
/// Ephemeral follow-up checks belong to one conversation, not the saved-task list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonitorCommand {
    Start {
        prompt: String,
        delay_seconds: i64,
        #[serde(default = "monitor_interval")]
        interval_seconds: i64,
        #[serde(default = "monitor_checks")]
        max_checks: i64,
    },
    List {},
    Cancel {
        id: i64,
        revision: i64,
    },
}
const fn monitor_interval() -> i64 {
    600
}
const fn monitor_checks() -> i64 {
    1
}
impl MonitorCommand {
    pub fn draft(&self, session: i64, now: i64) -> Result<Option<TaskDraft>, String> {
        let Self::Start {
            prompt,
            delay_seconds,
            interval_seconds,
            max_checks,
        } = self
        else {
            return Ok(None);
        };
        if !(5..86400).contains(delay_seconds)
            || !(5..=86400).contains(interval_seconds)
            || !(1..=24).contains(max_checks)
            || delay_seconds.saturating_add(interval_seconds.saturating_mul(max_checks - 1))
                >= 86400
        {
            return Err("Monitor checks must fit before the 24-hour expiry, with delays of 5–86399 seconds, intervals of 5–86400 seconds and 1–24 checks.".into());
        }
        let draft = TaskDraft {
            session_target: SessionTarget::Existing,
            auto_title: false,
            title: "Monitor".into(),
            prompt: prompt.clone(),
            session_id: session,
            model: None,
            schedule: Schedule::Once {
                at: now.saturating_add(*delay_seconds),
            },
            enabled: true,
        };
        draft.validate(now)?;
        Ok(Some(draft))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionHost {
    pub id: String,
    pub name: String,
    pub last_seen: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBinding {
    pub host_id: String,
    pub path: String,
}
impl HostBinding {
    /// Preserve a mapped workspace root as an explicit working directory.
    pub fn new(host_id: String, path: String) -> Self {
        Self {
            host_id,
            path: if path.is_empty() { ".".into() } else { path },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: i64,
    pub revision: i64,
    pub project_id: Option<i64>,
    pub draft: TaskDraft,
    pub next_run: Option<i64>,
    pub host_id: String,
    pub host_available: bool,
    pub last_run: Option<TaskRun>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskRun {
    #[serde(default)]
    pub session_id: Option<i64>,
    pub id: i64,
    pub task_id: i64,
    pub due_at: i64,
    pub status: String,
    pub detail: String,
    pub message_id: Option<i64>,
    pub permission_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskDelivery {
    pub run_id: i64,
    pub task_id: i64,
    pub user_id: i64,
    pub session_id: i64,
    pub prompt: crate::QueuedPrompt,
    pub binding: Option<HostBinding>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchResult {
    #[serde(default)]
    pub permission_id: Option<String>,
    pub run_id: i64,
    pub status: String,
    pub detail: String,
}
impl DispatchResult {
    /// Match the persisted result limit without splitting a Unicode character.
    pub fn bounded_detail(detail: &str) -> String {
        let mut end = detail.len().min(1024);
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail[..end].to_owned()
    }
}
pub fn calendar_cron(expression: &str) -> Option<(String, Vec<u8>)> {
    let fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 || fields[2] != "*" || fields[3] != "*" {
        return None;
    }
    let minute = fields[0].parse::<u8>().ok()?;
    let hour = fields[1].parse::<u8>().ok()?;
    if minute > 59 || hour > 23 {
        return None;
    }
    let days = if fields[4] == "*" {
        (0..7).collect()
    } else {
        fields[4]
            .split(',')
            .map(str::parse::<u8>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?
    };
    if days.is_empty() || days.iter().any(|day| *day > 6) {
        return None;
    }
    Some((format!("{hour:02}:{minute:02}"), days))
}
pub fn weekly_cron(time: &str, weekdays: &[u8]) -> Result<String, String> {
    let (hour, minute) = time.split_once(':').ok_or("Choose a time.")?;
    let hour: u8 = hour.parse().map_err(|_| "Choose a time.")?;
    let minute: u8 = minute.parse().map_err(|_| "Choose a time.")?;
    if hour > 23 || minute > 59 || weekdays.is_empty() || weekdays.iter().any(|day| *day > 6) {
        return Err("Choose a time and at least one weekday.".into());
    }
    let mut days = weekdays.to_vec();
    days.sort_unstable();
    days.dedup();
    Ok(format!(
        "{minute} {hour} * * {}",
        days.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_host_binding_preserves_root_and_nested_workspace_mappings() {
        for (mapped, expected) in [("", "."), (".", "."), ("repos/project", "repos/project")] {
            let binding = HostBinding::new("paired-host".into(), mapped.into());
            assert_eq!(binding.host_id, "paired-host");
            assert_eq!(binding.path, expected);
            let restored: HostBinding =
                serde_json::from_str(&serde_json::to_string(&binding).unwrap()).unwrap();
            assert_eq!(restored, binding);
        }
    }
    #[test]
    fn task_ui_commands_preserve_public_sdk_shapes_without_feature_dispatch() {
        let call = TaskCommand::SetEnabled {
            id: 12,
            revision: 3,
            enabled: false,
        }
        .plugin_call()
        .unwrap();
        assert_eq!(call.name, "schedule_set_enabled");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap(),
            serde_json::json!({"id":12,"revision":3,"enabled":false})
        );
        let call = TaskCommand::Monitor {
            session_id: 9,
            command: MonitorCommand::Cancel {
                id: 12,
                revision: 4,
            },
        }
        .plugin_call()
        .unwrap();
        assert_eq!(call.name, "monitor");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap(),
            serde_json::json!({"action":"cancel","id":12,"revision":4,"session_id":9})
        );
        let draft:TaskDraft = serde_json::from_value(serde_json::json!({"title":"Task","prompt":"Work","session_id":1,"schedule":{"kind":"once","at":1},"enabled":false})).unwrap();
        for command in [
            TaskCommand::Create {
                draft: draft.clone(),
            },
            TaskCommand::Update {
                id: 12,
                revision: 4,
                draft,
            },
        ] {
            let call = command.plugin_call().unwrap();
            let args: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
            assert_eq!(args["draft"]["schedule"]["at"], 1);
            assert!(args.get("action").is_none());
        }
    }
    #[test]
    fn host_result_detail_obeys_the_byte_limit_for_unicode() {
        let detail = format!("{}{}", "a".repeat(1023), "😀".repeat(100));
        assert_eq!(DispatchResult::bounded_detail(&detail), "a".repeat(1023));
        assert_eq!(
            DispatchResult::bounded_detail("Build failed"),
            "Build failed"
        );
    }
    #[test]
    fn task_model_defaults_remain_compatible_and_reject_invalid_overrides() {
        let mut draft: TaskDraft = serde_json::from_value(serde_json::json!({"title":"Task","prompt":"Work","session_id":1,"schedule":{"kind":"once","at":60},"enabled":true})).unwrap();
        assert!(draft.model.is_none());
        draft.validate(0).unwrap();
        for (server_id, model) in [(0, "valid"), (1, " "), (1, "bad\nmodel")] {
            draft.model = Some(crate::ModelSelection {
                server_id,
                model: model.into(),
            });
            assert!(draft.validate(0).is_err());
        }
    }

    #[test]
    fn calendar_choices_and_cron_keep_wall_time_across_dst() {
        assert_eq!(weekly_cron("09:30", &[5, 1, 1]).unwrap(), "30 9 * * 1,5");
        assert!(weekly_cron("24:00", &[1]).is_err());
        let schedule = Schedule::Cron {
            expression: "0 9 * * *".into(),
            timezone: "America/Chicago".into(),
        };
        let before = DateTime::parse_from_rfc3339("2026-03-07T16:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(
            schedule.next_after(before).unwrap(),
            Some(
                DateTime::parse_from_rfc3339("2026-03-08T14:00:00Z")
                    .unwrap()
                    .timestamp()
            )
        );
        assert!(
            Schedule::Cron {
                expression: "* * * * * *".into(),
                timezone: "UTC".into()
            }
            .validate(before)
            .is_err()
        );
        assert_eq!(Schedule::Once { at: 100 }.next_after(100).unwrap(), None);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunControl {
    pub cancelled: bool,
    pub approved: Option<bool>,
}
