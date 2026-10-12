//! SSH process/OS adapter. Uses the existing SSH agent and pinned known_hosts.
use crate::exec::{SpawnSpec, ToolExecution};
use base64::Engine;
use openwebide_core::{
    CommandOutcome, HostInfo,
    host_admin::{
        HostCommand, HostEnvironment, HostInventoryAdapter, HostQuery, HostRelationship,
        HostResource, HostSample, MAX_RESOURCES, ResourceKind,
    },
};
use std::{sync::Arc, time::Duration};

pub use openwebide_core::host_admin::HostPlatform as Platform;
#[derive(Clone)]
pub struct SshHost {
    pub destination: String,
    pub platform: Platform,
    pub port: u16,
    pub revision: i64,
    known_hosts: Option<Arc<tempfile::NamedTempFile>>,
    pub execution: Arc<dyn ToolExecution>,
}
impl SshHost {
    pub fn new(
        connection: openwebide_core::host_admin::HostConnection,
        execution: Arc<dyn ToolExecution>,
    ) -> Result<Self, String> {
        let default_host =
            std::env::var("OPENWEBIDE_HOST_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        Self::with_default_host(connection, execution, &default_host)
    }
    pub(super) fn with_default_host(
        connection: openwebide_core::host_admin::HostConnection,
        execution: Arc<dyn ToolExecution>,
        default_host: &str,
    ) -> Result<Self, String> {
        connection.validate()?;
        if !connection.enabled {
            return Err("Enable host administration and configure SSH access in Settings.".into());
        }
        let known_hosts = if connection.known_hosts.is_empty() {
            None
        } else {
            use std::io::Write;
            let mut file = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
            file.write_all(connection.known_hosts.as_bytes())
                .map_err(|error| error.to_string())?;
            Some(Arc::new(file))
        };
        Ok(Self {
            destination: connection.ssh_destination(default_host)?,
            platform: connection.platform,
            port: connection.port,
            revision: connection.revision,
            known_hosts,
            execution,
        })
    }
    fn spec(
        &self,
        command: String,
        timeout_seconds: u64,
        interactive: bool,
    ) -> Result<SpawnSpec, String> {
        let mut args = vec![
            if interactive { "-tt" } else { "-T" }.into(),
            "-oBatchMode=yes".into(),
            "-oStrictHostKeyChecking=yes".into(),
            "-oConnectTimeout=5".into(),
            "-oServerAliveInterval=10".into(),
            "-oServerAliveCountMax=3".into(),
            "--".into(),
            self.destination.clone(),
            command,
        ];
        args.splice(0..0, ["-p".into(), self.port.to_string()]);
        if let Some(file) = &self.known_hosts {
            args.splice(
                0..0,
                [
                    format!("-oUserKnownHostsFile={}", file.path().display()),
                    format!(
                        "-oGlobalKnownHostsFile={}",
                        if cfg!(windows) { "NUL" } else { "/dev/null" }
                    ),
                ],
            );
        }
        Ok(SpawnSpec {
            command: "ssh".into(),
            args,
            cwd: std::env::current_dir().map_err(|error| error.to_string())?,
            env: Default::default(),
            timeout: Duration::from_secs(timeout_seconds),
            cancel: Box::pin(std::future::pending()),
        })
    }
    async fn remote(
        &self,
        command: String,
        timeout_seconds: u64,
    ) -> Result<CommandOutcome, String> {
        self.execution
            .run_command(self.spec(command, timeout_seconds, false)?)
            .await
            .map_err(|error| format!("Host SSH execution failed: {error}"))
    }
    pub fn interactive(
        &self,
        command: &HostCommand,
        context: openwebide_core::host_admin::HostCommandContext,
    ) -> Result<openwebide_core::host_admin::HostProcessStream, String> {
        super::interactive::start(
            self.execution.clone(),
            self.spec(
                command_script(self.platform, command)?,
                command.timeout_seconds,
                true,
            )?,
            context,
        )
    }
    pub async fn command(&self, command: &HostCommand) -> Result<CommandOutcome, String> {
        command.validate()?;
        let script = command_script(self.platform, command)?;
        self.remote(script, command.timeout_seconds).await
    }
    async fn probe(&self, script: &str) -> Result<String, String> {
        let remote = if self.platform == Platform::Windows {
            powershell(script, false)
        } else {
            format!("sh -c {}", quote(script))
        };
        let outcome = self.remote(remote, 10).await?;
        if !outcome.is_success() {
            return Err(format!(
                "Host collector unavailable: {}",
                outcome.stderr.chars().take(300).collect::<String>()
            ));
        }
        Ok(outcome.stdout)
    }
}
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn powershell(script: &str, interactive: bool) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    format!(
        "powershell.exe -NoLogo -NoProfile {}-EncodedCommand {}",
        if interactive { "" } else { "-NonInteractive " },
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
pub fn command_script(platform: Platform, command: &HostCommand) -> Result<String, String> {
    if platform == Platform::Windows {
        if command.elevated {
            return Err("Windows SSH commands use the configured account's privileges. Noninteractive sudo is available only on Unix hosts; Windows UAC elevation is not supported.".into());
        }
        let args = command
            .args
            .iter()
            .map(|arg| ps_quote(arg))
            .collect::<Vec<_>>()
            .join(",");
        Ok(powershell(
            &format!(
                "$ErrorActionPreference='Stop'; Set-Location -LiteralPath {}; $LASTEXITCODE=0; & {} @({}); if (-not $?) {{ exit 1 }}; exit $LASTEXITCODE",
                ps_quote(&command.cwd),
                ps_quote(&command.program),
                args
            ),
            command.interactive,
        ))
    } else {
        let args = command
            .args
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        Ok(format!(
            "cd {} && exec {}{} {}",
            quote(&command.cwd),
            if command.elevated {
                if command.interactive {
                    "sudo -- "
                } else {
                    "sudo -n -- "
                }
            } else {
                ""
            },
            quote(&command.program),
            args
        ))
    }
}
fn resource(
    id: String,
    kind: ResourceKind,
    name: String,
    status: String,
    detail: String,
) -> HostResource {
    HostResource {
        id,
        kind,
        name,
        status,
        detail,
    }
}
fn relation(from: String, to: String, description: &str) -> HostRelationship {
    HostRelationship {
        from,
        to,
        description: description.into(),
    }
}

// Select only inventory fields; full inspect output can contain application credentials.
const CONTAINER_FORMAT: &str = r#"{"Id":{{json .Id}},"Name":{{json .Name}},"State":{{json .State.Status}},"Image":{{json .Config.Image}},"Ports":{{json .NetworkSettings.Ports}},"Labels":{{json .Config.Labels}},"Mounts":{{json .Mounts}}}"#;
fn container_script(platform: Platform) -> String {
    if platform == Platform::Windows {
        format!(
            "$ErrorActionPreference='Stop'; if (Get-Command podman -ErrorAction SilentlyContinue) {{$runtime='podman'}} elseif (Get-Command docker -ErrorAction SilentlyContinue) {{$runtime='docker'}} else {{throw 'No container runtime found'}}; $ids=@(& $runtime ps -aq | Select-Object -First 64); if ($LASTEXITCODE -ne 0) {{exit $LASTEXITCODE}}; if ($ids.Count -eq 0) {{'[]'}} else {{& $runtime inspect --format {} @ids; exit $LASTEXITCODE}}",
            ps_quote(CONTAINER_FORMAT)
        )
    } else {
        format!(
            r#"set -eu; runtime=$(command -v podman || command -v docker) || exit 1; all=$("$runtime" ps -aq); ids=$(printf '%s\n' "$all" | head -n 64); if [ -z "$ids" ]; then printf '[]\n'; else "$runtime" inspect --format {} $ids; fi"#,
            quote(CONTAINER_FORMAT)
        )
    }
}

const LINUX_OVERVIEW: &str = r#"set -eu
hostname
awk -F= '/^PRETTY_NAME=/{gsub(/"/,"",$2); print $2}' /etc/os-release
cat /etc/machine-id
cat /proc/sys/kernel/random/boot_id
uname -m
getconf _NPROCESSORS_ONLN
awk '/MemTotal:/{printf "%.0f\n", $2*1024}' /proc/meminfo
awk '/MemAvailable:/{printf "%.0f\n", $2*1024}' /proc/meminfo
printf '%s\n' "$HOME" "${SHELL:-/bin/sh}"
"#;
const MAC_OVERVIEW: &str = r#"set -eu
hostname
printf 'macOS %s\n' "$(sw_vers -productVersion)"
ioreg -rd1 -c IOPlatformExpertDevice | awk -F'"' '/IOPlatformUUID/{print $(NF-1)}'
sysctl -n kern.boottime | awk '{gsub(/,/,"",$4); print $4}'
sysctl -n machdep.cpu.brand_string
sysctl -n hw.ncpu
sysctl -n hw.memsize
printf '0\n%s\n%s\n' "$HOME" "${SHELL:-/bin/sh}"
"#;
const WINDOWS_OVERVIEW: &str = r#"$ErrorActionPreference='Stop'; $os=Get-CimInstance Win32_OperatingSystem; $cpu=Get-CimInstance Win32_Processor | Select-Object -First 1; $machine=(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Cryptography').MachineGuid; [ordered]@{host_name=$env:COMPUTERNAME;os="$($os.Caption) $($os.Version)";machine_id=$machine;boot_id=$os.LastBootUpTime.ToUniversalTime().ToString('o');cpu=$cpu.Name;logical_cores=[int]$env:NUMBER_OF_PROCESSORS;ram_total_bytes=[long]$os.TotalVisibleMemorySize*1024;ram_available_bytes=[long]$os.FreePhysicalMemory*1024;home_directory=$env:USERPROFILE;shell="PowerShell $($PSVersionTable.PSVersion)"} | ConvertTo-Json -Compress"#;
#[derive(serde::Deserialize)]
struct Overview {
    host_name: String,
    os: String,
    machine_id: String,
    boot_id: String,
    cpu: String,
    logical_cores: usize,
    ram_total_bytes: u64,
    ram_available_bytes: u64,
    home_directory: String,
    #[serde(default)]
    shell: String,
}
fn parse_overview(raw: &str, platform: Platform) -> Result<Overview, String> {
    if platform == Platform::Windows {
        return serde_json::from_str(raw).map_err(|error| error.to_string());
    }
    let lines = raw.lines().map(str::trim).collect::<Vec<_>>();
    if !(9..=10).contains(&lines.len()) || lines[2].is_empty() || lines[3].is_empty() {
        return Err(
            "SSH host identity unavailable; check the configured OS and host utilities.".into(),
        );
    }
    let bytes = |value: &str| {
        value
            .parse::<u64>()
            .map_err(|_| "Invalid host memory reading")
    };
    Ok(Overview {
        host_name: lines[0].into(),
        os: lines[1].into(),
        machine_id: lines[2].into(),
        boot_id: lines[3].into(),
        cpu: lines[4].into(),
        logical_cores: lines[5].parse().map_err(|_| "Invalid host CPU reading")?,
        ram_total_bytes: bytes(lines[6])?,
        ram_available_bytes: bytes(lines[7])?,
        home_directory: lines[8].into(),
        shell: lines.get(9).copied().unwrap_or("POSIX shell").into(),
    })
}
impl HostInventoryAdapter for SshHost {
    async fn overview(&self) -> Result<HostEnvironment, String> {
        let script = match self.platform {
            Platform::Linux => LINUX_OVERVIEW,
            Platform::Macos => MAC_OVERVIEW,
            Platform::Windows => WINDOWS_OVERVIEW,
        };
        let overview = parse_overview(&self.probe(script).await?, self.platform)?;
        Ok(HostEnvironment {
            target: format!("{}@{}", overview.host_name, overview.machine_id),
            boot_id: overview.boot_id,
            info: HostInfo {
                host_name: Some(overview.host_name),
                os: Some(overview.os),
                scope: format!(
                    "Administration target via SSH alias {}. Commands run as its configured SSH account. Home: {}. Login shell: {}. Commands invoke POSIX shell syntax on Unix and PowerShell on Windows.",
                    self.destination, overview.home_directory, overview.shell
                ),
                cpu: Some(overview.cpu),
                logical_cores: overview.logical_cores,
                ram_total_bytes: overview.ram_total_bytes,
                ram_available_bytes: overview.ram_available_bytes,
                ..Default::default()
            },
            resources: vec![],
            relationships: vec![],
            notes: if self.platform == Platform::Macos {
                vec!["Available RAM is not reported by the macOS collector.".into(), "Container resources come from the SSH account's selected runtime, which can be a separate VM.".into()]
            } else if self.platform == Platform::Windows {
                vec!["Container resources come from the SSH account's selected runtime, which can be a separate VM.".into()]
            } else {
                vec![]
            },
        })
    }
    async fn query(&self, query: HostQuery) -> Result<HostSample, String> {
        if self.platform == Platform::Windows {
            return self.windows_query(query).await;
        }
        match query {
            HostQuery::Processes => {
                let raw = self.probe("ps -eo pid=,comm=,rss=").await?;
                Ok((
                    raw.lines()
                        .take(MAX_RESOURCES)
                        .filter_map(|line| {
                            let mut fields = line.split_whitespace();
                            let pid = fields.next()?;
                            let name = fields.next()?;
                            Some(resource(
                                format!("process:{pid}"),
                                ResourceKind::Process,
                                name.into(),
                                "running".into(),
                                line.into(),
                            ))
                        })
                        .collect(),
                    vec![],
                ))
            }
            HostQuery::Services => self.services().await,
            HostQuery::Containers => {
                let raw = self.probe(&container_script(self.platform)).await?;
                parse_containers(&raw)
            }
            HostQuery::Ports => {
                let script = if self.platform == Platform::Linux {
                    "ss -H -lntup"
                } else {
                    "lsof -nP -iTCP -sTCP:LISTEN"
                };
                let raw = self.probe(script).await?;
                Ok(parse_ports(&raw, self.platform))
            }
            HostQuery::Storage => {
                if self.platform == Platform::Linux {
                    parse_mounts(
                        &self
                            .probe("findmnt --json --list --output TARGET,SOURCE,FSTYPE")
                            .await?,
                    )
                } else {
                    let raw = self.probe("df -kP").await?;
                    Ok((
                        raw.lines()
                            .skip(1)
                            .take(64)
                            .filter_map(|line| {
                                let fields = line.split_whitespace().collect::<Vec<_>>();
                                if fields.len() < 6 {
                                    return None;
                                }
                                let mount = fields[5..].join(" ");
                                Some(resource(
                                    format!("mount:{mount}"),
                                    ResourceKind::Mount,
                                    mount,
                                    "mounted".into(),
                                    line.into(),
                                ))
                            })
                            .collect(),
                        vec![],
                    ))
                }
            }
            HostQuery::Configuration => {
                if self.platform != Platform::Linux {
                    return Err("Quadlet configuration inventory is available on Linux; other host configuration can be inspected through approved host commands.".into());
                }
                let raw=self.probe(r#"for f in /etc/containers/systemd/*.container "${XDG_CONFIG_HOME:-$HOME/.config}"/containers/systemd/*.container; do [ -f "$f" ] || continue; printf '%s\n' "$f"; done"#).await?;
                let mut resources = vec![];
                let mut relationships = vec![];
                for path in raw.lines().take(MAX_RESOURCES) {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    let unit = format!("{}.service", name.trim_end_matches(".container"));
                    let scope = if path.starts_with("/etc/") {
                        "system"
                    } else {
                        "user"
                    };
                    resources.push(resource(
                        format!("config:{path}"),
                        ResourceKind::Configuration,
                        name.into(),
                        "Quadlet".into(),
                        path.into(),
                    ));
                    relationships.push(relation(
                        format!("config:{path}"),
                        format!("service:{scope}:{unit}"),
                        "defines",
                    ));
                }
                Ok((resources, relationships))
            }
        }
    }
}
impl SshHost {
    async fn services(&self) -> Result<HostSample, String> {
        if self.platform == Platform::Macos {
            let raw = self.probe("launchctl list").await?;
            return Ok((
                raw.lines()
                    .skip(1)
                    .take(MAX_RESOURCES)
                    .filter_map(|line| {
                        let fields = line.split_whitespace().collect::<Vec<_>>();
                        let name = *fields.get(2)?;
                        Some(resource(
                            format!("service:{name}"),
                            ResourceKind::Service,
                            name.into(),
                            if fields[0] == "-" { "idle" } else { "running" }.into(),
                            line.into(),
                        ))
                    })
                    .collect(),
                vec![],
            ));
        }
        let mut resources = vec![];
        for scope in ["system", "user"] {
            let script = format!(
                "systemctl {}list-units --type=service --all --no-legend --plain --no-pager",
                if scope == "user" { "--user " } else { "" }
            );
            match self.probe(&script).await {
                Ok(raw) => {
                    for line in raw.lines().take(MAX_RESOURCES / 2) {
                        let fields = line.split_whitespace().collect::<Vec<_>>();
                        if fields.len() < 4 {
                            continue;
                        }
                        resources.push(resource(
                            format!("service:{scope}:{}", fields[0]),
                            ResourceKind::Service,
                            fields[0].into(),
                            fields[2].into(),
                            format!("{scope}: {}", fields[3..].join(" ")),
                        ));
                    }
                }
                Err(error) => resources.push(resource(
                    format!("service:{scope}:unavailable"),
                    ResourceKind::Service,
                    format!("{scope} services"),
                    "unavailable".into(),
                    error,
                )),
            }
        }
        Ok((resources, vec![]))
    }
    async fn windows_query(&self, query: HostQuery) -> Result<HostSample, String> {
        let script = match query {
            HostQuery::Processes => {
                r#"Get-Process | Select-Object -First 256 | ForEach-Object { @{id="process:$($_.Id)";kind='process';name=$_.ProcessName;status='running';detail="PID $($_.Id); memory $($_.WorkingSet64) bytes"} }"#
            }
            HostQuery::Services => {
                r#"Get-Service | Select-Object -First 256 | ForEach-Object { @{id="service:$($_.Name)";kind='service';name=$_.DisplayName;status=[string]$_.Status;detail=$_.Name} }"#
            }
            HostQuery::Ports => {
                r#"Get-NetTCPConnection -State Listen | Select-Object -First 256 | ForEach-Object { @{id="port:tcp:$($_.LocalAddress):$($_.LocalPort)";kind='port';name="$($_.LocalAddress):$($_.LocalPort)";status='listening';detail="PID $($_.OwningProcess)"} }"#
            }
            HostQuery::Storage => {
                r#"Get-CimInstance Win32_LogicalDisk | ForEach-Object { @{id="disk:$($_.DeviceID)";kind='disk';name=$_.DeviceID;status='mounted';detail="$($_.FreeSpace) available / $($_.Size) bytes"} }"#
            }
            HostQuery::Containers => {
                return parse_containers(&self.probe(&container_script(self.platform)).await?);
            }
            HostQuery::Configuration => {
                return Err("Inspect Windows configuration through approved host commands.".into());
            }
        };
        let raw=self.probe(&format!("$ErrorActionPreference='Stop'; $items=@({script}); ConvertTo-Json -InputObject $items -Compress -Depth 5")).await?;
        let resources: Vec<HostResource> =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let relationships = if query == HostQuery::Ports {
            resources
                .iter()
                .filter_map(|resource| {
                    resource.detail.strip_prefix("PID ").map(|pid| {
                        relation(resource.id.clone(), format!("process:{pid}"), "owned by")
                    })
                })
                .collect()
        } else {
            vec![]
        };
        Ok((resources, relationships))
    }
}
fn parse_ports(raw: &str, platform: Platform) -> HostSample {
    let mut resources = vec![];
    let mut relationships = vec![];
    for (index, line) in raw.lines().take(MAX_RESOURCES).enumerate() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let name = fields
            .get(if platform == Platform::Linux { 4 } else { 8 })
            .copied()
            .unwrap_or("Listener");
        let id = format!("listener:{index}");
        let pid = if platform == Platform::Linux {
            line.split("pid=")
                .nth(1)
                .map(|value| value.split(',').next().unwrap_or(""))
        } else {
            fields.get(1).copied()
        };
        if let Some(pid) = pid {
            relationships.push(relation(id.clone(), format!("process:{pid}"), "owned by"));
        }
        resources.push(resource(
            id,
            ResourceKind::Port,
            name.into(),
            "listening".into(),
            line.into(),
        ));
    }
    (resources, relationships)
}
fn parse_containers(raw: &str) -> Result<HostSample, String> {
    let containers: Vec<serde_json::Value> = if raw.trim_start().starts_with('[') {
        serde_json::from_str(raw).map_err(|error| format!("Container inventory: {error}"))?
    } else {
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Container inventory: {error}"))?
    };
    let mut resources = vec![];
    let mut relationships = vec![];
    for container in containers.into_iter().take(64) {
        let Some(id) = container.get("Id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let name = container
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                container
                    .get("Names")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|names| names.first())
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or(id)
            .trim_start_matches('/');
        resources.push(resource(
            format!("container:{id}"),
            ResourceKind::Container,
            name.into(),
            container
                .get("State")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .into(),
            container
                .get("Image")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .into(),
        ));
        if let Some(ports) = container.get("Ports").and_then(serde_json::Value::as_array) {
            for port in ports.iter().take(16) {
                if let Some(number) = port.get("host_port").and_then(serde_json::Value::as_u64) {
                    let protocol = port
                        .get("protocol")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tcp");
                    let port_id = format!("port:{protocol}:{number}");
                    resources.push(resource(
                        port_id.clone(),
                        ResourceKind::Port,
                        format!("{protocol}:{number}"),
                        "published".into(),
                        name.into(),
                    ));
                    relationships.push(relation(format!("container:{id}"), port_id, "publishes"));
                }
            }
        }
        if let Some(ports) = container
            .get("Ports")
            .and_then(serde_json::Value::as_object)
        {
            for (container_port, bindings) in ports.iter().take(16) {
                let protocol = container_port
                    .split_once('/')
                    .map_or("tcp", |(_, protocol)| protocol);
                for binding in bindings.as_array().into_iter().flatten().take(4) {
                    if let Some(number) =
                        binding.get("HostPort").and_then(serde_json::Value::as_str)
                    {
                        let port_id = format!("port:{protocol}:{number}");
                        resources.push(resource(
                            port_id.clone(),
                            ResourceKind::Port,
                            format!("{protocol}:{number}"),
                            "published".into(),
                            name.into(),
                        ));
                        relationships.push(relation(
                            format!("container:{id}"),
                            port_id,
                            "publishes",
                        ));
                    }
                }
            }
        }
        if let Some(mounts) = container
            .get("Mounts")
            .and_then(serde_json::Value::as_array)
        {
            for mount in mounts.iter().take(16) {
                if let Some(source) = mount
                    .get("Source")
                    .and_then(serde_json::Value::as_str)
                    .filter(|source| !source.is_empty())
                {
                    let mount_id = format!("mount:{source}");
                    let destination = mount
                        .get("Destination")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    resources.push(resource(
                        mount_id.clone(),
                        ResourceKind::Mount,
                        source.into(),
                        "runtime source".into(),
                        format!("Runtime-reported source for {name}:{destination}"),
                    ));
                    relationships.push(relation(format!("container:{id}"), mount_id, "mounts"));
                }
            }
        }
        if let Some(unit) = container
            .get("Labels")
            .and_then(|labels| labels.get("PODMAN_SYSTEMD_UNIT"))
            .and_then(serde_json::Value::as_str)
        {
            for scope in ["user", "system"] {
                relationships.push(relation(
                    format!("service:{scope}:{unit}"),
                    format!("container:{id}"),
                    "runs",
                ));
            }
        }
    }
    Ok((resources, relationships))
}
fn parse_mounts(raw: &str) -> Result<HostSample, String> {
    let json: serde_json::Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let mut resources = vec![];
    let mut relationships = vec![];
    for mount in json
        .get("filesystems")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_RESOURCES / 2)
    {
        let Some(target) = mount.get("target").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let source = mount
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        resources.push(resource(
            format!("mount:{target}"),
            ResourceKind::Mount,
            target.into(),
            "mounted".into(),
            source.into(),
        ));
        resources.push(resource(
            format!("disk:{source}"),
            ResourceKind::Disk,
            source.into(),
            "mounted".into(),
            mount
                .get("fstype")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .into(),
        ));
        relationships.push(relation(
            format!("mount:{target}"),
            format!("disk:{source}"),
            "stored on",
        ));
    }
    Ok((resources, relationships))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unix_commands_quote_every_argument_and_elevate_only_the_requested_process() {
        let command = HostCommand {
            program: "printf".into(),
            args: vec![
                "%s".into(),
                "'\"; $(touch /tmp/should-not-exist) `uname`".into(),
            ],
            cwd: "/tmp/a b".into(),
            elevated: true,
            interactive: true,
            timeout_seconds: 30,
        };
        let script = command_script(Platform::Linux, &command).unwrap();
        assert!(script.starts_with("cd '/tmp/a b' && exec sudo -- 'printf'"));
        assert!(script.contains("'\\''"));
        let mut command = command;
        command.interactive = false;
        assert!(
            command_script(Platform::Macos, &command)
                .unwrap()
                .contains("sudo -n --")
        );
    }
    #[test]
    fn windows_command_arguments_use_encoded_powershell_and_reject_sudo() {
        let mut command = HostCommand {
            program: "Write-Output".into(),
            args: vec!["x'; Remove-Item /; '".into()],
            cwd: "C:\\Users\\Example".into(),
            elevated: false,
            interactive: true,
            timeout_seconds: 30,
        };
        let encoded = command_script(Platform::Windows, &command).unwrap();
        assert!(!encoded.contains("-NonInteractive"));
        command.interactive = false;
        assert!(
            command_script(Platform::Windows, &command)
                .unwrap()
                .contains("-NonInteractive")
        );
        let payload = encoded.split_whitespace().last().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let utf16 = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let script = String::from_utf16(&utf16).unwrap();
        assert!(script.contains("'x''; Remove-Item /; '''"));
        assert!(script.contains("Set-Location -LiteralPath"));
        command.elevated = true;
        assert!(command_script(Platform::Windows, &command).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn container_collector_runs_both_runtimes_without_querying_environment_secrets() {
        use std::os::unix::fs::PermissionsExt;
        for runtime in ["podman", "docker"] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join(runtime);
            std::fs::write(&file, r#"#!/bin/sh
if [ "$1" = ps ]; then printf 'plex\n'; exit 0; fi
[ "$1" = inspect ] && [ "$2" = --format ] && [ "$4" = plex ] || exit 10
case "$3" in *Env*) exit 11 ;; esac
printf '%s\n' '{"Id":"plex","Name":"/Plex","State":"running","Image":"plex:latest","Ports":{"32400/tcp":[{"HostPort":"32400"}]},"Labels":{"PODMAN_SYSTEMD_UNIT":"plex.service"},"Mounts":[{"Source":"/media/movies","Destination":"/movies"}]}'
"#).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
            // Keep installed host runtimes out of discovery: the Docker case
            // must not select a real Podman ahead of its fixture.
            std::os::unix::fs::symlink("/usr/bin/head", dir.path().join("head")).unwrap();
            let result = std::process::Command::new("/bin/sh")
                .args(["-c", &container_script(Platform::Linux)])
                .env("PATH", dir.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let (resources, relationships) =
                parse_containers(&String::from_utf8(result.stdout).unwrap()).unwrap();
            assert_eq!(resources.len(), 3);
            assert!(resources.iter().any(|resource| resource.name == "Plex"));
            assert!(
                relationships
                    .iter()
                    .any(|link| link.to == "mount:/media/movies")
            );
            assert!(relationships.iter().any(|link| link.to == "port:tcp:32400"));
        }
    }
    #[test]
    fn inventories_decode_linux_macos_windows_and_resource_relationships() {
        for platform in [Platform::Linux, Platform::Macos] {
            let overview=parse_overview("physical-host\nOS version\nmachine\nboot\nCPU\n8\n16000000000\n8000000000\n/home/user\n",platform).unwrap();
            assert_eq!(overview.logical_cores, 8);
            assert_eq!(overview.machine_id, "machine");
        }
        let overview=parse_overview(r#"{"host_name":"Windows PC","os":"Windows 11","machine_id":"guid","boot_id":"boot","cpu":"CPU","logical_cores":8,"ram_total_bytes":16000000000,"ram_available_bytes":8000000000,"home_directory":"C:\\Users\\User"}"#,Platform::Windows).unwrap();
        assert_eq!(overview.os, "Windows 11");
        let (resources,links)=parse_containers(r#"[{"Id":"plex","Names":["Plex"],"State":"running","Image":"plex:latest","Ports":[{"host_port":32400,"protocol":"tcp"}],"Labels":{"PODMAN_SYSTEMD_UNIT":"plex.service"}}]"#).unwrap();
        assert_eq!(resources.len(), 2);
        assert!(
            links
                .iter()
                .any(|link| link.from == "container:plex" && link.to == "port:tcp:32400")
        );
        let (resources, links) = parse_mounts(
            r#"{"filesystems":[{"target":"/media","source":"/dev/sdb1","fstype":"ext4"}]}"#,
        )
        .unwrap();
        assert_eq!(resources.len(), 2);
        assert_eq!(links[0].to, "disk:/dev/sdb1");
    }
}
