use std::collections::BTreeMap;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ObjectId;

/// Declares the inheritable settings once and generates the struct plus the
/// field-by-field merge logic, so adding a setting is a one-line change.
macro_rules! inheritable {
    ($( $(#[$doc:meta])* $field:ident : $ty:ty ),* $(,)?) => {
        /// Settings that a folder can define as defaults and a connection can
        /// override. `None` means "not set here, inherit".
        #[derive(Debug, Clone, PartialEq, Default)]
        #[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(default))]
        pub struct Settings {
            $( $(#[$doc])* pub $field: Option<$ty>, )*
        }

        impl Settings {
            /// Names of every inheritable field, in declaration order.
            pub const FIELDS: &'static [&'static str] = &[$( stringify!($field) ),*];

            /// Fill every unset field in `self` from `lower`, recording `source`
            /// for each field that was filled.
            pub(crate) fn fill_from(
                &mut self,
                lower: &Settings,
                source: &Source,
                sources: &mut BTreeMap<&'static str, Source>,
            ) {
                $(
                    if self.$field.is_none() && lower.$field.is_some() {
                        self.$field = lower.$field.clone();
                        sources.insert(stringify!($field), source.clone());
                    }
                )*
            }

            /// Names of the fields that are set (`Some`).
            pub fn set_fields(&self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $( if self.$field.is_some() { out.push(stringify!($field)); } )*
                out
            }

            pub fn is_empty(&self) -> bool {
                self.set_fields().is_empty()
            }
        }
    };
}

inheritable! {
    /// Login username.
    username: String,
    /// Reference to a credential in the vault (never the secret itself).
    credential_id: ObjectId,
    /// Connection id of a jump host (SSH ProxyJump).
    jump_host: ObjectId,
    /// Tab colour, e.g. "#d64545" for production.
    tab_color: String,
    /// Terminal profile id (font, colours, scrollback).
    terminal_profile: ObjectId,
    /// Keyword-highlight rule set id.
    highlight_rules: ObjectId,
    /// Whether session logging is on.
    log_enabled: bool,
    /// Log filename template, e.g. "%folder/%host_%Y%m%d_%H%M.log".
    log_path_template: String,
    /// Prefix each logged line with a timestamp.
    log_timestamps: bool,
    /// Keepalive interval in seconds (0 = off).
    keepalive_secs: u32,
    /// Reconnect automatically when the session drops.
    auto_reconnect: bool,
}

/// Where an effective setting came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "from", content = "id", rename_all = "lowercase")
)]
pub enum Source {
    /// Set on the connection itself.
    Own,
    /// Inherited from this folder.
    Folder(ObjectId),
    /// App-wide default.
    Global,
}

/// Effective settings for a connection plus the source of each value.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Resolved {
    pub settings: Settings,
    pub sources: BTreeMap<&'static str, Source>,
}

impl Resolved {
    pub fn source_of(&self, field: &str) -> Option<&Source> {
        self.sources.get(field)
    }
}
