//! Native hardware primitives. No model-supplied paths or commands are accepted.
use crate::BridgeError;
use openwebide_core::{HostDisk, HostGpu, HostInfo, HostTemperature};
use std::time::Duration;
use sysinfo::{Components, Disks, System};

fn basic_snapshot() -> HostInfo {
    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let mut info = HostInfo {
        host_name: System::host_name(),
        os: System::long_os_version(),
        scope: "Execution bridge host; readings reflect what the bridge process can see. Container limits and mounted disks may differ from the physical machine. This is not necessarily the model server.".into(),
        cpu: system.cpus().first().map(|cpu| cpu.brand().to_owned()),
        logical_cores: system.cpus().len(),
        ram_total_bytes: system.total_memory(),
        ram_available_bytes: system.available_memory(),
        disks: Disks::new_with_refreshed_list().iter().take(32).map(|disk| HostDisk {
            name: disk.name().to_string_lossy().into_owned(),
            total_bytes:disk.total_space(), available_bytes:disk.available_space(),
        }).collect(),
        temperatures: Components::new_with_refreshed_list().iter().take(64).filter_map(|sensor| {
            sensor.temperature().filter(|value| value.is_finite()).map(|celsius| HostTemperature { name:sensor.label().to_owned(), celsius })
        }).collect(),
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    linux_sensors(&mut info);
    if info.temperatures.is_empty() {
        info.notes.push(
            "Temperature sensors unavailable on this host or not exposed to the bridge.".into(),
        );
    }
    info
}

pub async fn snapshot() -> Result<HostInfo, BridgeError> {
    let mut info = tokio::task::spawn_blocking(basic_snapshot)
        .await
        .map_err(|error| BridgeError::Execution(error.to_string()))?;
    #[cfg(target_os = "macos")]
    if let Some(output) = probe("system_profiler", &["SPDisplaysDataType", "-json"]).await {
        info.gpus.extend(parse_apple(&output));
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(output) = probe(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,memory.used,temperature.gpu,fan.speed",
            "--format=csv,noheader,nounits",
        ],
    )
    .await
    {
        info.gpus.extend(parse_nvidia(&output));
    }
    if info.gpus.is_empty() {
        info.notes.push("GPU inventory and VRAM unavailable; driver or platform telemetry is not exposed to the bridge.".into());
    }
    if info.gpus.iter().any(|gpu| gpu.vram_total_bytes.is_none()) {
        info.notes.push("Some GPUs do not expose dedicated VRAM measurements; Apple silicon uses unified system memory.".into());
    }
    if info.fans.is_empty() && info.gpus.iter().all(|gpu| gpu.fan_percent.is_none()) {
        info.notes
            .push("Fan readings unavailable on this host or not exposed to the bridge.".into());
    }
    Ok(info)
}

async fn probe(program: &str, args: &[&str]) -> Option<String> {
    let spec = super::SpawnSpec {
        command: program.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: std::env::current_dir().ok()?,
        env: Default::default(),
        timeout: Duration::from_secs(3),
        cancel: Box::pin(std::future::pending()),
        folder: None,
    };
    let output = super::host::execute_command_direct(spec).await.ok()?;
    (output.exit_code == Some(0)).then_some(output.stdout)
}

#[cfg(any(target_os = "macos", test))]
fn parse_apple(raw: &str) -> Vec<HostGpu> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    json.get("SPDisplaysDataType")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|gpu| {
            Some(HostGpu {
                name: gpu
                    .get("sppci_model")
                    .or_else(|| gpu.get("_name"))?
                    .as_str()?
                    .to_owned(),
                vram_total_bytes: None,
                vram_used_bytes: None,
                temperature_celsius: None,
                fan_percent: None,
            })
        })
        .collect()
}

#[cfg(any(not(target_os = "macos"), test))]
fn parse_nvidia(csv: &str) -> Vec<HostGpu> {
    csv.lines()
        .take(16)
        .filter_map(|line| {
            let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
            if fields.len() != 5 {
                return None;
            }
            let bytes = |value: &str| {
                value
                    .parse::<u64>()
                    .ok()
                    .and_then(|mib| mib.checked_mul(1024 * 1024))
            };
            let reading = |value: &str| value.parse::<f32>().ok().filter(|value| value.is_finite());
            Some(HostGpu {
                name: fields[0].into(),
                vram_total_bytes: bytes(fields[1]),
                vram_used_bytes: bytes(fields[2]),
                temperature_celsius: reading(fields[3]),
                fan_percent: reading(fields[4]),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn linux_sensors(info: &mut HostInfo) {
    // Read only fixed kernel telemetry files; never accept a model-provided path.
    if let Ok(chips) = std::fs::read_dir("/sys/class/hwmon") {
        for chip in chips.take(32).flatten() {
            let path = chip.path();
            let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
            for index in 1..=16 {
                if let Ok(rpm) = std::fs::read_to_string(path.join(format!("fan{index}_input")))
                    && let Ok(rpm) = rpm.trim().parse()
                {
                    info.fans.push(openwebide_core::HostFan {
                        name: format!("{} fan {index}", name.trim()),
                        rpm,
                    });
                }
            }
        }
    }
    if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
        for card in cards.take(64).flatten() {
            let name = card.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let device = card.path().join("device");
            if std::fs::read_to_string(device.join("vendor"))
                .ok()
                .as_deref()
                .map(str::trim)
                != Some("0x1002")
            {
                continue;
            }
            let bytes = |file| {
                std::fs::read_to_string(device.join(file))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            info.gpus.push(HostGpu {
                name: format!("AMD GPU ({name})"),
                vram_total_bytes: bytes("mem_info_vram_total"),
                vram_used_bytes: bytes("mem_info_vram_used"),
                temperature_celsius: None,
                fan_percent: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apple_gpu_inventory_does_not_invent_dedicated_vram() {
        let gpus = parse_apple(r#"{"SPDisplaysDataType":[{"sppci_model":"Apple M3"}]}"#);
        assert_eq!(gpus[0].name, "Apple M3");
        assert_eq!(gpus[0].vram_total_bytes, None);
        assert!(parse_apple("broken").is_empty());
    }
    #[test]
    fn gpu_readings_preserve_missing_values_and_units() {
        let gpus = parse_nvidia(
            "RTX 4090, 24564, 1024, 42, 30\nA100, 40960, N/A, 51, [Not Supported]\ninvalid",
        );
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].vram_used_bytes, Some(1024 * 1024 * 1024));
        assert_eq!(gpus[0].fan_percent, Some(30.0));
        assert_eq!(gpus[1].vram_used_bytes, None);
        assert_eq!(gpus[1].fan_percent, None);
    }
}
