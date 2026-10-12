//! Host administration policy shared by UI, agent tools and execution transports.
use crate::{CommandOutcome, HostInfo};
use serde::{Deserialize, Serialize};
use std::future::Future;

pub const MAX_RESOURCES: usize = 256;
pub const MAX_STEPS: usize = 16;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostEnvironment {
    /// Stable execution-host identity, never the browser or model server.
    pub target: String,
    pub boot_id: String,
    pub info: HostInfo,
    pub resources: Vec<HostResource>,
    pub relationships: Vec<HostRelationship>,
    pub notes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Process,
    Service,
    Container,
    Port,
    Mount,
    Disk,
    Configuration,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostResource {
    pub id: String,
    pub kind: ResourceKind,
    pub name: String,
    pub status: String,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRelationship {
    pub from: String,
    pub to: String,
    pub description: String,
}

/// Exact argv, directory, timeout and elevation are part of the approved plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub elevated: bool,
    #[serde(default = "interactive_default")]
    pub interactive: bool,
    #[serde(default = "command_timeout")]
    pub timeout_seconds: u64,
}
fn interactive_default() -> bool {
    true
}
fn command_timeout() -> u64 {
    300
}
impl HostCommand {
    pub fn validate(&self) -> Result<(), String> {
        if self.program.trim().is_empty()
            || self.program.starts_with('-')
            || self.program.len() > 4096
        {
            return Err("Provide a command program (up to 4096 bytes).".into());
        }
        if self.args.len() > 128 || self.args.iter().map(String::len).sum::<usize>() > 64 * 1024 {
            return Err("Command arguments exceed the host operation limit.".into());
        }
        if !self.cwd.starts_with('/')
            && !(self.cwd.as_bytes().get(1) == Some(&b':') && self.cwd.len() > 2)
        {
            return Err("Host commands require an absolute working directory.".into());
        }
        if self.cwd.len() > 4096
            || std::iter::once(&self.program)
                .chain(std::iter::once(&self.cwd))
                .chain(&self.args)
                .any(|value| value.contains('\0'))
        {
            return Err(
                "Command paths/arguments cannot contain NUL bytes or oversized paths.".into(),
            );
        }
        if !(1..=3600).contains(&self.timeout_seconds) {
            return Err("Host command timeout must be between 1 and 3600 seconds.".into());
        }
        Ok(())
    }
    pub fn preview(&self) -> String {
        format!(
            "{}{}\nArguments: {}\nDirectory: {}\nTimeout: {}s",
            if self.elevated {
                if self.interactive {
                    "sudo -- "
                } else {
                    "sudo -n -- "
                }
            } else {
                ""
            },
            self.program,
            serde_json::to_string(&self.args).unwrap_or_default(),
            self.cwd,
            self.timeout_seconds
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPlan {
    pub target: String,
    pub title: String,
    pub steps: Vec<HostCommand>,
    #[serde(default)]
    pub checks: Vec<HostCommand>,
    /// An approved reboot is never automatically replayed after reconnect.
    #[serde(default)]
    pub expects_reboot: bool,
}
impl HostPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.target.is_empty()
            || self.target.len() > 512
            || self.title.trim().is_empty()
            || self.title.len() > 256
        {
            return Err("Provide a host target and an operation title (up to 256 bytes).".into());
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS || self.checks.len() > MAX_STEPS {
            return Err(format!(
                "Provide 1–{MAX_STEPS} steps and at most {MAX_STEPS} verification commands."
            ));
        }
        for command in self.steps.iter().chain(&self.checks) {
            command.validate()?;
        }
        Ok(())
    }
    pub fn preview(&self) -> String {
        let mut text = format!("{}\nHost: {}\n", self.title, self.target);
        for (index, step) in self.steps.iter().enumerate() {
            text.push_str(&format!("\nStep {}\n{}\n", index + 1, step.preview()));
        }
        for check in &self.checks {
            text.push_str(&format!("\nVerification\n{}\n", check.preview()));
        }
        if self.expects_reboot {
            text.push_str(
                "\nReboot expected. Mutation steps will not be replayed after restart.\n",
            );
        }
        text
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Prepared,
    Running,
    AwaitingReconnect,
    Succeeded,
    Failed,
    Interrupted,
}
impl OperationState {
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::AwaitingReconnect)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostOperation {
    pub id: i64,
    pub user_id: i64,
    pub session_id: i64,
    pub request_id: String,
    pub plan: HostPlan,
    pub boot_id: String,
    pub connection_revision: i64,
    pub steps_started: usize,
    pub state: OperationState,
    #[serde(rename = "private_reply_token", alias = "input_token")]
    pub input_token: Option<String>,
    pub live_output: String,
    pub outputs: Vec<HostCommandResult>,
    pub detail: String,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostCommandResult {
    pub verification: bool,
    pub index: usize,
    pub outcome: CommandOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostRequest {
    Inspect,
    Context,
    Prepare { request_id: String, plan: HostPlan },
    Apply { id: i64 },
    Operations,
    Operation { id: i64 },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HostResponse {
    Environment(HostEnvironment),
    Operation(HostOperation),
    Operations(Vec<HostOperation>),
}

/// Adapters supply only process and journal primitives. Workflow stays here.
pub trait HostOperationAdapter: Send + Sync {
    fn command_stream(
        &self,
        command: &HostCommand,
        _context: HostCommandContext,
    ) -> impl Future<Output = Result<HostProcessStream, String>> + Send {
        async move {
            let outcome = self.command(command).await?;
            Ok(Box::pin(futures::stream::once(async move {
                Ok(HostProcessEvent::Finished(outcome))
            })) as HostProcessStream)
        }
    }
    fn command(
        &self,
        command: &HostCommand,
    ) -> impl Future<Output = Result<CommandOutcome, String>> + Send;
    fn save(&self, operation: &HostOperation) -> impl Future<Output = Result<(), String>> + Send;
    fn now(&self) -> i64;
}
/// Called only after an atomic, durable claim of an explicitly approved operation.
/// Never replays mutation commands while recovering an interrupted operation.
pub async fn execute_operation<A: HostOperationAdapter>(
    adapter: &A,
    operation: &mut HostOperation,
    recovery: bool,
) -> Result<(), String> {
    if recovery
        && (!operation.plan.expects_reboot
            || operation.steps_started != operation.plan.steps.len()
            || operation.plan.checks.is_empty())
    {
        operation.state = OperationState::Interrupted;
        operation.input_token = None;
        operation.detail = "Bridge restarted; inspect the host before preparing another operation. Mutation steps were not replayed.".into();
        operation.updated_at = adapter.now();
        return adapter.save(operation).await;
    }
    if !recovery {
        for (index, command) in operation.plan.steps.clone().iter().enumerate() {
            let mut outcome = match run_command(adapter, operation, command, false, index).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    return finish(adapter, operation, OperationState::Failed, error).await;
                }
            };
            retained_output(&mut outcome.stdout, MAX_OUTPUT_BYTES / (MAX_STEPS * 4));
            retained_output(&mut outcome.stderr, MAX_OUTPUT_BYTES / (MAX_STEPS * 4));
            let ok = outcome.exit_code == Some(0);
            operation.outputs.push(HostCommandResult {
                verification: false,
                index,
                outcome,
            });
            operation.updated_at = adapter.now();
            adapter.save(operation).await?;
            if !ok {
                let state = if operation.plan.expects_reboot
                    && index + 1 == operation.plan.steps.len()
                    && matches!(outcome_exit(operation), None | Some(255))
                {
                    OperationState::AwaitingReconnect
                } else {
                    OperationState::Failed
                };
                return finish(
                    adapter,
                    operation,
                    state,
                    "Command did not report success; review retained output.".into(),
                )
                .await;
            }
        }
        if operation.plan.expects_reboot {
            return finish(
                adapter,
                operation,
                OperationState::AwaitingReconnect,
                "Waiting for a new boot before verification.".into(),
            )
            .await;
        }
    }
    for (index, check) in operation.plan.checks.clone().iter().enumerate() {
        let mut outcome = match run_command(adapter, operation, check, true, index).await {
            Ok(outcome) => outcome,
            Err(error) => return finish(adapter, operation, OperationState::Failed, error).await,
        };
        retained_output(&mut outcome.stdout, MAX_OUTPUT_BYTES / (MAX_STEPS * 4));
        retained_output(&mut outcome.stderr, MAX_OUTPUT_BYTES / (MAX_STEPS * 4));
        let ok = outcome.exit_code == Some(0);
        operation.outputs.push(HostCommandResult {
            verification: true,
            index,
            outcome,
        });
        operation.updated_at = adapter.now();
        adapter.save(operation).await?;
        if !ok {
            return finish(
                adapter,
                operation,
                OperationState::Failed,
                "Verification failed; review retained output.".into(),
            )
            .await;
        }
    }
    finish(
        adapter,
        operation,
        OperationState::Succeeded,
        if operation.plan.checks.is_empty() {
            "Commands completed; no verification commands were provided.".into()
        } else {
            "Commands and verification completed.".into()
        },
    )
    .await
}
async fn finish<A: HostOperationAdapter>(
    adapter: &A,
    operation: &mut HostOperation,
    state: OperationState,
    detail: String,
) -> Result<(), String> {
    operation.input_token = None;
    operation.state = state;
    operation.detail = detail;
    operation.updated_at = adapter.now();
    adapter.save(operation).await
}

pub fn reconnect_expired(operation: &HostOperation, now: i64) -> bool {
    operation.plan.expects_reboot && now.saturating_sub(operation.updated_at) >= 3600
}

/// Reconcile persisted state against the observed machine, never replaying mutation steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRecovery {
    Wait,
    Verify,
    Interrupt,
}
pub fn host_recovery(
    operation: &HostOperation,
    target: &str,
    boot_id: &str,
    connection_revision: i64,
    now: i64,
) -> HostRecovery {
    if operation.plan.target != target
        || operation.connection_revision != connection_revision
        || reconnect_expired(operation, now)
    {
        return HostRecovery::Interrupt;
    }
    if operation.plan.expects_reboot && operation.boot_id != boot_id {
        return HostRecovery::Verify;
    }
    if operation.state == OperationState::AwaitingReconnect {
        return HostRecovery::Wait;
    }
    HostRecovery::Interrupt
}

/// Internal journal protocol, accepted only from the authenticated server bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostJournalCommand {
    Connection,
    Prepare {
        user: i64,
        session: i64,
        request_id: String,
        plan: HostPlan,
        boot_id: String,
        connection_revision: i64,
    },
    Get {
        user: i64,
        session: i64,
        id: i64,
    },
    List {
        user: i64,
        session: i64,
    },
    Claim {
        user: i64,
        session: i64,
        id: i64,
        target: String,
        boot_id: String,
        connection_revision: i64,
    },
    Save {
        operation: HostOperation,
    },
    Active,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HostJournalResult {
    Connection(HostConnection),
    Operation(Box<HostOperation>),
    Operations(Vec<HostOperation>),
    Saved,
}

/// Merge partial adapter samples into a single bounded environment map.
/// Missing collectors remain visible as notes; relationships never dangle.
pub fn environment_map(
    target: String,
    boot_id: String,
    info: HostInfo,
    samples: Vec<Result<HostSample, String>>,
) -> HostEnvironment {
    let mut environment = HostEnvironment {
        target,
        boot_id,
        info,
        resources: Vec::new(),
        relationships: Vec::new(),
        notes: Vec::new(),
    };
    for sample in samples {
        match sample {
            Ok((resources, relationships)) => {
                environment
                    .resources
                    .extend(resources.into_iter().take(MAX_RESOURCES));
                environment
                    .relationships
                    .extend(relationships.into_iter().take(MAX_RESOURCES * 4));
            }
            Err(error) => environment.notes.push(error),
        }
    }
    environment.resources.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| (a.status == "runtime source").cmp(&(b.status == "runtime source")))
    });
    environment.resources.dedup_by(|a, b| a.id == b.id);
    // Relate runtime source paths to a containing filesystem only where its mount was observed.
    for source in environment.resources.iter().filter(|resource| {
        resource.kind == ResourceKind::Mount && resource.status == "runtime source"
    }) {
        let parent = environment
            .resources
            .iter()
            .filter(|resource| {
                resource.kind == ResourceKind::Mount
                    && resource.status == "mounted"
                    && (resource.name == "/"
                        || source
                            .name
                            .strip_prefix(&resource.name)
                            .is_some_and(|suffix| suffix.starts_with('/')))
            })
            .max_by_key(|resource| resource.name.len());
        if let Some(parent) = parent {
            environment.relationships.push(HostRelationship {
                from: source.id.clone(),
                to: parent.id.clone(),
                description: "path under observed mount".into(),
            });
        }
    }
    // Preserve actionable resources before a long process list consumes the inventory.
    environment
        .resources
        .sort_by_key(|resource| match resource.kind {
            ResourceKind::Configuration => 0,
            ResourceKind::Container => 1,
            ResourceKind::Disk | ResourceKind::Mount => 2,
            ResourceKind::Service => 3,
            ResourceKind::Port => 4,
            ResourceKind::Process => 5,
        });
    if environment.resources.len() > MAX_RESOURCES {
        environment.notes.push(format!("Inventory limited to {MAX_RESOURCES} resources; approved commands can inspect omitted resources."));
        environment.resources.truncate(MAX_RESOURCES);
    }
    let ids = environment
        .resources
        .iter()
        .map(|resource| resource.id.clone())
        .collect::<std::collections::HashSet<_>>();
    environment
        .relationships
        .retain(|relationship| ids.contains(&relationship.from) && ids.contains(&relationship.to));
    environment
        .relationships
        .sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    environment
        .relationships
        .dedup_by(|a, b| a.from == b.from && a.to == b.to);
    environment
}
fn retained_output(text: &mut String, limit: usize) {
    if text.len() > limit {
        let marker = "\n[… output omitted …]\n";
        let mut boundary = limit.saturating_sub(marker.len());
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
        text.push_str(marker);
    }
}

fn retained_live_output(text: &mut String) {
    const LIMIT: usize = 8192;
    const MARKER: &str = "[… earlier output omitted …]\n";
    if text.len() > LIMIT {
        let mut start = text.len() - (LIMIT - MARKER.len());
        while !text.is_char_boundary(start) {
            start += 1;
        }
        *text = format!("{MARKER}{}", &text[start..]);
    }
}

/// Host feature facade. Adapters supply metadata, persistence and resident scheduling.
pub trait HostAdapter: Send + Sync {
    fn connection_revision(&self) -> i64;
    fn identity(&self) -> impl Future<Output = Result<(String, String), String>> + Send;
    fn overview(&self) -> impl Future<Output = Result<HostEnvironment, String>> + Send;
    fn environment(&self) -> impl Future<Output = Result<HostEnvironment, String>> + Send;
    fn journal(
        &self,
        command: &HostJournalCommand,
    ) -> impl Future<Output = Result<HostJournalResult, String>> + Send;
    fn launch(&self, operation: HostOperation) -> impl Future<Output = Result<(), String>> + Send;
}
fn journal_operation(result: HostJournalResult) -> Result<HostOperation, String> {
    match result {
        HostJournalResult::Operation(operation) => Ok(*operation),
        _ => Err("Invalid host journal response".into()),
    }
}
/// Single entry point for host policy, irrespective of the process/transport adapter.
pub async fn host_request<A: HostAdapter>(
    adapter: &A,
    user: i64,
    session: i64,
    request: HostRequest,
) -> Result<HostResponse, String> {
    // Validates ownership and project-less scope before any host inspection or mutation.
    let operations = adapter
        .journal(&HostJournalCommand::List { user, session })
        .await?;
    match request {
        HostRequest::Context => Ok(HostResponse::Environment(adapter.overview().await?)),
        HostRequest::Inspect => Ok(HostResponse::Environment(adapter.environment().await?)),
        HostRequest::Operations => match operations {
            HostJournalResult::Operations(operations) => Ok(HostResponse::Operations(operations)),
            _ => Err("Invalid host journal response".into()),
        },
        HostRequest::Operation { id } => Ok(HostResponse::Operation(journal_operation(
            adapter
                .journal(&HostJournalCommand::Get { user, session, id })
                .await?,
        )?)),
        HostRequest::Prepare { request_id, plan } => {
            plan.validate()?;
            let (target, boot_id) = adapter.identity().await?;
            if plan.target != target {
                return Err(
                    "Host target changed; inspect the host before preparing an operation.".into(),
                );
            }
            let result = adapter
                .journal(&HostJournalCommand::Prepare {
                    user,
                    session,
                    request_id,
                    plan,
                    boot_id,
                    connection_revision: adapter.connection_revision(),
                })
                .await?;
            Ok(HostResponse::Operation(journal_operation(result)?))
        }
        HostRequest::Apply { id } => {
            let existing = journal_operation(
                adapter
                    .journal(&HostJournalCommand::Get { user, session, id })
                    .await?,
            )?;
            let (target, boot_id) = adapter.identity().await?;
            if existing.plan.target != target {
                return Err("This operation belongs to another host.".into());
            }
            if existing.state != OperationState::Prepared {
                return Ok(HostResponse::Operation(existing));
            }
            let claimed = journal_operation(
                adapter
                    .journal(&HostJournalCommand::Claim {
                        user,
                        session,
                        id,
                        target,
                        boot_id,
                        connection_revision: adapter.connection_revision(),
                    })
                    .await?,
            )?;
            let response = claimed.clone();
            adapter.launch(claimed).await?;
            Ok(HostResponse::Operation(response))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostQuery {
    Processes,
    Services,
    Containers,
    Ports,
    Storage,
    Configuration,
}
pub type HostSample = (Vec<HostResource>, Vec<HostRelationship>);
/// Each collector returns normalized primitives; partial failure policy lives here.
pub trait HostInventoryAdapter: Send + Sync {
    fn overview(&self) -> impl Future<Output = Result<HostEnvironment, String>> + Send;
    fn query(&self, query: HostQuery) -> impl Future<Output = Result<HostSample, String>> + Send;
}
pub async fn inspect_host<A: HostInventoryAdapter>(adapter: &A) -> Result<HostEnvironment, String> {
    let overview = adapter.overview().await?;
    let mut samples = Vec::new();
    for query in [
        HostQuery::Processes,
        HostQuery::Services,
        HostQuery::Containers,
        HostQuery::Ports,
        HostQuery::Storage,
        HostQuery::Configuration,
    ] {
        samples.push(adapter.query(query).await);
    }
    let mut environment =
        environment_map(overview.target, overview.boot_id, overview.info, samples);
    environment.notes.extend(overview.notes);
    Ok(environment)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    #[default]
    Linux,
    Macos,
    Windows,
}
/// Shared server configuration. Authentication uses the bridge's SSH agent/keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConnection {
    pub enabled: bool,
    #[serde(default)]
    pub username: String,
    pub destination: String,
    pub port: u16,
    pub platform: HostPlatform,
    /// Optional public known_hosts entries, pinned in the database.
    pub known_hosts: String,
    #[serde(default)]
    pub revision: i64,
}
impl Default for HostConnection {
    fn default() -> Self {
        Self {
            enabled: true,
            username: String::new(),
            destination: String::new(),
            port: 22,
            platform: HostPlatform::Linux,
            known_hosts: String::new(),
            revision: 0,
        }
    }
}
impl HostConnection {
    pub fn is_configured(&self) -> bool {
        self.enabled && (!self.destination.is_empty() || !self.username.is_empty())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled
            && ((self.destination.is_empty() && self.username.is_empty())
                || self.destination.len() > 256
                || self.destination.starts_with('-')
                || !self
                    .destination
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "._-@[]:".contains(ch)))
        {
            return Err("Provide an SSH username or an address override.".into());
        }
        if self.username.len() > 128
            || self.username.starts_with('-')
            || !self
                .username
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
        {
            return Err("Provide a valid SSH username.".into());
        }
        if self.port == 0 || self.known_hosts.len() > 64 * 1024 || self.known_hosts.contains('\0') {
            return Err(
                "Provide a nonzero SSH port and at most 64 KiB of public host keys.".into(),
            );
        }
        Ok(())
    }

    /// Transport supplies its host address; policy and legacy override handling stay shared.
    pub fn ssh_destination(&self, default_host: &str) -> Result<String, String> {
        self.validate()?;
        let address = if self.destination.is_empty() {
            default_host
        } else {
            &self.destination
        };
        if address.is_empty()
            || address.len() > 256
            || address.starts_with('-')
            || !address
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "._-@[]:".contains(ch))
        {
            return Err("The bridge's default SSH host address is invalid.".into());
        }
        Ok(if self.username.is_empty() || address.contains('@') {
            address.to_owned()
        } else {
            format!("{}@{address}", self.username)
        })
    }
}

#[derive(Debug, Clone)]
pub struct HostCommandContext {
    pub user: i64,
    pub session: i64,
    pub operation: i64,
    pub index: usize,
    pub verification: bool,
}
#[derive(Debug, Clone)]
pub enum HostProcessEvent {
    InputReady(String),
    Output(String),
    Finished(CommandOutcome),
}
pub type HostProcessStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<HostProcessEvent, String>> + Send>>;
async fn run_command<A: HostOperationAdapter>(
    adapter: &A,
    operation: &mut HostOperation,
    command: &HostCommand,
    verification: bool,
    index: usize,
) -> Result<CommandOutcome, String> {
    use futures::StreamExt;
    let context = HostCommandContext {
        user: operation.user_id,
        session: operation.session_id,
        operation: operation.id,
        index,
        verification,
    };
    operation.input_token = None;
    if !verification {
        operation.steps_started = index + 1;
    }
    operation.live_output.clear();
    operation.detail = format!(
        "Running {} {} of {}",
        if verification { "verification" } else { "step" },
        index + 1,
        if verification {
            operation.plan.checks.len()
        } else {
            operation.plan.steps.len()
        }
    );
    operation.updated_at = adapter.now();
    adapter.save(operation).await?;
    let mut process = adapter.command_stream(command, context).await?;
    while let Some(event) = process.next().await {
        match event? {
            HostProcessEvent::InputReady(token) => operation.input_token = Some(token),
            HostProcessEvent::Output(output) => {
                operation.live_output = output;
                retained_live_output(&mut operation.live_output);
            }
            HostProcessEvent::Finished(outcome) => {
                operation.input_token = None;
                return Ok(outcome);
            }
        }
        operation.updated_at = adapter.now();
        adapter.save(operation).await?;
    }
    Err("Host process ended without an exit result.".into())
}
pub mod private_input;
pub use private_input::{HostInput, HostInputKey, HostInputKeyRequest};

fn outcome_exit(operation: &HostOperation) -> Option<i32> {
    operation
        .outputs
        .last()
        .and_then(|result| result.outcome.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    fn command() -> HostCommand {
        HostCommand {
            program: "printf".into(),
            args: vec!["hello".into()],
            cwd: "/tmp".into(),
            elevated: false,
            interactive: false,
            timeout_seconds: 30,
        }
    }
    fn operation() -> HostOperation {
        HostOperation {
            id: 1,
            user_id: 1,
            session_id: 1,
            request_id: "one".into(),
            plan: HostPlan {
                target: "physical-host".into(),
                title: "Configure host".into(),
                steps: vec![command(), command()],
                checks: vec![command()],
                expects_reboot: false,
            },
            boot_id: "boot".into(),
            connection_revision: 1,
            steps_started: 0,
            state: OperationState::Running,
            input_token: None,
            live_output: String::new(),
            outputs: vec![],
            detail: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }
    struct Runtime {
        commands: Mutex<usize>,
        saved: Mutex<Vec<HostOperation>>,
        fail_command: bool,
        fail_save: bool,
    }
    impl HostOperationAdapter for Runtime {
        async fn command(&self, _command: &HostCommand) -> Result<CommandOutcome, String> {
            *self.commands.lock().unwrap() += 1;
            Ok(CommandOutcome {
                exit_code: Some(if self.fail_command { 1 } else { 0 }),
                stdout: "é".repeat(10000),
                stderr: String::new(),
            })
        }
        async fn save(&self, operation: &HostOperation) -> Result<(), String> {
            if self.fail_save {
                return Err("database unavailable".into());
            }
            self.saved.lock().unwrap().push(operation.clone());
            Ok(())
        }
        fn now(&self) -> i64 {
            2
        }
    }
    fn runtime(fail_command: bool, fail_save: bool) -> Runtime {
        Runtime {
            commands: Mutex::new(0),
            saved: Mutex::new(vec![]),
            fail_command,
            fail_save,
        }
    }
    #[test]
    fn ssh_destination_defaults_overrides_and_legacy_settings() {
        let connection = HostConnection {
            username: "admin".into(),
            ..Default::default()
        };
        assert!(connection.is_configured());
        for host in [
            "host.docker.internal",
            "host.containers.internal",
            "127.0.0.1",
        ] {
            assert_eq!(
                connection.ssh_destination(host).unwrap(),
                format!("admin@{host}")
            );
        }
        let mut custom = connection.clone();
        custom.destination = "other-host".into();
        assert_eq!(
            custom.ssh_destination("host.docker.internal").unwrap(),
            "admin@other-host"
        );
        custom.destination = "legacy@other-host".into();
        assert_eq!(
            custom.ssh_destination("host.docker.internal").unwrap(),
            "legacy@other-host"
        );
        custom.username.clear();
        custom.destination = "ssh-alias".into();
        let mut legacy = serde_json::to_value(&custom).unwrap();
        legacy.as_object_mut().unwrap().remove("username");
        let restored: HostConnection = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.ssh_destination("127.0.0.1").unwrap(), "ssh-alias");
        for username in [
            "-bad",
            "user@other-host",
            "user name",
            "user;command",
            "user\n",
        ] {
            custom.username = username.into();
            assert!(custom.validate().is_err());
        }
        assert!(connection.ssh_destination("host;command").is_err());
    }

    #[test]
    fn private_reply_token_is_not_advertised_to_plaintext_clients() {
        let mut current = operation();
        current.input_token = Some("private-prompt".into());
        let mut wire = serde_json::to_value(&current).unwrap();
        assert!(wire.get("input_token").is_none());
        assert_eq!(wire["private_reply_token"], "private-prompt");
        let restored: HostOperation = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(restored.input_token, current.input_token);
        let object = wire.as_object_mut().unwrap();
        let token = object.remove("private_reply_token").unwrap();
        object.insert("input_token".into(), token);
        let legacy: HostOperation = serde_json::from_value(wire).unwrap();
        assert_eq!(legacy.input_token, current.input_token);
    }

    #[test]
    fn default_connection_is_enabled_but_needs_setup_and_recovery_tracks_identity() {
        let connection = HostConnection::default();
        assert!(connection.enabled);
        assert!(!connection.is_configured());
        assert!(connection.validate().is_err());
        let mut operation = operation();
        assert_eq!(
            host_recovery(&operation, "physical-host", "boot", 1, 2),
            HostRecovery::Interrupt
        );
        operation.state = OperationState::AwaitingReconnect;
        operation.plan.expects_reboot = true;
        assert_eq!(
            host_recovery(&operation, "physical-host", "boot", 1, 2),
            HostRecovery::Wait
        );
        assert_eq!(
            host_recovery(&operation, "physical-host", "new-boot", 1, 2),
            HostRecovery::Verify
        );
        assert_eq!(
            host_recovery(&operation, "physical-host", "boot", 1, 3601),
            HostRecovery::Interrupt
        );
        assert_eq!(
            host_recovery(&operation, "replacement", "new-boot", 1, 2),
            HostRecovery::Interrupt
        );
        assert_eq!(
            host_recovery(&operation, "physical-host", "new-boot", 2, 2),
            HostRecovery::Interrupt
        );
    }
    #[test]
    fn commands_and_checks_persist_with_bounded_output_and_fail_closed() {
        futures::executor::block_on(async {
            let adapter = runtime(false, false);
            let mut operation = operation();
            execute_operation(&adapter, &mut operation, false)
                .await
                .unwrap();
            assert_eq!(operation.state, OperationState::Succeeded);
            assert_eq!(*adapter.commands.lock().unwrap(), 3);
            assert!(
                operation
                    .outputs
                    .iter()
                    .all(|output| output.outcome.stdout.len() <= 4096)
            );
            assert_eq!(operation.steps_started, 2);
            let adapter = runtime(true, false);
            let mut failed = operation.clone();
            failed.state = OperationState::Running;
            failed.outputs.clear();
            execute_operation(&adapter, &mut failed, false)
                .await
                .unwrap();
            assert_eq!(failed.state, OperationState::Failed);
            assert_eq!(*adapter.commands.lock().unwrap(), 1);
            let adapter = runtime(false, true);
            let mut failed = operation.clone();
            failed.state = OperationState::Running;
            assert!(
                execute_operation(&adapter, &mut failed, false)
                    .await
                    .is_err()
            );
            assert_eq!(*adapter.commands.lock().unwrap(), 0);
        });
    }
    #[test]
    fn recovery_never_replays_mutations_and_requires_reboot_evidence() {
        futures::executor::block_on(async {
            let adapter = runtime(false, false);
            let mut ordinary = operation();
            execute_operation(&adapter, &mut ordinary, true)
                .await
                .unwrap();
            assert_eq!(ordinary.state, OperationState::Interrupted);
            assert_eq!(*adapter.commands.lock().unwrap(), 0);
            let mut reboot = operation();
            reboot.plan.expects_reboot = true;
            execute_operation(&adapter, &mut reboot, true)
                .await
                .unwrap();
            assert_eq!(reboot.state, OperationState::Interrupted);
            assert_eq!(*adapter.commands.lock().unwrap(), 0);
            let mut reboot = operation();
            reboot.plan.expects_reboot = true;
            reboot.steps_started = 2;
            execute_operation(&adapter, &mut reboot, true)
                .await
                .unwrap();
            assert_eq!(reboot.state, OperationState::Succeeded);
            assert_eq!(*adapter.commands.lock().unwrap(), 1);
            assert!(reboot.outputs.iter().all(|result| result.verification));
        });
    }
    #[test]
    fn invalid_paths_arguments_and_unconfirmed_reboot_are_rejected() {
        let mut cmd = command();
        cmd.cwd = "relative".into();
        assert!(cmd.validate().is_err());
        cmd.cwd = "/tmp".into();
        cmd.args.push("\0".into());
        assert!(cmd.validate().is_err());
        let mut plan = operation().plan;
        plan.steps.clear();
        assert!(plan.validate().is_err());
        for mode in crate::ApprovalMode::CHOICES {
            assert!(!mode.auto_approves("host_apply"));
        }
    }
    #[test]
    fn live_output_keeps_recent_prompts_and_inventory_links_storage_with_a_global_bound() {
        let mut output = format!("{}Password: ", "é".repeat(10000));
        retained_live_output(&mut output);
        assert!(output.len() <= 8192);
        assert!(output.ends_with("Password: "));
        let mount = |path: &str, status: &str| HostResource {
            id: format!("mount:{path}"),
            kind: ResourceKind::Mount,
            name: path.into(),
            status: status.into(),
            detail: String::new(),
        };
        let processes = (0..MAX_RESOURCES)
            .map(|id| HostResource {
                id: format!("process:{id}"),
                kind: ResourceKind::Process,
                name: id.to_string(),
                status: "running".into(),
                detail: String::new(),
            })
            .collect();
        let map = environment_map(
            "host".into(),
            "boot".into(),
            HostInfo::default(),
            vec![
                Ok((processes, vec![])),
                Ok((
                    vec![
                        mount("/media/movies", "runtime source"),
                        mount("/", "mounted"),
                        mount("/media", "mounted"),
                        mount("/media", "runtime source"),
                    ],
                    vec![],
                )),
            ],
        );
        assert_eq!(map.resources.len(), MAX_RESOURCES);
        assert!(map.notes.iter().any(|note| note.contains("limited")));
        assert!(
            map.resources
                .iter()
                .any(|resource| resource.name == "/media" && resource.status == "mounted")
        );
        assert!(map.relationships.iter().any(
            |relation| relation.from == "mount:/media/movies" && relation.to == "mount:/media"
        ));
    }
    #[test]
    fn partial_environment_deduplicates_resources_and_removes_dangling_edges() {
        let resource = HostResource {
            id: "one".into(),
            kind: ResourceKind::Service,
            name: "one".into(),
            status: "active".into(),
            detail: String::new(),
        };
        let map = environment_map(
            "actual-host".into(),
            "boot".into(),
            HostInfo::default(),
            vec![
                Ok((
                    vec![resource.clone(), resource],
                    vec![HostRelationship {
                        from: "one".into(),
                        to: "missing".into(),
                        description: "runs".into(),
                    }],
                )),
                Err("Container runtime unavailable".into()),
            ],
        );
        assert_eq!(map.resources.len(), 1);
        assert!(map.relationships.is_empty());
        assert_eq!(map.notes.len(), 1);
    }
}
