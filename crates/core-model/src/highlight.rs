//! Keyword-highlight profiles: named rule sets ("Cisco", "Linux") that a
//! folder or connection picks through the inheritable `highlight_rules`
//! setting. Profiles sync like folders do.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ObjectId;

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct HighlightProfile {
    pub id: ObjectId,
    pub name: String,
    /// Case-sensitive matching for every rule in the profile.
    #[cfg_attr(feature = "serde", serde(default))]
    pub match_case: bool,
    /// Earlier rules win where matches overlap.
    pub rules: Vec<HighlightRule>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct HighlightRule {
    /// A whole word, or a regular expression when `regex` is set.
    pub text: String,
    /// A palette name ("red", "amber", …) that follows the theme, or "#rrggbb".
    pub color: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub regex: bool,
}
