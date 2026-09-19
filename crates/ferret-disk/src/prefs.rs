//! The choices worth surviving a restart. Every field carries
//! `serde(default)`, so a file from an older or newer build still loads.

use serde::{Deserialize, Serialize};

use crate::i18n::Lang;
use crate::theme::Theme;

pub const KEY: &str = "ferret-disk-prefs";

/// Which size the map and the lists rank by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Metric {
    /// Clusters occupied — what fills a disk.
    #[default]
    OnDisk,
    /// Logical size — what Explorer shows.
    Size,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub lang: Lang,
    pub theme: Theme,
    /// Drive letter, or empty for "the system drive".
    pub drive: String,
    pub metric: Metric,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            lang: Lang::default(),
            theme: Theme::default(),
            drive: String::new(),
            metric: Metric::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_file_keeps_what_it_says_and_defaults_the_rest() {
        let loaded: Prefs = serde_json::from_str(r#"{"drive":"D"}"#).unwrap();
        assert_eq!(loaded.drive, "D");
        assert_eq!(loaded.metric, Metric::OnDisk);
    }
}
