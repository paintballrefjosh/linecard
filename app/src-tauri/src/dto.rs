//! JSON shapes sent to the UI. Keep in sync with app/src/types.ts.

use linecard_core_model::{Connection, ConnectionKind, Endpoint, FlowControl, Parity, SerialSettings, Tree};
use linecard_core_sync::{ChangeKind, ChangeTracker, PendingChange, Value};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDiffDto {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEntryDto {
    pub op_id: String,
    pub kind: ChangeKind,
    pub device: String,
    pub at: u64,
    pub fields: Vec<FieldDiffDto>,
}

#[derive(Serialize)]
pub struct PendingChangeDto {
    pub kind: ChangeKind,
    pub entries: Vec<ChangeEntryDto>,
}

impl From<&PendingChange> for PendingChangeDto {
    fn from(p: &PendingChange) -> Self {
        let show = |v: &Option<Value>| v.as_ref().map(|v| v.to_string());
        PendingChangeDto {
            kind: p.kind,
            entries: p
                .entries
                .iter()
                .map(|e| ChangeEntryDto {
                    op_id: crate::encode_op_id(&e.op),
                    kind: e.kind,
                    device: e.op.device.clone(),
                    at: e.op.wall_ms,
                    fields: e
                        .fields
                        .iter()
                        .filter(|f| f.field != linecard_core_sync::DELETED_FIELD)
                        .map(|f| FieldDiffDto {
                            field: f.field.clone(),
                            before: show(&f.before),
                            after: show(&f.after),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TreeNodeDto {
    Folder {
        id: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        change: Option<PendingChangeDto>,
        children: Vec<TreeNodeDto>,
    },
    Connection {
        id: String,
        name: String,
        kind: ConnectionKind,
        endpoint: String,
        #[serde(rename = "tabColor", skip_serializing_if = "Option::is_none")]
        tab_color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        change: Option<PendingChangeDto>,
    },
}

/// Build the UI tree for `parent` (None = top level), folders first.
pub fn tree_dto(tree: &Tree, changes: &ChangeTracker, parent: Option<&str>) -> Vec<TreeNodeDto> {
    let change = |id: &str| changes.get(id).map(PendingChangeDto::from);
    let folders = tree.child_folders(parent).into_iter().map(|f| TreeNodeDto::Folder {
        id: f.id.clone(),
        name: f.name.clone(),
        color: f.color.clone(),
        change: change(&f.id),
        children: tree_dto(tree, changes, Some(&f.id)),
    });
    let conns = tree
        .child_connections(parent)
        .into_iter()
        .map(|c| TreeNodeDto::Connection {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: c.kind,
            endpoint: c.endpoint.label(),
            tab_color: tree.resolve(&c.id).ok().and_then(|r| r.settings.tab_color),
            change: change(&c.id),
        });
    folders.chain(conns).collect()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialDto {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: FlowControl,
}

/// The connection editor's form. `id: None` creates a new connection.
/// Empty `username` / `tabColor` mean "inherit".
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDraftDto {
    pub id: Option<String>,
    pub folder_id: Option<String>,
    pub name: String,
    pub kind: ConnectionKind,
    pub host: String,
    pub port: u16,
    pub serial: Option<SerialDto>,
    pub username: Option<String>,
    pub tab_color: Option<String>,
    /// Id of an SSH connection to tunnel through; empty = inherit.
    #[serde(default)]
    pub jump_host: Option<String>,
    pub notes: String,
}

impl From<&Connection> for ConnectionDraftDto {
    fn from(c: &Connection) -> Self {
        let (host, port, serial) = match &c.endpoint {
            Endpoint::Network { host, port } => (host.clone(), *port, None),
            Endpoint::Serial(s) => (
                String::new(),
                0,
                Some(SerialDto {
                    port: s.port.clone(),
                    baud: s.baud,
                    data_bits: s.data_bits,
                    parity: s.parity,
                    stop_bits: s.stop_bits,
                    flow: s.flow,
                }),
            ),
        };
        ConnectionDraftDto {
            id: Some(c.id.clone()),
            folder_id: c.folder.clone(),
            name: c.name.clone(),
            kind: c.kind,
            host,
            port,
            serial,
            username: c.overrides.username.clone(),
            tab_color: c.overrides.tab_color.clone(),
            jump_host: c.overrides.jump_host.clone(),
            notes: c.notes.clone(),
        }
    }
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl ConnectionDraftDto {
    /// Validate the form and write it onto `conn` (everything except id,
    /// folder and order, which the caller owns).
    pub fn apply_to(&self, conn: &mut Connection) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        conn.endpoint = if self.kind == ConnectionKind::Serial {
            let s = self.serial.as_ref().ok_or("Serial settings are required")?;
            if s.port.trim().is_empty() {
                return Err("Serial port is required".into());
            }
            Endpoint::Serial(SerialSettings {
                port: s.port.trim().into(),
                baud: s.baud,
                data_bits: s.data_bits,
                parity: s.parity,
                stop_bits: s.stop_bits,
                flow: s.flow,
            })
        } else {
            let host = self.host.trim();
            if host.is_empty() {
                return Err("Host is required".into());
            }
            if self.port == 0 {
                return Err("Port must be 1–65535".into());
            }
            Endpoint::Network {
                host: host.into(),
                port: self.port,
            }
        };
        conn.name = name.into();
        conn.kind = self.kind;
        conn.overrides.username = non_empty(&self.username);
        conn.overrides.tab_color = non_empty(&self.tab_color);
        conn.overrides.jump_host = non_empty(&self.jump_host).filter(|j| Some(j) != self.id.as_ref());
        conn.notes = self.notes.clone();
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub os: String,
    pub app_version: String,
    pub protocol_version: u32,
    pub last_seen: u64,
    pub transport: String,
    pub pending_ops: u32,
    pub is_this_device: bool,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SyncStateDto {
    Off,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHostsLineDto {
    pub path: String,
    pub line: usize,
}

#[derive(Clone, Serialize)]
pub struct PromptFieldDto {
    pub label: String,
    pub echo: bool,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum SessionEventDto {
    Connected,
    Data {
        data: Vec<u8>,
    },
    Title {
        title: String,
    },
    /// `changed` is set when a different key is on record.
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        changed: Option<KnownHostsLineDto>,
    },
    Auth {
        title: String,
        instructions: String,
        fields: Vec<PromptFieldDto>,
        /// Offer "Save password" with this initial state; null = don't offer.
        remember: Option<bool>,
    },
    Disconnected {
        reason: Option<String>,
    },
}

impl From<linecard_proto::SessionEvent> for SessionEventDto {
    fn from(e: linecard_proto::SessionEvent) -> Self {
        use linecard_proto::{HostKeyStatus, SessionEvent as E};
        match e {
            E::Connected => SessionEventDto::Connected,
            E::Data(data) => SessionEventDto::Data { data },
            E::Title(title) => SessionEventDto::Title { title },
            E::HostKey(k) => SessionEventDto::HostKey {
                host: k.host,
                port: k.port,
                algorithm: k.algorithm,
                fingerprint: k.fingerprint,
                changed: match k.status {
                    HostKeyStatus::Unknown => None,
                    HostKeyStatus::Changed { known_hosts, line } => Some(KnownHostsLineDto {
                        path: known_hosts.display().to_string(),
                        line,
                    }),
                },
            },
            E::Auth(p) => SessionEventDto::Auth {
                title: p.title,
                instructions: p.instructions,
                fields: p
                    .fields
                    .into_iter()
                    .map(|f| PromptFieldDto {
                        label: f.label,
                        echo: f.echo,
                    })
                    .collect(),
                remember: p.remember,
            },
            E::Disconnected { reason } => SessionEventDto::Disconnected { reason },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKeyDecisionDto {
    AcceptAndSave,
    AcceptOnce,
    Reject,
}

/// The UI's answer to a `hostKey` or `auth` event.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PromptReplyDto {
    HostKey {
        decision: HostKeyDecisionDto,
    },
    Fields {
        values: Vec<String>,
        #[serde(default)]
        remember: bool,
    },
    Cancel,
}

impl From<PromptReplyDto> for linecard_proto::PromptReply {
    fn from(r: PromptReplyDto) -> Self {
        use linecard_proto::{HostKeyDecision as D, PromptReply as R};
        match r {
            PromptReplyDto::HostKey { decision } => R::HostKey(match decision {
                HostKeyDecisionDto::AcceptAndSave => D::AcceptAndSave,
                HostKeyDecisionDto::AcceptOnce => D::AcceptOnce,
                HostKeyDecisionDto::Reject => D::Reject,
            }),
            PromptReplyDto::Fields { values, remember } => R::Fields { values, remember },
            PromptReplyDto::Cancel => R::Cancel,
        }
    }
}
