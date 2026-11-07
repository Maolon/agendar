//! Local-only UI preferences (`<data>/preferences.json`).
//!
//! Preferences never live in the Vault. They carry a content revision so a
//! stale write is refused instead of overwriting a newer choice, and the
//! explicit native-notification consent bit that gates the OS permission
//! request.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{Sha256, UtcTime, canonical_json};
use agendar_core::runtime::{LocalPaths, read_json_value, write_json_atomic};

/// Current local preferences file version.
pub const PREFERENCES_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub v: u8,
    /// Local write counter; the wire revision is the content hash.
    pub revision: u64,
    pub theme: String,
    pub motion: bool,
    pub snoozed: BTreeMap<String, UtcTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_looked_at: Option<UtcTime>,
    pub last_looked_ids: Vec<String>,
    /// Explicit consent to request the OS notification permission. Default off.
    pub notifications: bool,
    /// UI language: `auto` (follow the system), `en`, `zh-Hans` or `zh-Hant`.
    #[serde(default = "auto_language")]
    pub language: String,
    /// Hand-off items the user closed for good on this computer. The step is
    /// untouched; only the item stops showing. Pruned to items still waiting.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub dismissed: BTreeSet<String>,
}

/// At most this many dismissed items are remembered.
pub const MAX_DISMISSED: usize = 256;

fn auto_language() -> String {
    "auto".to_owned()
}

/// The UI languages a person can choose.
pub const LANGUAGES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];

/// The language of text the host writes itself: notifications and the tray
/// tooltip. The app renders everything else from its own catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhHans,
    ZhHant,
}

impl Lang {
    /// `auto` (or anything unknown) follows the system's preferred languages.
    pub fn resolve(preference: &str) -> Self {
        match preference {
            "en" => Self::En,
            "zh-Hans" => Self::ZhHans,
            "zh-Hant" => Self::ZhHant,
            _ => system_lang(),
        }
    }

    /// The same mapping as the app's `languageOfTag`: Chinese with a Hant
    /// script or a TW/HK/MO region is Traditional, other Chinese Simplified.
    pub fn of_tag(tag: &str) -> Option<Self> {
        let tag = tag
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .replace('_', "-")
            .to_ascii_lowercase();
        let mut parts = tag.split('-');
        match parts.next()? {
            "en" => Some(Self::En),
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                let traditional = rest
                    .iter()
                    .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo"));
                let simplified = rest.contains(&"hans");
                Some(if traditional && !simplified {
                    Self::ZhHant
                } else {
                    Self::ZhHans
                })
            }
            _ => None,
        }
    }

    /// One of three translations of a template.
    pub const fn pick(
        self,
        en: &'static str,
        hans: &'static str,
        hant: &'static str,
    ) -> &'static str {
        match self {
            Self::En => en,
            Self::ZhHans => hans,
            Self::ZhHant => hant,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::En => 0,
            Self::ZhHans => 1,
            Self::ZhHant => 2,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::ZhHans,
            2 => Self::ZhHant,
            _ => Self::En,
        }
    }
}

/// The language the host last applied, for code without the preferences at
/// hand (the tray). Set by the host when preferences load or change.
static ACTIVE_LANG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_active_lang(lang: Lang) {
    ACTIVE_LANG.store(lang.code(), std::sync::atomic::Ordering::Relaxed);
}

pub fn active_lang() -> Lang {
    Lang::from_code(ACTIVE_LANG.load(std::sync::atomic::Ordering::Relaxed))
}

/// Fill `{name}` placeholders of a picked template.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = template.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// The system's first preferred language Agendar speaks, else English. macOS
/// keeps the person's order in AppleLanguages (what the app's web view sees);
/// elsewhere the locale variables carry it.
fn system_lang() -> Lang {
    static CACHE: std::sync::OnceLock<Lang> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        #[cfg(target_os = "macos")]
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            && let Some(lang) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|line| {
                    line.trim()
                        .trim_matches(|c| c == '"' || c == ',' || c == ' ')
                })
                .filter(|line| !line.is_empty() && *line != "(" && *line != ")")
                .find_map(Lang::of_tag)
        {
            return lang;
        }
        ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .flat_map(|value| value.split(':').map(str::to_owned).collect::<Vec<_>>())
            .find_map(|tag| Lang::of_tag(&tag))
            .unwrap_or(Lang::En)
    })
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            v: PREFERENCES_VERSION,
            revision: 0,
            theme: "system".to_owned(),
            motion: true,
            snoozed: BTreeMap::new(),
            last_looked_at: None,
            last_looked_ids: Vec::new(),
            notifications: false,
            language: auto_language(),
            dismissed: BTreeSet::new(),
        }
    }
}

impl Preferences {
    pub fn load(data_root: &Path) -> Result<Self> {
        let path = LocalPaths::new(data_root)?.preferences_path();
        let Some(value) = read_json_value(&path)? else {
            return Ok(Self::default());
        };
        let version = value.get("v").and_then(Value::as_u64).ok_or_else(|| {
            AgendarError::validation_at(
                "/preferences",
                "preferences_missing_version",
                "preferences.json has no version field.",
            )
        })?;
        if version != u64::from(PREFERENCES_VERSION) {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                format!("Local preferences version {version} is not supported."),
            )
            .with_detail("preferences_version"));
        }
        let preferences: Self = serde_json::from_value(value).map_err(|error| {
            AgendarError::validation_at(
                "/preferences",
                "preferences_malformed",
                format!("preferences.json has an unexpected shape: {error}."),
            )
        })?;
        preferences.validate()?;
        Ok(preferences)
    }

    fn validate(&self) -> Result<()> {
        if !matches!(self.theme.as_str(), "system" | "light" | "dark") {
            return Err(AgendarError::validation_at(
                "/preferences/theme",
                "preferences_theme",
                "theme must be system, light or dark.",
            ));
        }
        if !LANGUAGES.contains(&self.language.as_str()) {
            return Err(AgendarError::validation_at(
                "/preferences/language",
                "preferences_language",
                "language must be auto, en, zh-Hans or zh-Hant.",
            ));
        }
        if self.dismissed.len() > MAX_DISMISSED {
            return Err(AgendarError::validation_at(
                "/preferences/dismissed",
                "preferences_dismissed",
                "dismissed must contain at most 256 ids.",
            ));
        }
        if self.last_looked_ids.len() > 128 {
            return Err(AgendarError::validation_at(
                "/preferences/last_looked_ids",
                "preferences_ids",
                "last_looked_ids must contain at most 128 ids.",
            ));
        }
        Ok(())
    }

    /// The wire revision: a content hash, never the minute-tick generation.
    pub fn wire_revision(&self) -> Sha256 {
        let mut value = json!([
            "preferences",
            self.revision,
            self.theme,
            self.motion,
            self.snoozed,
            self.last_looked_at,
            self.last_looked_ids,
            self.notifications,
        ]);
        // A chosen language joins the hash; `auto` keeps earlier revisions.
        if self.language != "auto"
            && let Value::Array(items) = &mut value
        {
            items.push(json!(self.language));
        }
        // Dismissed items join only when there are some, for the same reason.
        if !self.dismissed.is_empty()
            && let Value::Array(items) = &mut value
        {
            items.push(json!(["dismissed", self.dismissed]));
        }
        Sha256::from_bytes(canonical_json(&value).as_bytes())
    }

    /// The frontend `Preferences` projection.
    pub fn view(&self) -> Value {
        json!({
            "theme": self.theme,
            "motion": self.motion,
            "snoozed": self.snoozed,
            "last_looked_at": self.last_looked_at,
            "last_looked_ids": self.last_looked_ids,
            "revision": self.wire_revision(),
            "notifications": self.notifications,
            "language": self.language,
            "dismissed": self.dismissed,
        })
    }

    pub fn write(&mut self, data_root: &Path) -> Result<()> {
        self.validate()?;
        let path = LocalPaths::new(data_root)?.preferences_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json_atomic(&path, self)?;
        Ok(())
    }
}

#[cfg(test)]
mod lang_tests {
    use super::*;

    #[test]
    fn language_tags_map_like_the_app() {
        assert_eq!(Lang::of_tag("zh-Hans-CN"), Some(Lang::ZhHans));
        assert_eq!(Lang::of_tag("zh-SG"), Some(Lang::ZhHans));
        assert_eq!(Lang::of_tag("zh_TW.UTF-8"), Some(Lang::ZhHant));
        assert_eq!(Lang::of_tag("zh-Hant-HK"), Some(Lang::ZhHant));
        assert_eq!(Lang::of_tag("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::of_tag("fr-FR"), None);
        assert_eq!(Lang::of_tag("C"), None);
        assert_eq!(Lang::resolve("zh-Hant"), Lang::ZhHant);
        assert_eq!(fill("{a} and {b}", &[("a", "1"), ("b", "2")]), "1 and 2");
    }
}
