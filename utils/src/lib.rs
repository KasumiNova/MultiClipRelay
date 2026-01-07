use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

impl Message {
    pub fn new_join(device_id: &str, room: &str) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            device_id: device_id.to_string(),
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
        bincode::serialize(self).expect("serialize message")
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        bincode::deserialize(b).expect("deserialize message")
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
        let b = m.to_bytes();
        let m2 = Message::from_bytes(&b);
        assert!(matches!(m2.kind, Kind::File));
        assert_eq!(m2.name.as_deref(), Some("hello.txt"));
        assert_eq!(m2.mime.as_deref(), Some("text/plain"));
        assert_eq!(m2.payload.as_deref(), Some(b"hi".as_slice()));
        assert_eq!(m2.sha256.as_deref(), Some("abc"));
    }
}
