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

fn recursive_page(page: usize, count: usize, truncated: bool, rows: Value) -> Value {
    json!({"sha":id(1),"page":page,"total_count":count,"truncated":truncated,"tree":rows})
}

fn recursive_get(page: usize, value: Value) -> Step {
    exact(
        get(&format!("git/trees/{}", id(1)), value),
        &[
            ("recursive", "true"),
            ("page", &page.to_string()),
            ("per_page", "1000"),
        ],
    )
}

#[test]
fn gitea_recursive_collects_clamped_pages_through_the_legacy_http_adapter() {
    let http = Mock::new(vec![
        recursive_get(
            1,
            recursive_page(
                1,
                3,
                true,
                json!([
                    entry("Sub/child.xml", 2, "blob", "100644"),
                    entry("a.xml", 3, "blob", "100755"),
                ]),
            ),
        ),
        // The historical Gitea implementation set truncated even on the final chunk.
        recursive_get(
            2,
            recursive_page(2, 3, true, json!([entry("Sub", 4, "tree", "040000"),])),
        ),
    ]);
    let provider = gitea(http.clone());
    let context = profile::Context::new("recursive-tree-test", true, None);
    let result = {
        let _scope = context.enter();
        provider
            .recursive_tree(&id(1), &Cancellation::default())
            .unwrap()
    };
    let RecursiveListing::Complete(rows) = result else {
        panic!("fully collected page sequence was rejected");
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].path, "Sub/child.xml");
    assert_eq!(rows[0].entry.name, "child.xml");
    assert_eq!(rows[2].entry.size, 0);
    assert!(provider.trees.lock().unwrap().is_empty());
    let report = context.report("ok").unwrap();
    assert_eq!(
        report.metrics["provider.tree_recursive_page"].totals.calls,
        2
    );
    assert_eq!(
        report.metrics["provider.tree_recursive_terminal_true_seen"]
            .totals
            .items,
        1
    );
    http.done();
}

#[test]
fn gitea_recursive_requires_explicit_empty_tree_and_stable_paging() {
    let cancel = Cancellation::default();
    for empty in [Value::Null, json!([])] {
        let http = Mock::new(vec![recursive_get(1, recursive_page(1, 0, false, empty))]);
        assert!(matches!(
            gitea(http.clone()).recursive_tree(&id(1), &cancel).unwrap(),
            RecursiveListing::Complete(rows) if rows.is_empty()
        ));
        http.done();
    }

    let mut missing_tree = recursive_page(1, 0, false, Value::Null);
    missing_tree.as_object_mut().unwrap().remove("tree");
    let mut wrong_sha = recursive_page(2, 2, false, json!([entry("b", 4, "blob", "100644")]));
    wrong_sha["sha"] = json!(id(2));
    let first = || {
        recursive_get(
            1,
            recursive_page(1, 2, true, json!([entry("a", 3, "blob", "100644"),])),
        )
    };
    for (steps, expected) in [
        (
            vec![recursive_get(1, missing_tree)],
            RecursiveFallback::InvalidResponse,
        ),
        (
            vec![recursive_get(1, recursive_page(1, 0, true, Value::Null))],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![recursive_get(
                1,
                recursive_page(1, 2, false, json!([entry("a", 3, "blob", "100644")])),
            )],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![recursive_get(
                1,
                recursive_page(1, 0, false, json!([entry("a", 3, "blob", "100644")])),
            )],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![
                first(),
                recursive_get(2, recursive_page(2, 2, false, json!([]))),
            ],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![
                first(),
                recursive_get(2, recursive_page(2, 2, false, Value::Null)),
            ],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![
                first(),
                recursive_get(
                    2,
                    recursive_page(2, 2, false, json!([entry("a", 3, "blob", "100644")])),
                ),
            ],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![
                first(),
                recursive_get(
                    2,
                    recursive_page(1, 2, false, json!([entry("b", 4, "blob", "100644")])),
                ),
            ],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![
                first(),
                recursive_get(
                    2,
                    recursive_page(2, 3, false, json!([entry("b", 4, "blob", "100644")])),
                ),
            ],
            RecursiveFallback::InconsistentPages,
        ),
        (
            vec![first(), recursive_get(2, wrong_sha)],
            RecursiveFallback::InconsistentPages,
        ),
    ] {
        let http = Mock::new(steps);
        assert!(matches!(
            gitea(http.clone()).recursive_tree(&id(1), &cancel).unwrap(),
            RecursiveListing::ProofIncomplete(reason) if reason == expected
        ));
        http.done();
    }
}

enum TypedReply {
    Body(Vec<u8>),
    Error(HttpFailure),
    Cancel,
}

struct RecursiveMock {
    total: usize,
    replies: Mutex<VecDeque<TypedReply>>,
}

impl Http for RecursiveMock {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        self.bytes_typed(path, query, body, limit, raw, cancel)
            .map_err(HttpFailure::into_domain_error)
    }

    fn bytes_typed(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> std::result::Result<Vec<u8>, HttpFailure> {
        cancel.check()?;
        assert_eq!(path, format!("git/trees/{}", id(1)));
        assert!(body.is_none() && !raw);
        assert_eq!(limit, 2 * 1024 * 1024);
        let mut replies = self.replies.lock().unwrap();
        let page = self.total - replies.len() + 1;
        assert_eq!(
            query
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("recursive", "true"),
                ("page", &page.to_string()),
                ("per_page", "1000")
            ])
        );
        match replies
            .pop_front()
            .expect("unexpected recursive retry or next page")
        {
            TypedReply::Body(bytes) => Ok(bytes),
            TypedReply::Error(error) => Err(error),
            TypedReply::Cancel => {
                cancel.cancel();
                Err(HttpFailure::ResponseTooLarge)
            }
        }
    }
}

fn typed_recursive(replies: Vec<TypedReply>) -> Result<RecursiveListing<Vec<RecursiveRow>>> {
    let http = Arc::new(RecursiveMock {
        total: replies.len(),
        replies: Mutex::new(replies.into()),
    });
    let provider = Gitea {
        http: http.clone(),
        trees: Mutex::new(BTreeMap::new()),
    };
    let result = provider.recursive_tree(&id(1), &Cancellation::default());
    assert!(http.replies.lock().unwrap().is_empty());
    assert!(provider.trees.lock().unwrap().is_empty());
    result
}

#[test]
fn gitea_recursive_bounds_stop_before_an_additional_request_and_accept_equality() {
    let row = |index| entry(&format!("file{index:04}"), 12, "blob", "100644");
    let page = |index, count, rows| {
        serde_json::to_vec(&recursive_page(index, count, true, Value::Array(rows))).unwrap()
    };
    let max_pages: Vec<_> = (1..=4)
        .map(|index| TypedReply::Body(page(index, 5, vec![row(index)])))
        .collect();
    assert!(matches!(
        typed_recursive(max_pages).unwrap(),
        RecursiveListing::ProofIncomplete(RecursiveFallback::PageLimit)
    ));
    assert!(matches!(
        typed_recursive(vec![TypedReply::Body(page(1, 4001, vec![row(0)]))]).unwrap(),
        RecursiveListing::ProofIncomplete(RecursiveFallback::RowLimit)
    ));
    for oversized in [
        TypedReply::Error(HttpFailure::ResponseTooLarge),
        TypedReply::Body(vec![b' '; 2 * 1024 * 1024 + 1]),
    ] {
        assert!(matches!(
            typed_recursive(vec![oversized]).unwrap(),
            RecursiveListing::ProofIncomplete(RecursiveFallback::BodyLimit)
        ));
    }
    let first_oversized_path = page(1, 1, vec![entry(&"Ю".repeat(2_049), 12, "blob", "100644")]);
    assert!(matches!(
        typed_recursive(vec![TypedReply::Body(first_oversized_path)]).unwrap(),
        RecursiveListing::ProofIncomplete(RecursiveFallback::PathLimit)
    ));
    let at_all_limits = (0..4)
        .map(|index| {
            let mut body = page(
                index + 1,
                4_000,
                (index * 1_000..(index + 1) * 1_000).map(row).collect(),
            );
            body.resize(2 * 1024 * 1024, b' ');
            TypedReply::Body(body)
        })
        .collect();
    assert!(matches!(
        typed_recursive(at_all_limits).unwrap(),
        RecursiveListing::Complete(rows) if rows.len() == 4_000
    ));
}

#[test]
fn gitea_recursive_http_status_transport_and_cancellation_are_fatal() {
    let first_page = || {
        TypedReply::Body(
            serde_json::to_vec(&recursive_page(
                1,
                2,
                true,
                json!([entry("a", 3, "blob", "100644")]),
            ))
            .unwrap(),
        )
    };
    for status in [302, 400, 401, 403, 404, 429, 500, 501, 503] {
        let result = typed_recursive(vec![
            first_page(),
            TypedReply::Error(HttpFailure::Status(status)),
        ]);
        assert!(
            matches!(result, Err(Error::Invalid(message)) if message == status_error(status).to_string())
        );
    }
    for message in ["transport", "timeout", "read"] {
        let result = typed_recursive(vec![
            first_page(),
            TypedReply::Error(HttpFailure::Fatal(Error::External(message.into()))),
        ]);
        assert!(matches!(result, Err(Error::External(actual)) if actual == message));
    }
    assert!(matches!(
        typed_recursive(vec![first_page(), TypedReply::Cancel]),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        github(Mock::new(Vec::new()))
            .recursive_tree(&id(1), &Cancellation::default())
            .unwrap(),
        RecursiveListing::Unsupported
    ));
}

fn object_tree(ordered_rows: &[(&str, &str, &str)]) -> String {
    let mut bytes = Vec::new();
    for (name, mode, oid) in ordered_rows {
        bytes.extend_from_slice(mode.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend(hex::decode(oid).unwrap());
    }
    let mut digest = sha1::Sha1::new();
    digest.update(format!("tree {}\0", bytes.len()));
    digest.update(bytes);
    hex::encode(digest.finalize())
}

fn wire_entry(name: &str, oid: &str, kind: &str, mode: &str, size: u64) -> Value {
    json!({"path":name,"sha":oid,"type":kind,"mode":mode,"size":size})
}

fn inventory_response(oid: &str, recursive: bool, count: usize, rows: Value) -> Step {
    let step = exact(
        get(
            &format!("git/trees/{oid}"),
            json!({
                "sha":oid,"page":1,"total_count":count,"truncated":false,"tree":rows
            }),
        ),
        &[("page", "1"), ("per_page", "1000")],
    );
    if !recursive {
        return step;
    }
    exact(
        step,
        &[("page", "1"), ("per_page", "1000"), ("recursive", "true")],
    )
}

fn raw_at(path: &str, revision: &str, bytes: &[u8]) -> Step {
    let mut step = exact(
        get(&format!("raw/{path}"), Value::Null),
        &[("ref", revision)],
    );
    step.raw = Some(bytes.to_vec());
    step
}

struct RecursiveInventory {
    revision: String,
    scope: String,
    catalogs: String,
    owner: String,
    ext: String,
    file: Entry,
}

impl RecursiveInventory {
    const CONFIG: &[u8] = b"<MetaDataObject><Configuration uuid='00000000-0000-4000-8000-000000000001'><Properties><Name>Fixture</Name></Properties><ChildObjects><Catalog>Owner</Catalog></ChildObjects></Configuration></MetaDataObject>";
    const DESCRIPTOR: &[u8] = b"descriptor";
    const PAYLOAD: &[u8] = b"authoritative bytes";
    const OWNER: &str = "Catalogs/Owner";
    const FILE: &str = "Catalogs/Owner/Ext/Payload.bin";

    fn new() -> Self {
        let file = blob(Self::PAYLOAD);
        let ext = object_tree(&[("Payload.bin", "100644", &file.oid)]);
        let owner = object_tree(&[("Ext", "40000", &ext)]);
        let catalogs = object_tree(&[
            ("Alias", "40000", &owner),
            ("Owner.xml", "100644", &blob(Self::DESCRIPTOR).oid),
            ("Owner", "40000", &owner),
        ]);
        let scope = object_tree(&[
            ("Catalogs", "40000", &catalogs),
            ("Configuration.xml", "100644", &blob(Self::CONFIG).oid),
        ]);
        Self {
            revision: id(9),
            scope,
            catalogs,
            owner,
            ext,
            file,
        }
    }

    fn bootstrap(&self, config_cold: bool) -> Vec<Step> {
        let mut steps = vec![
            get(
                "git/refs/heads/main",
                json!({
                    "ref":"refs/heads/main","object":{"sha":self.revision}
                }),
            ),
            inventory_response(
                &self.revision,
                false,
                1,
                json!([wire_entry("config", &self.scope, "tree", "040000", 0),]),
            ),
            inventory_response(
                &self.scope,
                false,
                2,
                json!([
                    wire_entry("Catalogs", &self.catalogs, "tree", "040000", 0),
                    wire_entry(
                        "Configuration.xml",
                        &blob(Self::CONFIG).oid,
                        "blob",
                        "100644",
                        Self::CONFIG.len() as u64
                    ),
                ]),
            ),
        ];
        if config_cold {
            steps.push(raw_at(
                "config/Configuration.xml",
                &self.revision,
                Self::CONFIG,
            ));
        }
        steps.push(inventory_response(
            &self.catalogs,
            false,
            3,
            json!([
                wire_entry("Alias", &self.owner, "tree", "040000", 0),
                wire_entry("Owner", &self.owner, "tree", "040000", 0),
                wire_entry(
                    "Owner.xml",
                    &blob(Self::DESCRIPTOR).oid,
                    "blob",
                    "100644",
                    Self::DESCRIPTOR.len() as u64
                ),
            ]),
        ));
        steps
    }

    fn direct_owner(&self) -> Step {
        inventory_response(
            &self.owner,
            false,
            1,
            json!([wire_entry("Ext", &self.ext, "tree", "040000", 0)]),
        )
    }

    fn ext_response(&self, recursive: bool) -> Step {
        inventory_response(
            &self.ext,
            recursive,
            1,
            json!([wire_entry(
                "Payload.bin",
                &self.file.oid,
                "blob",
                "100644",
                self.file.size
            )]),
        )
    }

    fn open(cache: &std::path::Path, http: Arc<dyn Http>) -> crate::git_source::GitSource {
        crate::git_source::GitSource::open(
            &Connection {
                server: "https://gitea.test".into(),
                repository: "fixture/project".into(),
                branch: "main".into(),
                directory: "config".into(),
                cache: cache.to_string_lossy().into_owned(),
                ..Connection::default()
            },
            Arc::new(Gitea {
                http,
                trees: Mutex::new(BTreeMap::new()),
            }),
            &Cancellation::default(),
        )
        .unwrap()
    }
}

#[test]
fn gitea_recursive_inventory_refreshes_wrong_live_size_hint_and_reopens_without_inventory_http() {
    use cf_application::MetadataSource;
    let f = RecursiveInventory::new();
    let cache = tempfile::tempdir().unwrap();
    let mut steps = f.bootstrap(true);
    let mut first_page = inventory_response(
        &f.owner,
        true,
        2,
        json!([wire_entry("Ext", &f.ext, "tree", "040000", 0)]),
    );
    first_page.result.as_mut().unwrap()["truncated"] = json!(true);
    let mut second_page = inventory_response(
        &f.owner,
        true,
        2,
        json!([wire_entry(
            "Ext/Payload.bin",
            &f.file.oid,
            "blob",
            "100644",
            u64::MAX
        ),]),
    );
    second_page.result.as_mut().unwrap()["page"] = json!(2);
    second_page = exact(
        second_page,
        &[("page", "2"), ("per_page", "1000"), ("recursive", "true")],
    );
    steps.extend([
        first_page,
        second_page,
        f.ext_response(false),
        raw_at(
            "config/Catalogs/Owner/Ext/Payload.bin",
            &f.revision,
            RecursiveInventory::PAYLOAD,
        ),
    ]);
    let http = Mock::new(steps);
    let source = RecursiveInventory::open(cache.path(), http.clone());
    let cancel = Cancellation::default();
    let expected = vec![RecursiveInventory::FILE.to_owned()];
    let paths = vec![RecursiveInventory::OWNER.to_owned()];
    let context = profile::Context::new("recursive-paged-inventory", true, None);
    let cold = {
        let _scope = context.enter();
        source
            .inventory_window(&paths, &cancel)
            .unwrap()
            .unwrap()
            .remove(0)
            .unwrap()
    };
    assert_eq!(cold, expected);
    let report = context.report("ok").unwrap();
    assert_eq!(report.metrics["provider.tree"].totals.calls, 1);
    assert_eq!(report.metrics["provider.tree_recursive"].totals.calls, 1);
    assert_eq!(
        report.metrics["provider.tree_recursive_page"].totals.calls,
        2
    );
    assert_eq!(report.maximum_combined_in_flight, 1);
    assert_eq!(report.final_in_flight, 0);
    assert_eq!(
        source.read(RecursiveInventory::FILE, &cancel).unwrap(),
        RecursiveInventory::PAYLOAD
    );
    http.done();
    drop(source);

    let restart_http = Mock::new(f.bootstrap(false));
    let restart = RecursiveInventory::open(cache.path(), restart_http.clone());
    assert_eq!(
        restart
            .inventory_window(&paths, &cancel)
            .unwrap()
            .unwrap()
            .remove(0)
            .unwrap(),
        expected
    );
    assert_eq!(
        restart.read(RecursiveInventory::FILE, &cancel).unwrap(),
        RecursiveInventory::PAYLOAD
    );
    restart_http.done();
}

#[test]
fn gitea_recursive_inventory_disables_protocol_rejections_but_keeps_other_oids_eligible_after_bounds()
 {
    use cf_application::MetadataSource;
    let f = RecursiveInventory::new();
    let paths = vec![RecursiveInventory::OWNER.to_owned()];
    for bounded in [false, true] {
        let cache = tempfile::tempdir().unwrap();
        let mut steps = f.bootstrap(true);
        if bounded {
            steps.push(inventory_response(&f.owner, true, 4001, json!([])));
        } else {
            // The immediate child is valid, but the descendant was omitted.
            steps.push(inventory_response(
                &f.owner,
                true,
                1,
                json!([wire_entry("Ext", &f.ext, "tree", "040000", 0)]),
            ));
        }
        steps.extend([f.direct_owner(), f.ext_response(bounded)]);
        let http = Mock::new(steps);
        let source = RecursiveInventory::open(cache.path(), http.clone());
        let result = source
            .inventory_window(&paths, &Cancellation::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            result[0].as_ref().unwrap(),
            &[RecursiveInventory::FILE.to_owned()]
        );
        http.done();
    }
}

#[test]
fn gitea_recursive_inventory_propagates_transport_failure_without_direct_retry() {
    use cf_application::MetadataSource;
    let f = RecursiveInventory::new();
    let cache = tempfile::tempdir().unwrap();
    let mut steps = f.bootstrap(true);
    let mut failed = inventory_response(&f.owner, true, 0, Value::Null);
    failed.result = Err("network failure");
    steps.push(failed);
    let http = Mock::new(steps);
    let source = RecursiveInventory::open(cache.path(), http.clone());
    let result = source
        .inventory_window(
            &[RecursiveInventory::OWNER.into()],
            &Cancellation::default(),
        )
        .unwrap()
        .unwrap();
    assert!(matches!(&result[0], Err(Error::Invalid(message)) if message == "network failure"));
    http.done();
}

#[test]
fn gitea_recursive_inventory_aliases_share_the_root_flight_and_resume_at_first_missing_child() {
    use cf_application::MetadataSource;
    let fixture = RecursiveInventory::new();
    let cache = tempfile::tempdir().unwrap();
    let mut steps = fixture.bootstrap(true);
    steps.push(inventory_response(
        &fixture.owner,
        true,
        2,
        json!([
            wire_entry("Ext", &fixture.ext, "tree", "040000", 0),
            wire_entry(
                "Ext/Payload.bin",
                &fixture.file.oid,
                "blob",
                "100644",
                fixture.file.size
            ),
        ]),
    ));
    let http = Mock::new(steps);
    let first = RecursiveInventory::open(cache.path(), http.clone());
    let paths = vec![RecursiveInventory::OWNER.into(), "Catalogs/Alias".into()];
    let expected = [
        vec![RecursiveInventory::FILE.to_owned()],
        vec!["Catalogs/Alias/Ext/Payload.bin".to_owned()],
    ];
    let inventory = |source: &crate::git_source::GitSource| {
        source
            .inventory_window(&paths, &Cancellation::default())
            .unwrap()
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    };
    assert_eq!(inventory(&first), expected);
    http.done();

    let tree_cache =
        crate::git_tree_cache::TreeCache::new(cache.path(), first.connection(), first.revision())
            .unwrap();
    drop(first);
    let sqlite = rusqlite::Connection::open(tree_cache.sqlite_path_for_test()).unwrap();
    assert_eq!(
        sqlite
            .execute("DELETE FROM trees WHERE oid = ?1", [&fixture.ext])
            .unwrap(),
        1
    );
    drop(sqlite);
    let mut steps = fixture.bootstrap(false);
    steps.push(fixture.ext_response(true));
    let http = Mock::new(steps);
    let restarted = RecursiveInventory::open(cache.path(), http.clone());
    assert_eq!(inventory(&restarted), expected);
    http.done();
}

struct ConcurrentRecursiveHttp {
    bootstrap: Arc<Mock>,
    responses: BTreeMap<String, Value>,
    arrivals: Mutex<usize>,
    all_arrived: std::sync::Condvar,
    active: std::sync::atomic::AtomicUsize,
    maximum: std::sync::atomic::AtomicUsize,
}

impl Http for ConcurrentRecursiveHttp {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        if !query
            .iter()
            .any(|(key, value)| key == "recursive" && value == "true")
        {
            return self.bootstrap.bytes(path, query, body, limit, raw, cancel);
        }
        use std::sync::atomic::Ordering::SeqCst;
        cancel.check()?;
        let response = self
            .responses
            .get(path)
            .expect("unexpected recursive target");
        let active = self.active.fetch_add(1, SeqCst) + 1;
        self.maximum.fetch_max(active, SeqCst);
        let mut arrived = self.arrivals.lock().unwrap();
        *arrived += 1;
        if *arrived == self.responses.len() {
            self.all_arrived.notify_all();
        }
        let (arrived, _) = self
            .all_arrived
            .wait_timeout_while(arrived, Duration::from_secs(3), |arrived| {
                *arrived < self.responses.len()
            })
            .unwrap();
        self.active.fetch_sub(1, SeqCst);
        assert_eq!(*arrived, self.responses.len());
        serde_json::to_vec(response).map_err(crate::external)
    }
}

#[test]
fn gitea_recursive_inventory_profile_matches_four_overlapping_http_requests() {
    use cf_application::MetadataSource;
    use std::sync::atomic::Ordering::SeqCst;
    let mut fixture = RecursiveInventory::new();
    let cache = tempfile::tempdir().unwrap();
    let jobs: Vec<_> = (1..=4)
        .map(|index| {
            let file = id(index + 100);
            (
                format!("Job{index}"),
                object_tree(&[("Data.bin", "100644", &file)]),
                file,
            )
        })
        .collect();
    let owner = blob(RecursiveInventory::DESCRIPTOR);
    let mut catalog_rows: Vec<_> = jobs
        .iter()
        .map(|(name, oid, _)| (name.as_str(), "40000", oid.as_str()))
        .collect();
    catalog_rows.push(("Owner.xml", "100644", &owner.oid));
    fixture.catalogs = object_tree(&catalog_rows);
    fixture.scope = object_tree(&[
        ("Catalogs", "40000", &fixture.catalogs),
        (
            "Configuration.xml",
            "100644",
            &blob(RecursiveInventory::CONFIG).oid,
        ),
    ]);
    let mut steps = fixture.bootstrap(true);
    let mut rows: Vec<_> = jobs
        .iter()
        .map(|(name, oid, _)| wire_entry(name, oid, "tree", "040000", 0))
        .collect();
    rows.push(wire_entry(
        "Owner.xml",
        &owner.oid,
        "blob",
        "100644",
        owner.size,
    ));
    *steps.last_mut().unwrap() =
        inventory_response(&fixture.catalogs, false, rows.len(), Value::Array(rows));
    let responses = jobs
        .iter()
        .map(|(_, oid, file)| {
            (
                format!("git/trees/{oid}"),
                json!({"sha":oid,"page":1,"total_count":1,"truncated":false,
            "tree":[wire_entry("Data.bin", file, "blob", "100644", 1)]}),
            )
        })
        .collect();
    let http = Arc::new(ConcurrentRecursiveHttp {
        bootstrap: Mock::new(steps),
        responses,
        arrivals: Mutex::new(0),
        all_arrived: std::sync::Condvar::new(),
        active: Default::default(),
        maximum: Default::default(),
    });
    let open = profile::Context::new("recursive-profile-bootstrap", true, None);
    let mut source = {
        let _scope = open.enter();
        RecursiveInventory::open(cache.path(), http.clone())
    };
    source.set_test_source_counters(profile::SourceCounters::new(true));
    let context = profile::Context::new(
        "recursive-overlapping-inventory",
        true,
        source.profile_counters(),
    );
    let paths: Vec<_> = jobs
        .iter()
        .map(|(name, _, _)| format!("Catalogs/{name}"))
        .collect();
    {
        let _scope = context.enter();
        let files = source
            .inventory_window(&paths, &Cancellation::default())
            .unwrap()
            .unwrap();
        for (owner, rows) in paths.iter().zip(files) {
            assert_eq!(rows.unwrap(), [format!("{owner}/Data.bin")]);
        }
    }
    http.bootstrap.done();
    let report = context.report("ok").unwrap();
    assert_eq!(report.source_delta["provider.tree"].calls, 4);
    assert_eq!(report.metrics["provider.tree"].totals.calls, 4);
    assert_eq!(report.metrics["provider.tree_recursive"].totals.calls, 4);
    assert_eq!(
        report.metrics["provider.tree_recursive_page"].totals.calls,
        4
    );
    assert_eq!(
        report.maximum_combined_in_flight,
        http.maximum.load(SeqCst) as u64
    );
    assert_eq!(report.maximum_combined_in_flight, 4);
    assert_eq!(report.final_in_flight, 0);
    assert_eq!(http.active.load(SeqCst), 0);
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
