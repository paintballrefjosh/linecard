//! The GitHub transport against a small in-process imitation of the parts of
//! GitHub's REST API it uses (repo, trees, blobs, contents).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use linecard_sync::crypto::{open, seal};
use linecard_sync::github::GitHubTransport;
use linecard_sync::{DeviceKey, GroupKey};
use serde_json::{json, Value};

const TOKEN: &str = "test-token";

#[derive(Default)]
struct Repo {
    /// path -> (sha, base64 content)
    files: BTreeMap<String, (String, String)>,
    commits: usize,
}

type Shared = Arc<Mutex<Repo>>;

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn denied() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"message": "Bad credentials"}))).into_response()
}

async fn repo(h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    Json(json!({"default_branch": "main", "permissions": {"push": true}})).into_response()
}

async fn tree(State(s): State<Shared>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    let r = s.lock().unwrap();
    if r.files.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"message": "Git Repository is empty."})),
        )
            .into_response();
    }
    let tree: Vec<Value> = r
        .files
        .iter()
        .map(|(p, (sha, _))| json!({"path": p, "sha": sha, "type": "blob"}))
        .collect();
    Json(json!({"tree": tree, "truncated": false})).into_response()
}

async fn blob(State(s): State<Shared>, Path((_, _, sha)): Path<(String, String, String)>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    let r = s.lock().unwrap();
    match r.files.values().find(|(s, _)| *s == sha) {
        // GitHub wraps base64 at 60 columns.
        Some((_, content)) => {
            let wrapped: Vec<String> = content
                .as_bytes()
                .chunks(60)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            Json(json!({"content": wrapped.join("\n"), "encoding": "base64"})).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_contents(
    State(s): State<Shared>,
    Path((_, _, path)): Path<(String, String, String)>,
    h: HeaderMap,
) -> Response {
    if !authed(&h) {
        return denied();
    }
    match s.lock().unwrap().files.get(&path) {
        Some((sha, _)) => Json(json!({"sha": sha})).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
    }
}

async fn put_contents(
    State(s): State<Shared>,
    Path((_, _, path)): Path<(String, String, String)>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authed(&h) {
        return denied();
    }
    let mut r = s.lock().unwrap();
    let given = body["sha"].as_str().map(str::to_string);
    if let Some((sha, _)) = r.files.get(&path) {
        if given.as_deref() != Some(sha.as_str()) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"message": "sha wasn't supplied"})),
            )
                .into_response();
        }
    }
    r.commits += 1;
    let sha = format!("{:040x}", r.commits);
    r.files
        .insert(path, (sha.clone(), body["content"].as_str().unwrap_or("").to_string()));
    (StatusCode::CREATED, Json(json!({"content": {"sha": sha}}))).into_response()
}

async fn start() -> (String, Shared) {
    let state: Shared = Default::default();
    let app = Router::new()
        .route("/repos/{o}/{r}", get(repo))
        .route("/repos/{o}/{r}/git/trees/{branch}", get(tree))
        .route("/repos/{o}/{r}/git/blobs/{sha}", get(blob))
        .route("/repos/{o}/{r}/contents/{*path}", get(get_contents).put(put_contents))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state)
}

fn client(api: &str, group: &GroupKey, device: &str) -> GitHubTransport {
    GitHubTransport::new(api, "someone/linecard-sync", TOKEN, &group.group_id(), device).unwrap()
}

#[tokio::test]
async fn devices_exchange_batches_and_heartbeats_through_a_repo() {
    let (api, repo) = start().await;
    let group = GroupKey::generate();
    let (ka, kb) = (DeviceKey::generate(), DeviceKey::generate());
    let a = client(&api, &group, "laptop");
    let b = client(&api, &group, "desktop");
    a.check().await.unwrap();

    // Empty repository: nothing to pull yet.
    assert!(b.pull(&Default::default()).await.unwrap().0.is_empty());

    a.push(&seal(&group, &ka, "laptop", &1)).await.unwrap();
    a.push(&seal(&group, &ka, "laptop", &2)).await.unwrap();
    b.push(&seal(&group, &kb, "desktop", &10)).await.unwrap();

    let (got, cursor) = b.pull(&Default::default()).await.unwrap();
    let nums: Vec<i32> = got.iter().map(|r| open(&group, &r.sealed).unwrap()).collect();
    assert_eq!(nums, vec![1, 2], "the other device's batches, in order; not our own");
    assert_eq!(cursor.get("laptop"), Some(&2));
    assert!(b.pull(&cursor).await.unwrap().0.is_empty(), "nothing new");

    // A fresh client for the same device keeps counting where it left off.
    let a2 = client(&api, &group, "laptop");
    a2.push(&seal(&group, &ka, "laptop", &3)).await.unwrap();
    let (got, _) = b.pull(&cursor).await.unwrap();
    assert_eq!(open::<i32>(&group, &got[0].sealed).unwrap(), 3);

    // Heartbeats are replaced in place.
    a.put_device(&seal(&group, &ka, "laptop", &"v1")).await.unwrap();
    a.put_device(&seal(&group, &ka, "laptop", &"v2")).await.unwrap();
    let devs = b.devices().await.unwrap();
    assert_eq!(devs.len(), 1);
    assert_eq!(open::<String>(&group, &devs[0].sealed).unwrap(), "v2");

    // Only ciphertext in the repository.
    let r = repo.lock().unwrap();
    assert!(r
        .files
        .keys()
        .all(|p| p.starts_with(&format!("linecard-{}/", group.group_id()))));
}

#[tokio::test]
async fn bad_tokens_and_names_are_explained() {
    let (api, _) = start().await;
    let group = GroupKey::generate();
    assert!(GitHubTransport::new(&api, "not-a-repo", TOKEN, &group.group_id(), "d").is_err());
    let wrong = GitHubTransport::new(&api, "a/b", "nope", &group.group_id(), "d").unwrap();
    let err = wrong.check().await.unwrap_err().to_string();
    assert!(err.contains("refused the token"), "{err}");
}
