//! Small settings file (order, last tab, muted/paused, turned-off services, Slack online mode, ...).
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub order: Vec<String>,
    pub last_selected: Option<String>,
    pub disabled: Vec<String>,
    /// service id -> unix seconds until which its banners are muted
    pub muted_until: HashMap<String, f64>,
    pub paused_until: f64,
    /// "off" | "work" | "always"
    pub slack_online_mode: String,
    /// "important" | "all"
    pub gmail_source: String,
    pub last_greeting_day: String,
    pub login_item_configured: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            order: vec![],
            last_selected: None,
            disabled: vec![],
            muted_until: HashMap::new(),
            paused_until: 0.0,
            slack_online_mode: "off".into(),
            gmail_source: "important".into(),
            last_greeting_day: String::new(),
            login_item_configured: false,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        crate::native::settings_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    /// Saved order first (ignoring unknown ids), then any services it doesn't mention.
    pub fn resolved_order(&self, all: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = self.order.iter().filter(|id| all.contains(&id.as_str())).cloned().collect();
        out.dedup();
        for id in all {
            if !out.iter().any(|o| o == id) {
                out.push((*id).to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_keeps_saved_then_adds_missing() {
        let s = Settings { order: vec!["gmail".into(), "bogus".into(), "slack".into()], ..Default::default() };
        assert_eq!(s.resolved_order(&["slack", "whatsapp", "gmail"]), vec!["gmail", "slack", "whatsapp"]);
    }

    #[test]
    fn old_files_missing_fields_still_load() {
        let s: Settings = serde_json::from_str(r#"{"order":["a"]}"#).unwrap();
        assert_eq!(s.slack_online_mode, "off");
        assert_eq!(s.gmail_source, "important");
    }
}
