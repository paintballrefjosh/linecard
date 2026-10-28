//! JSON shapes sent to the UI. Keep in sync with app/src/types.ts.

use linecard_core_model::{ConnectionKind, Tree};
use linecard_core_sync::{ChangeKind, ChangeTracker, PendingChange, Value};
use serde::Serialize;

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
#[serde(tag = "event", rename_all = "lowercase")]
pub enum SessionEventDto {
    Connected,
    Data { data: Vec<u8> },
    Title { title: String },
    Disconnected { reason: Option<String> },
}

impl From<linecard_proto::SessionEvent> for SessionEventDto {
    fn from(e: linecard_proto::SessionEvent) -> Self {
        use linecard_proto::SessionEvent as E;
        match e {
            E::Connected => SessionEventDto::Connected,
            E::Data(data) => SessionEventDto::Data { data },
            E::Title(title) => SessionEventDto::Title { title },
            E::Disconnected { reason } => SessionEventDto::Disconnected { reason },
        }
    }
}
