//! Importers produce an [`Import`]: a preview the UI shows before anything is
//! written ("142 sessions in 18 folders, 3 warnings"). The app then turns it
//! into folders and connections in one undoable batch.

use linecard_core_model::{ConnectionKind, Endpoint};

pub mod mremoteng;
pub mod putty;
pub mod securecrt;
pub mod ssh_config;

/// One connection found in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedConnection {
    /// Folder names from the top of the source tree, e.g. ["DC1", "Spine"].
    pub folder_path: Vec<String>,
    pub name: String,
    pub kind: ConnectionKind,
    pub endpoint: Endpoint,
    pub username: Option<String>,
    /// Jump host / firewall name as the source called it (resolved later).
    pub jump_host: Option<String>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Import {
    pub source: String,
    /// Every folder path, including empty folders.
    pub folders: Vec<Vec<String>>,
    pub connections: Vec<ImportedConnection>,
    /// Human-readable problems ("Skipped 'modem1': TAPI is not supported").
    pub warnings: Vec<String>,
}

impl Import {
    pub fn summary(&self) -> String {
        format!(
            "{} sessions in {} folders, {} warning{}",
            self.connections.len(),
            self.folders.len(),
            self.warnings.len(),
            if self.warnings.len() == 1 { "" } else { "s" }
        )
    }
}

// Planned importers (Phase 1):
// - mremoteng: confCons.xml — <Node Type="Container"> = folder, Type="Connection";
//   Protocol (SSH2, Telnet, RDP, VNC, ...), Hostname, Port, Username; Inherit* flags map to
//   folder defaults. Passwords are AES-GCM with a user master password ("mR3m" default).
// - superputty: Sessions.xml — <SessionData SessionId="Folder/Sub/Name" Host Port Proto
//   Username PuttySession>; resolve PuttySession against the PuTTY registry.
// - putty: HKCU\Software\SimonTatham\PuTTY\Sessions (Windows registry).
// - ssh_config: Host blocks with HostName, Port, User, ProxyJump.
