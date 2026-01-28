use serde::Serialize;
use tokio::io::AsyncWriteExt;

use utils::{Kind, Message};

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEvent {
    pub ts_ms: u64,
    pub dir: String,
    pub room: String,
    pub relay: String,
    pub local_device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_device_name: Option<String>,
    pub remote_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_device_name: Option<String>,
    pub kind: String,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub bytes: usize,
    pub sha256: Option<String>,
}

fn history_path() -> std::path::PathBuf {
    crate::paths::history_path()
}

async fn append_history(event: HistoryEvent) {
    // Best-effort; never fail the main flow.
    let p = history_path();
    if let Some(parent) = p.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut f = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .await
    {
        Ok(v) => v,
        Err(_) => return,
    };

    let _ = f.write_all(line.as_bytes()).await;
    let _ = f.write_all(b"\n").await;
}

fn kind_to_string(k: &Kind) -> String {
    match k {
        Kind::Text => "text",
        Kind::Image => "image",
        Kind::File => "file",
        Kind::Join => "join",
    }
    .to_string()
}

pub async fn record_send(
    local_device_id: &str,
    local_device_name: Option<String>,
    room: &str,
    relay: &str,
    kind: Kind,
    mime: Option<String>,
    name: Option<String>,
    bytes: usize,
    sha256: Option<String>,
) {
    log::debug!(
        "send: room={} relay={} kind={:?} mime={:?} name={:?} bytes={} sha={:?}",
        room,
        relay,
        kind,
        mime,
        name,
        bytes,
        sha256
    );
    append_history(HistoryEvent {
        ts_ms: utils::now_ms(),
        dir: "send".to_string(),
        room: room.to_string(),
        relay: relay.to_string(),
        local_device_id: local_device_id.to_string(),
        local_device_name,
        remote_device_id: None,
        remote_device_name: None,
        kind: kind_to_string(&kind),
        mime,
        name,
        bytes,
        sha256,
    })
    .await;
}

pub async fn record_recv(
    local_device_id: &str,
    local_device_name: Option<String>,
    room: &str,
    relay: &str,
    msg: &Message,
) {
    log::debug!(
        "recv: room={} relay={} from={} kind={:?} mime={:?} name={:?} bytes={} sha={:?}",
        room,
        relay,
        msg.device_id,
        msg.kind,
        msg.mime,
        msg.name,
        msg.payload.as_ref().map(|p| p.len()).unwrap_or(0),
        msg.sha256
    );
    append_history(HistoryEvent {
        ts_ms: utils::now_ms(),
        dir: "recv".to_string(),
        room: room.to_string(),
        relay: relay.to_string(),
        local_device_id: local_device_id.to_string(),
        local_device_name,
        remote_device_id: Some(msg.device_id.clone()),
        remote_device_name: msg.sender_name.clone(),
        kind: kind_to_string(&msg.kind),
        mime: msg.mime.clone(),
        name: msg.name.clone(),
        bytes: msg.payload.as_ref().map(|p| p.len()).unwrap_or(0),
        sha256: msg.sha256.clone(),
    })
    .await;
}
