use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MSG_V2_MAGIC: &[u8; 4] = b"MCR2";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Kind {
    Text,
    Image,
    File,
    Join,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub event_id: String,
    pub device_id: String,
    /// Optional device display name (human-friendly), set by the sender.
    /// This is distinct from `name` (which is used for file/display name of the *payload*).
    pub sender_name: Option<String>,
    pub ts: u64,
    pub kind: Kind,
    pub room: String,
    pub mime: Option<String>,
    /// Optional display name (e.g. for file transfers).
    pub name: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub size: usize,
    pub sha256: Option<String>,
}

/// Older wire-compatible message (v0).
///
/// We keep it only for backward-compatible decoding.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct MessageV0 {
    pub event_id: String,
    pub device_id: String,
    pub ts: u64,
    pub kind: Kind,
    pub room: String,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub payload: Option<Vec<u8>>,
}

/// Wire-compatible old message (v1) without `sender_name`.
///
/// We keep it only for backward-compatible decoding.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct MessageV1 {
    pub event_id: String,
    pub device_id: String,
    pub ts: u64,
    pub kind: Kind,
    pub room: String,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub size: usize,
    pub sha256: Option<String>,
}

impl Message {
    pub fn new_join(device_id: &str, room: &str) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            device_id: device_id.to_string(),
            sender_name: None,
            ts: crate::now_ms(),
            kind: Kind::Join,
            room: room.to_string(),
            mime: None,
            name: None,
            payload: None,
            size: 0,
            sha256: None,
        }
    }

    pub fn new_text(device_id: &str, room: &str, text: &str) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            device_id: device_id.to_string(),
            sender_name: None,
            ts: crate::now_ms(),
            kind: Kind::Text,
            room: room.to_string(),
            mime: Some("text/plain;charset=utf-8".to_string()),
            name: None,
            payload: Some(text.as_bytes().to_vec()),
            size: text.as_bytes().len(),
            sha256: None,
        }
    }

    pub fn new_image(device_id: &str, room: &str, mime: &str, bytes: Vec<u8>) -> Self {
        let size = bytes.len();
        Self {
            event_id: Uuid::new_v4().to_string(),
            device_id: device_id.to_string(),
            sender_name: None,
            ts: crate::now_ms(),
            kind: Kind::Image,
            room: room.to_string(),
            mime: Some(mime.to_string()),
            name: None,
            payload: Some(bytes),
            size,
            sha256: None,
        }
    }

    pub fn new_file(device_id: &str, room: &str, name: &str, mime: &str, bytes: Vec<u8>) -> Self {
        let size = bytes.len();
        Self {
            event_id: Uuid::new_v4().to_string(),
            device_id: device_id.to_string(),
            sender_name: None,
            ts: crate::now_ms(),
            kind: Kind::File,
            room: room.to_string(),
            mime: Some(mime.to_string()),
            name: Some(name.to_string()),
            payload: Some(bytes),
            size,
            sha256: None,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let body = bincode::serialize(self).expect("serialize message");
        let mut out = Vec::with_capacity(MSG_V2_MAGIC.len() + body.len());
        out.extend_from_slice(MSG_V2_MAGIC);
        out.extend_from_slice(&body);
        out
    }

    /// Try decoding a message from raw bytes.
    ///
    /// This function is intentionally tolerant to older on-the-wire formats.
    /// Callers should handle errors without panicking (e.g. drop the frame / reconnect).
    pub fn try_from_bytes(b: &[u8]) -> Result<Self, bincode::Error> {
        if b.len() >= MSG_V2_MAGIC.len() && &b[..MSG_V2_MAGIC.len()] == MSG_V2_MAGIC {
            return bincode::deserialize(&b[MSG_V2_MAGIC.len()..]);
        }

        // Backward compat: v1 had no magic prefix and no `sender_name`.
        match bincode::deserialize::<MessageV1>(b) {
            Ok(v1) => {
                return Ok(Message {
                    event_id: v1.event_id,
                    device_id: v1.device_id,
                    sender_name: None,
                    ts: v1.ts,
                    kind: v1.kind,
                    room: v1.room,
                    mime: v1.mime,
                    name: v1.name,
                    payload: v1.payload,
                    size: v1.size,
                    sha256: v1.sha256,
                });
            }
            Err(e_v1) => {
                // Older compat: v0 may not have `size`/`sha256` fields.
                if let Ok(v0) = bincode::deserialize::<MessageV0>(b) {
                    let size = v0.payload.as_ref().map(|p| p.len()).unwrap_or(0);
                    return Ok(Message {
                        event_id: v0.event_id,
                        device_id: v0.device_id,
                        sender_name: None,
                        ts: v0.ts,
                        kind: v0.kind,
                        room: v0.room,
                        mime: v0.mime,
                        name: v0.name,
                        payload: v0.payload,
                        size,
                        sha256: None,
                    });
                }
                return Err(e_v1);
            }
        }
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        Self::try_from_bytes(b).expect("deserialize message")
    }
}

pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_file_roundtrip_preserves_name() {
        let mut m = Message::new_file("dev", "room", "hello.txt", "text/plain", b"hi".to_vec());
        m.sha256 = Some("abc".to_string());
        m.sender_name = Some("alice".to_string());
        let b = m.to_bytes();
        let m2 = Message::try_from_bytes(&b).expect("decode");
        assert!(matches!(m2.kind, Kind::File));
        assert_eq!(m2.name.as_deref(), Some("hello.txt"));
        assert_eq!(m2.sender_name.as_deref(), Some("alice"));
        assert_eq!(m2.mime.as_deref(), Some("text/plain"));
        assert_eq!(m2.payload.as_deref(), Some(b"hi".as_slice()));
        assert_eq!(m2.sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn message_v1_is_backward_compatible() {
        let v1 = MessageV1 {
            event_id: "e".to_string(),
            device_id: "dev".to_string(),
            ts: 1,
            kind: Kind::Text,
            room: "room".to_string(),
            mime: Some("text/plain".to_string()),
            name: None,
            payload: Some(b"hi".to_vec()),
            size: 2,
            sha256: None,
        };
        let b = bincode::serialize(&v1).expect("serialize v1");
        let m = Message::try_from_bytes(&b).expect("decode");
        assert_eq!(m.device_id, "dev");
        assert_eq!(m.sender_name, None);
        assert!(matches!(m.kind, Kind::Text));
    }
}
