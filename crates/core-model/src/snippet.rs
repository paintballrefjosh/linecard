//! Saved commands for the button bar: text to send to the focused session
//! with one click. A snippet is app-wide, or belongs to a folder and shows
//! for the connections inside it (and inside its subfolders). Snippets sync
//! like folders do.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ObjectId;

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct Snippet {
    pub id: ObjectId,
    /// The button's label.
    pub name: String,
    /// What gets sent (may span several lines).
    pub text: String,
    /// The folder it belongs to; `None` = every connection.
    #[cfg_attr(feature = "serde", serde(default))]
    pub folder: Option<ObjectId>,
    /// Sort position among its siblings.
    #[cfg_attr(feature = "serde", serde(default))]
    pub order: f64,
    /// Press Enter after the text.
    #[cfg_attr(feature = "serde", serde(default))]
    pub enter: bool,
}
