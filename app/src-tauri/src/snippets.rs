//! The button bar's saved commands. They sync with the library, app-wide or
//! tied to a folder (and so to the connections inside it).

use linecard_core_model::{Snippet, Tree};
use tauri::{AppHandle, State};

use crate::{edit, err, new_id, AppState, CmdResult};

fn sorted(mut v: Vec<Snippet>) -> Vec<Snippet> {
    v.sort_by(|a, b| a.order.total_cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    v
}

/// `folder` and the folders above it.
fn ancestors(tree: &Tree, folder: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = folder.map(String::from);
    while let Some(id) = at {
        if out.contains(&id) {
            break; // a cycle from a bad sync; stop rather than spin
        }
        at = tree.folder(&id).ok().and_then(|f| f.parent.clone());
        out.push(id);
    }
    out
}

/// What shows in the bar for a connection: app-wide snippets, then those of
/// its folders (a snippet whose folder is gone counts as app-wide).
pub fn for_connection(tree: &Tree, connection: Option<&str>) -> Vec<Snippet> {
    let folder = connection
        .and_then(|c| tree.connection(c).ok())
        .and_then(|c| c.folder.clone());
    let above = ancestors(tree, folder.as_deref());
    sorted(
        tree.snippets
            .values()
            .filter(|s| match &s.folder {
                None => true,
                Some(f) => above.contains(f) || tree.folder(f).is_err(),
            })
            .cloned()
            .collect(),
    )
}

/// Every snippet, for the manager.
#[tauri::command]
pub fn list_snippets(state: State<AppState>) -> CmdResult<Vec<Snippet>> {
    let lib = state.lib.lock().map_err(err)?;
    Ok(sorted(lib.tree().snippets.values().cloned().collect()))
}

/// The snippets for a connection's button bar (all app-wide ones for a quick connect).
#[tauri::command]
pub fn snippets_for(state: State<AppState>, connection_id: Option<String>) -> CmdResult<Vec<Snippet>> {
    let lib = state.lib.lock().map_err(err)?;
    Ok(for_connection(lib.tree(), connection_id.as_deref()))
}

/// Create (empty id) or replace a snippet. Returns its id.
#[tauri::command]
pub fn save_snippet(app: AppHandle, state: State<AppState>, mut snippet: Snippet) -> CmdResult<String> {
    snippet.name = snippet.name.trim().to_string();
    if snippet.name.is_empty() {
        return Err("Name the button".into());
    }
    if snippet.text.is_empty() {
        return Err("Enter the text to send".into());
    }
    let id = if snippet.id.is_empty() {
        new_id()
    } else {
        snippet.id.clone()
    };
    snippet.id = id.clone();
    edit(&app, &state, |t| {
        if let Some(f) = &snippet.folder {
            t.folder(f).map_err(err)?;
        }
        let is_new = !t.snippets.contains_key(&id);
        if is_new {
            snippet.order = t.snippets.values().map(|s| s.order).fold(0.0, f64::max) + 1.0;
        } else {
            snippet.order = t.snippets[&id].order;
        }
        t.snippets.insert(id.clone(), snippet);
        Ok(())
    })?;
    Ok(id)
}

#[tauri::command]
pub fn delete_snippet(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    edit(&app, &state, |t| {
        t.snippets.remove(&id);
        Ok(())
    })
}

/// Put snippets in this order (ids not listed keep their place after them).
#[tauri::command]
pub fn reorder_snippets(app: AppHandle, state: State<AppState>, ids: Vec<String>) -> CmdResult<()> {
    edit(&app, &state, |t| {
        for (i, id) in ids.iter().enumerate() {
            if let Some(s) = t.snippets.get_mut(id) {
                s.order = i as f64;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings};

    #[test]
    fn bar_shows_app_wide_then_folder_snippets_for_the_connections_ancestry() {
        let folder = |id: &str, parent: Option<&str>| Folder {
            id: id.into(),
            name: id.into(),
            parent: parent.map(Into::into),
            order: 0.0,
            color: None,
            defaults: Settings::default(),
        };
        let mut tree = Tree::default();
        tree.add_folder(folder("dc", None)).unwrap();
        tree.add_folder(folder("leaf", Some("dc"))).unwrap();
        tree.add_folder(folder("other", None)).unwrap();
        tree.add_connection(Connection {
            id: "sw".into(),
            name: "sw".into(),
            folder: Some("leaf".into()),
            order: 0.0,
            kind: ConnectionKind::Ssh,
            endpoint: Endpoint::network(ConnectionKind::Ssh, "sw.lab"),
            overrides: Settings::default(),
            notes: String::new(),
            tags: vec![],
            forwards: vec![],
        })
        .unwrap();
        let snip = |id: &str, folder: Option<&str>, order: f64| Snippet {
            id: id.into(),
            name: id.into(),
            text: "x".into(),
            folder: folder.map(Into::into),
            order,
            enter: true,
        };
        for s in [
            snip("everywhere", None, 3.0),
            snip("dc-wide", Some("dc"), 1.0),
            snip("leaf-only", Some("leaf"), 2.0),
            snip("elsewhere", Some("other"), 0.0),
            snip("orphan", Some("gone"), 4.0),
        ] {
            tree.snippets.insert(s.id.clone(), s);
        }
        let names = |c: Option<&str>| for_connection(&tree, c).into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(names(Some("sw")), ["dc-wide", "leaf-only", "everywhere", "orphan"]);
        assert_eq!(names(None), ["everywhere", "orphan"]);
    }
}
