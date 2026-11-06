//! Named credentials: a user name and password kept once and picked by many
//! connections through the inheritable `credential_id` setting, so changing
//! the password in one place updates every device that uses it.
//!
//! Only the name and user name are here (and sync like folders do). The
//! password lives in the keychain under [`Credential::slot`], the same slot
//! a login offers to save into, and syncs only through the opt-in vault.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ObjectId;

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct Credential {
    pub id: ObjectId,
    /// What pickers show, e.g. "NetOps admin".
    pub name: String,
    /// The user name to log in as; empty = the connection's own.
    #[cfg_attr(feature = "serde", serde(default))]
    pub username: String,
}

impl Credential {
    /// The keychain slot holding this credential's password.
    pub fn slot(id: &str) -> String {
        format!("cred:{id}")
    }
}

/// One step of a connection's logon actions: wait for some text, then send
/// a line. Network devices use these for `enable`, `terminal length 0` or a
/// jump-box menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct LogonStep {
    /// Text to wait for (plain, case-sensitive); empty = send straight away.
    #[cfg_attr(feature = "serde", serde(default))]
    pub expect: String,
    /// The line to send; Enter follows it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub send: String,
    /// Send this credential's saved password instead of `send` (an enable
    /// secret, say). The password never passes through the window.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    pub password_of: Option<ObjectId>,
}
