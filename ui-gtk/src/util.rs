pub fn chrono_like_timestamp() -> String {
    // Tiny helper: avoid pulling chrono dependency.
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}", ms)
}

pub fn fake_remote_device_id() -> String {
    // Stable enough uniqueness without adding a uuid dependency.
    format!("ui-test-{}-{}", std::process::id(), chrono_like_timestamp())
}
