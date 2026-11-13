use super::*;
use std::collections::VecDeque;
struct Step {
    path: String,
    query: Option<BTreeMap<String, String>>,
    body: bool,
    result: std::result::Result<Value, &'static str>,
    raw: Option<Vec<u8>>,
}
type HttpCall = (String, Vec<(String, String)>, Option<Value>);
struct Mock {
    steps: Mutex<VecDeque<Step>>,
    calls: Mutex<Vec<HttpCall>>,
}
impl Mock {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            calls: Mutex::new(vec![]),
        })
    }
    fn done(&self) {
        assert!(self.steps.lock().unwrap().is_empty(), "unconsumed requests");
    }
}
impl Http for Mock {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        c: &Cancellation,
    ) -> Result<Vec<u8>> {
        c.check()?;
        self.calls
            .lock()
            .unwrap()
            .push((path.into(), query.to_vec(), body.clone()));
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request");
        assert_eq!(path, step.path);
        assert_eq!(body.is_some(), step.body);
        if let Some(expected) = step.query {
            assert_eq!(query.iter().cloned().collect::<BTreeMap<_, _>>(), expected);
        }
        let value = step.result.map_err(invalid)?;
        if let Some(bytes) = step.raw {
            assert!(raw);
            assert!(bytes.len() <= limit);
            Ok(bytes)
        } else {
            Ok(serde_json::to_vec(&value).unwrap())
        }
    }
}
fn get(path: &str, result: Value) -> Step {
    Step {
        path: path.into(),
        query: None,
        body: false,
        result: Ok(result),
        raw: None,
    }
}
fn exact(mut s: Step, q: &[(&str, &str)]) -> Step {
    s.query = Some(
        q.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    s
}
fn gitea(m: Arc<Mock>) -> Gitea {
    Gitea {
        http: m,
        trees: Mutex::new(BTreeMap::new()),
    }
}
fn github(m: Arc<Mock>) -> Github {
    Github {
        http: m,
        owner: "owner".into(),
        repo: "repo".into(),
        trees: Mutex::new(BTreeMap::new()),
        commits: Mutex::new(BTreeMap::new()),
    }
}
fn id(n: u32) -> String {
    format!("{n:040x}")
}
fn blob(bytes: &[u8]) -> Entry {
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    Entry {
        name: "file.xml".into(),
        oid: hex::encode(h.finalize()),
        size: bytes.len() as u64,
        kind: EntryKind::Blob,
        mode: "100644".into(),
    }
}
fn tree(id: u32, rows: Value) -> Value {
    json!({"sha":self::id(id),"tree":rows,"truncated":false})
}
fn entry(name: &str, n: u32, kind: &str, mode: &str) -> Value {
    json!({"path":name,"sha":id(n),"type":kind,"mode":mode,"size":1})
}
#[test]
fn identities_reject_cross_origin_and_preserve_legacy() {
    assert_eq!(
        canonical_server("HTTPS://Git.Example:443/team/", Provider::Gitea).unwrap(),
        "https://git.example:443/team"
    );
    assert_eq!(
        canonical_server("https://api.github.com/", Provider::Github).unwrap(),
        "https://github.com"
    );
    for s in [
        "http://github.com",
        "https://user:password@github.com",
        "https://github.com.evil",
        "https://github.com:443",
        "https://github.com/path",
        "https://github.com?token=secret",
    ] {
        assert!(canonical_server(s, Provider::Github).is_err(), "{s}");
    }
    let mut c = Connection {
        server: "https://github.com".into(),
        repository: "owner/repo".into(),
        branch: "main".into(),
        ..Connection::default()
    };
    let gitea = c.source_key();
    c.provider = "github".into();
    assert_ne!(gitea, c.source_key());
}
#[test]
fn branches_are_paginated_until_empty_and_endpoints_are_distinct() {
    let m = Mock::new(vec![
        get("branches", json!([{"name":"main"}])),
        get("branches", json!([{"name":"dev"}])),
        get("branches", json!([])),
        get(
            "git/refs/heads/main",
            json!({"ref":"refs/heads/main","object":{"sha":id(1)}}),
        ),
    ]);
    let a = gitea(m.clone());
    assert_eq!(
        a.branches(&Cancellation::default()).unwrap(),
        ["main", "dev"]
    );
    assert_eq!(a.head("main", &Cancellation::default()).unwrap(), id(1));
    m.done();
    let m = Mock::new(vec![get(
        "git/ref/heads/main",
        json!({"ref":"refs/heads/main","object":{"type":"commit","sha":id(1)}}),
    )]);
    assert_eq!(
        github(m.clone())
            .head("main", &Cancellation::default())
            .unwrap(),
        id(1)
    );
    m.done();
}
#[test]
fn trees_validate_paging_totals_and_github_never_requests_recursive() {
    let m = Mock::new(vec![
        get(
            &format!("git/trees/{}", id(1)),
            json!({"sha":id(1),"page":1,"total_count":2,"truncated":true,"tree":[entry("A",2,"tree","040000")]}),
        ),
        get(
            &format!("git/trees/{}", id(1)),
            json!({"sha":id(1),"page":2,"total_count":2,"truncated":false,"tree":[entry("B",3,"blob","100644")]}),
        ),
    ]);
    assert_eq!(
        gitea(m.clone())
            .tree(&id(1), &Cancellation::default())
            .unwrap()
            .len(),
        2
    );
    m.done();
    let m = Mock::new(vec![exact(
        get(&format!("git/trees/{}", id(1)), tree(1, json!([]))),
        &[],
    )]);
    assert!(
        github(m.clone())
            .tree(&id(1), &Cancellation::default())
            .unwrap()
            .is_empty()
    );
    m.done();
    let m = Mock::new(vec![get(
        &format!("git/trees/{}", id(1)),
        json!({"sha":id(1),"truncated":true,"tree":[]}),
    )]);
    assert!(github(m).tree(&id(1), &Cancellation::default()).is_err());
}
#[test]
fn blob_hash_size_and_lfs_are_enforced() {
    let a = blob(b"data");
    verify_blob(b"data", &a.oid, 4).unwrap();
    assert!(verify_blob(b"Data", &a.oid, 4).is_err());
    assert!(verify_blob(b"data", &a.oid, 3).is_err());
    let bytes = b"version https://git-lfs.github.com/spec/v1\n";
    let a = blob(bytes);
    assert!(verify_blob(bytes, &a.oid, a.size).is_err());
}
#[test]
fn path_segments_encode_space_as_percent20_and_literal_plus_separately() {
    assert_eq!(
        quote("folder with space/A+B.xml"),
        "folder%20with%20space/A%2BB.xml"
    );
}
#[test]
fn gitea_batch_is_get_body_query_and_only_missing_rows_fall_back() {
    let a = blob(b"<A/>");
    let b = blob(b"<B/>");
    let m = Mock::new(vec![
        get(
            "file-contents",
            json!([{"path":"config/A.xml","encoding":"base64","content":STANDARD.encode(b"<A/>")}]),
        ),
        Step {
            raw: Some(b"<B/>".to_vec()),
            ..get("raw/config/B.xml", Value::Null)
        },
    ]);
    let api = gitea(m.clone());
    let result = api
        .files(
            &id(1),
            &[("config/A.xml".into(), a), ("config/B.xml".into(), b)],
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(result.len(), 2);
    let calls = m.calls.lock().unwrap();
    let query = calls[0].1.iter().cloned().collect::<BTreeMap<_, _>>();
    assert_eq!(
        serde_json::from_str::<Value>(&query["body"]).unwrap(),
        json!({"files":["config/A.xml","config/B.xml"]})
    );
    drop(calls);
    m.done();
    let m = Mock::new(vec![Step {
        result: Err("HTTP 404"),
        ..get("file-contents", Value::Null)
    }]);
    assert!(
        gitea(m.clone())
            .files(
                &id(1),
                &[("config/A.xml".into(), blob(b"x"))],
                &Cancellation::default()
            )
            .is_err()
    );
    m.done();
}
#[test]
fn github_graphql_binary_fallback_and_partial_errors() {
    let a = blob(b"abc");
    let m = Mock::new(vec![
        Step {
            body: true,
            ..get(
                "/graphql",
                json!({"data":{"repository":{"b0":{"__typename":"Blob","oid":a.oid,"byteSize":3,"isBinary":true,"isTruncated":false,"text":null}}}}),
            )
        },
        Step {
            raw: Some(b"abc".to_vec()),
            ..get(&format!("git/blobs/{}", a.oid), Value::Null)
        },
    ]);
    let result = github(m.clone())
        .files(
            &id(1),
            &[("config/A.xml".into(), a)],
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(result["config/A.xml"], b"abc");
    m.done();
    let a = blob(b"abc");
    let m = Mock::new(vec![Step {
        body: true,
        ..get(
            "/graphql",
            json!({"errors":[{"message":"private details"}],"data":{"repository":{"b0":null}}}),
        )
    }]);
    let error = github(m.clone())
        .files(
            &id(1),
            &[("config/A.xml".into(), a)],
            &Cancellation::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(!error.contains("private details"));
    m.done();
}
fn commit(n: u32, tree: u32, parents: &[u32]) -> Value {
    json!({"sha":id(n),"tree":{"sha":id(tree)},"parents":parents.iter().map(|&p|json!({"sha":id(p)})).collect::<Vec<_>>()})
}
#[test]
fn github_diff_uses_first_parent_and_includes_rename_delete_add_and_mode_change() {
    let m = Mock::new(vec![
        get(&format!("git/commits/{}", id(1)), commit(1, 10, &[2, 99])),
        get(&format!("git/commits/{}", id(2)), commit(2, 20, &[])),
        get(
            &format!("git/trees/{}", id(20)),
            tree(20, json!([entry("config", 21, "tree", "040000")])),
        ),
        get(
            &format!("git/trees/{}", id(10)),
            tree(10, json!([entry("config", 11, "tree", "040000")])),
        ),
        get(
            &format!("git/trees/{}", id(21)),
            tree(
                21,
                json!([
                    entry("deleted.xml", 30, "blob", "100644"),
                    entry("old.xml", 31, "blob", "100644"),
                    entry("mode.xml", 32, "blob", "100644")
                ]),
            ),
        ),
        get(
            &format!("git/trees/{}", id(11)),
            tree(
                11,
                json!([
                    entry("added.xml", 33, "blob", "100644"),
                    entry("new.xml", 31, "blob", "100644"),
                    entry("mode.xml", 32, "blob", "100755")
                ]),
            ),
        ),
    ]);
    assert_eq!(
        github(m.clone())
            .changed(&id(1), "config", &Cancellation::default())
            .unwrap(),
        [
            "config/added.xml",
            "config/deleted.xml",
            "config/mode.xml",
            "config/new.xml",
            "config/old.xml"
        ]
    );
    m.done();
}
#[test]
fn github_initial_commit_and_large_tree_avoid_commit_file_list() {
    let rows = (0..3501)
        .map(|n| entry(&format!("{n}.xml"), 100 + n, "blob", "100644"))
        .collect::<Vec<_>>();
    let m = Mock::new(vec![
        get(&format!("git/commits/{}", id(1)), commit(1, 10, &[])),
        get(
            &format!("git/trees/{}", id(10)),
            tree(10, json!([entry("config", 11, "tree", "040000")])),
        ),
        get(&format!("git/trees/{}", id(11)), tree(11, json!(rows))),
    ]);
    assert_eq!(
        github(m.clone())
            .changed(&id(1), "config", &Cancellation::default())
            .unwrap()
            .len(),
        3501
    );
    m.done();
}
#[test]
fn history_propagates_dates_reads_short_pages_and_honours_cancel_before_http() {
    let row = |n| json!({"sha":id(n),"commit":{"message":"ERP-123","committer":{"date":"2020-01-01T00:00:00Z"}}});
    let m = Mock::new(vec![
        get("commits", json!([row(1)])),
        get("commits", json!([row(2)])),
        get("commits", json!([])),
    ]);
    assert_eq!(
        github(m.clone())
            .commits(
                &id(9),
                Some("2026-01-01T00:00:00Z"),
                "2026-02-01T00:00:00Z",
                &Cancellation::default(),
                &|_| {}
            )
            .unwrap()
            .len(),
        2
    );
    let calls = m.calls.lock().unwrap();
    for (_, q, _) in calls.iter() {
        let q = q.iter().cloned().collect::<BTreeMap<_, _>>();
        assert_eq!(q["since"], "2026-01-01T00:00:00Z");
        assert_eq!(q["until"], "2026-02-01T00:00:00Z");
    }
    drop(calls);
    m.done();
    let m = Mock::new(vec![]);
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(
        gitea(m.clone())
            .commits(&id(9), None, "now", &cancel, &|_| {})
            .is_err()
    );
    m.done();
}
