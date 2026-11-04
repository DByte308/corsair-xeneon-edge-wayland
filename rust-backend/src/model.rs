// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Window {
    pub id: String,
    pub title: String,
    pub app: String,
    pub active: bool,
    pub minimized: bool,
    // EdgeLine's own app windows are Edge apps too; older feeds omit the flag.
    #[serde(default)]
    pub own: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub available: bool,
    #[serde(default)]
    pub error: String,
    pub windows: Vec<Window>,
}

impl Snapshot {
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > 1024 * 1024 {
            bail!("window report exceeds 1 MiB");
        }
        let mut value: Self = serde_json::from_str(input)?;
        value.windows.truncate(100);
        value.windows.retain(|w| canonical_uuid(&w.id).is_ok());
        for window in &mut value.windows {
            window.title = window.title.chars().take(256).collect();
            window.app = window.app.chars().take(128).collect();
        }
        Ok(value)
    }
}

pub fn canonical_uuid(input: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(input)?;
    Ok(format!("{{{id}}}"))
}

pub fn shortcut_suffix(input: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(input)?.simple().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_validation_and_format() {
        let id = "{12345678-abcd-1234-1234-123456789abc}";
        assert_eq!(canonical_uuid(id).unwrap(), id);
        assert_eq!(
            shortcut_suffix(id).unwrap(),
            "12345678abcd12341234123456789abc"
        );
        assert!(canonical_uuid("; arbitrary-code").is_err());
    }
    #[test]
    fn report_filters_invalid_ids_and_bounds_unicode() {
        let input = serde_json::json!({"available":true,"windows":[
            {"id":"bad","title":"x","app":"x","active":false,"minimized":false},
            {"id":"12345678-abcd-1234-1234-123456789abc","title":"é".repeat(300),"app":"test","active":true,"minimized":false}
        ]});
        let snapshot = Snapshot::parse(&input.to_string()).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].title.chars().count(), 256);
        assert!(snapshot.windows[0].active);
    }
    #[test]
    fn oversized_or_malformed_reports_fail() {
        assert!(Snapshot::parse(&"x".repeat(1024 * 1024 + 1)).is_err());
        assert!(Snapshot::parse("{}").is_err());
    }
}
