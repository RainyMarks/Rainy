//! Host hardware, cgroup, disk and listening-port metrics (`host.metrics`, `host.ports`).

use serde::{Deserialize, Serialize};

/// One compute process currently occupying memory on a GPU.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GpuProcess {
    pub pid: u32,
    pub process_name: String,
    pub used_memory_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
}

/// Instantaneous status of one NVIDIA GPU reported by `nvidia-smi`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization_percent: Option<u32>,
    pub memory_used_mib: u64,
    pub memory_total_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<GpuProcess>,
}

/// Container cgroup v1/v2 CPU and memory limits, when enforced on Linux.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CgroupLimits {
    /// `1` for cgroup v1, `2` for cgroup v2.
    pub version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_quota_cores: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_current_bytes: Option<u64>,
}

/// Available and total filesystem space for one watched path or mount point.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// Response of `host.metrics`: lightweight CPU, memory, cgroup, GPU and disk snapshot.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostMetrics {
    pub collected_at: u64,
    pub hostname: String,
    pub platform: String,
    pub cpu_count: u32,
    /// Effective CPU cores after applying cgroup CPU quota (falls back to `cpu_count`).
    pub effective_cpu_cores: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_1m: Option<f64>,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub host_memory_total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<CgroupLimits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<DiskUsage>,
}

/// One TCP port currently listening on the Host (`host.ports`), used to offer one-click SSH `-L` forwarding.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    pub port: u16,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Response of `host.ports`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostPortsResponse {
    #[serde(default)]
    pub ports: Vec<ListeningPort>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_host_metrics_and_ports() {
        let metrics = HostMetrics {
            collected_at: 1_730_000_000_000,
            hostname: "leaper".into(),
            platform: "linux".into(),
            cpu_count: 128,
            effective_cpu_cores: 36.0,
            cpu_percent: Some(42.5),
            load_avg_1m: Some(12.3),
            memory_used_bytes: 48 * 1024 * 1024 * 1024,
            memory_total_bytes: 192 * 1024 * 1024 * 1024,
            host_memory_total_bytes: 512 * 1024 * 1024 * 1024,
            cgroup: Some(CgroupLimits {
                version: 2,
                cpu_quota_cores: Some(36.0),
                memory_limit_bytes: Some(192 * 1024 * 1024 * 1024),
                memory_current_bytes: Some(48 * 1024 * 1024 * 1024),
            }),
            gpus: vec![GpuInfo {
                index: 0,
                name: "NVIDIA L20".into(),
                utilization_percent: Some(15),
                memory_used_mib: 1024,
                memory_total_mib: 46068,
                temperature_c: Some(41),
                processes: vec![GpuProcess {
                    pid: 1234,
                    process_name: "python".into(),
                    used_memory_mib: 1024,
                    user: Some("root".into()),
                    uid: Some(0),
                }],
            }],
            disks: vec![DiskUsage {
                path: "/root/kube-user".into(),
                mount: Some("/root/kube-user".into()),
                available_bytes: 100 * 1024 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
            }],
        };
        let encoded = serde_json::to_value(&metrics).unwrap();
        assert_eq!(encoded["effectiveCpuCores"], json!(36.0));
        assert_eq!(encoded["gpus"][0]["memoryTotalMib"], json!(46068));
        assert_eq!(serde_json::from_value::<HostMetrics>(encoded).unwrap(), metrics);

        let ports: HostPortsResponse = serde_json::from_value(json!({
            "ports": [{ "port": 6006, "address": "127.0.0.1", "pid": 42, "processName": "tensorboard", "label": "TensorBoard" }]
        }))
        .unwrap();
        assert_eq!(ports.ports.len(), 1);
        assert_eq!(ports.ports[0].port, 6006);
    }
}
