//! Detached long-running background experiments that outlive Host restarts (`experiments.*`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Lifecycle state of a detached background experiment.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentStatus {
    Running,
    Exited,
    Stopped,
    #[default]
    Unknown,
}

/// One persisted detached experiment entry (`experiments.list`, `experiments.start`, `experiments.stop`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRecord {
    pub id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub log_path: String,
    pub status: ExperimentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// Linux `/proc/<pid>/stat` field 22 (`starttime` in clock ticks), guarding against PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_start_ticks: Option<u64>,
    /// Linux process owner UID recorded at launch, preventing signals to another user's process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda_visible_devices: Option<String>,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// Request payload for `experiments.start`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cwd: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda_visible_devices: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

/// Incremental log tail chunk returned by `experiments.tail`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentTailResult {
    pub id: String,
    pub log_path: String,
    pub text: String,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub reset: bool,
}

/// Response of `experiments.list`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentListResponse {
    #[serde(default)]
    pub experiments: Vec<ExperimentRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_experiment_records_and_tail() {
        let params: ExperimentStartParams = serde_json::from_value(json!({
            "name": "train-l20",
            "cwd": "/root/kube-user/proj",
            "command": "python train.py --epochs 10",
            "cudaVisibleDevices": "0,1",
            "env": { "HF_ENDPOINT": "https://hf-mirror.com" }
        }))
        .unwrap();
        assert_eq!(params.cuda_visible_devices.as_deref(), Some("0,1"));
        assert_eq!(params.env.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));

        let record = ExperimentRecord {
            id: "exp-1".into(),
            name: "train-l20".into(),
            command: params.command,
            cwd: params.cwd,
            log_path: "/root/kube-user/proj/train.log".into(),
            status: ExperimentStatus::Running,
            pid: Some(4321),
            pgid: Some(4321),
            proc_start_ticks: Some(987654),
            uid: Some(0),
            cuda_visible_devices: params.cuda_visible_devices,
            started_at: 1_730_000_000_000,
            ended_at: None,
            exit_code: None,
        };
        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded["status"], json!("running"));
        assert_eq!(encoded["procStartTicks"], json!(987654));
        assert_eq!(serde_json::from_value::<ExperimentRecord>(encoded).unwrap(), record);

        let tail: ExperimentTailResult = serde_json::from_value(json!({
            "id": "exp-1",
            "logPath": "/root/kube-user/proj/train.log",
            "text": "epoch 1 loss 0.42\n",
            "offset": 1024,
            "nextOffset": 1042,
            "totalBytes": 1042,
            "truncated": false,
            "reset": false
        }))
        .unwrap();
        assert_eq!(tail.next_offset, 1042);
    }
}
