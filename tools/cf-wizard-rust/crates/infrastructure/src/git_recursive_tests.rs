use super::*;

// Independent Git object vectors: tree headers and explicitly ordered byte
// records were hashed outside the Rust verifier. App.check sorts before App/.
const SHA1: (&str, &str, &str) = (
    "29f474d4b43bcc98849e653f38e8bd9727346dae",
    "5f86e710869c3fcbd63037387b56c12574738b28",
    "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
);
const SHA256: (&str, &str, &str) = (
    "8f68d7c4b5c30d3d66995a3b228754338b2a1dea4f7a8769f2b3be0f3c8df7f5",
    "573a9b96ba7363c65f9f4a6f083b2dd206ead0117e1df668aca6c43e9118db62",
    "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
);

fn row(path: &str, mode: &str, oid: &str) -> RecursiveRow {
    let kind = if mode == "040000" {
        EntryKind::Tree
    } else {
        EntryKind::Blob
    };
    RecursiveRow {
        path: path.into(),
        entry: Entry {
            name: path.rsplit('/').next().unwrap().into(),
            oid: oid.into(),
            mode: mode.into(),
            size: if kind == EntryKind::Tree { 0 } else { 42 },
            kind,
        },
    }
}

fn fixture((root, child, empty): (&str, &str, &str)) -> Vec<RecursiveRow> {
    let width = root.len() / 2;
    vec![
        row("Alias/x", "100755", &"11".repeat(width)),
        row("App/x", "100755", &"11".repeat(width)),
        row("Я.xml", "100644", &"44".repeat(width)),
        row("Alias/Sub", "040000", empty),
        row("App/Sub", "040000", empty),
        row("Пусто", "040000", empty),
        row("App0", "100644", &"22".repeat(width)),
        row("App", "040000", child),
        row("App.check", "100755", &"33".repeat(width)),
        row("Alias", "040000", child),
    ]
}

fn assert_incomplete(root: &str, rows: Vec<RecursiveRow>, prefix: usize, why: RecursiveFallback) {
    assert!(matches!(
        validate(root, rows, prefix, &Cancellation::default()).unwrap(),
        RecursiveListing::ProofIncomplete(reason) if reason == why
    ));
}

#[test]
fn proves_independent_sha1_and_sha256_nested_trees_aliases_and_empty_directories() {
    for (root, child, empty) in [SHA1, SHA256] {
        let mut rows = fixture((root, child, empty));
        // Aliases have identical canonical structure even when an untrusted size differs.
        rows[0].entry.size = 1;
        let cancel = Cancellation::default();
        let RecursiveListing::Complete(trees) = validate(root, rows, 3, &cancel).unwrap() else {
            panic!("independent tree vector did not validate");
        };
        assert_eq!(trees.len(), 3);
        assert_eq!(trees[root].len(), 6);
        assert_eq!(trees[root]["Alias"].oid, child);
        assert_eq!(trees[root]["App"].oid, child);
        assert_eq!(trees[root]["Я.xml"].mode, "100644");
        assert_eq!(trees[child]["x"].mode, "100755");
        assert!(trees[empty].is_empty());
        assert!(matches!(
            validate(empty, Vec::new(), 128, &cancel).unwrap(),
            RecursiveListing::Complete(trees) if trees.len() == 1 && trees[empty].is_empty()
        ));
    }
}

#[test]
fn no_proof_is_returned_for_a_valid_root_with_missing_or_corrupt_descendants() {
    let (root, _, _) = SHA1;
    let mutations: [fn(&mut Vec<RecursiveRow>); 6] = [
        |rows: &mut Vec<RecursiveRow>| {
            rows.remove(0);
        },
        |rows: &mut Vec<RecursiveRow>| {
            rows[0].entry.oid = "22".repeat(20);
        },
        |rows: &mut Vec<RecursiveRow>| {
            rows[0].entry.oid = "11".repeat(32);
        },
        |rows: &mut Vec<RecursiveRow>| {
            rows[0].entry.mode = "100644".into();
        },
        |rows: &mut Vec<RecursiveRow>| {
            rows[0].entry.mode = "120000".into();
        },
        |rows: &mut Vec<RecursiveRow>| {
            rows[0].entry.name = "other".into();
        },
    ];
    for mutate in mutations {
        let mut rows = fixture(SHA1);
        mutate(&mut rows);
        assert_incomplete(root, rows, 0, RecursiveFallback::InvalidTree);
    }
}

#[test]
fn rejects_missing_or_blob_parents_duplicates_and_windows_aliases() {
    let (root, _, empty) = SHA1;
    for (path, mode) in [
        ("Absent/x", "100644"),
        ("App0/x", "100644"),
        ("Alias/x", "100755"),
        ("App", "040000"),
        ("я.xml", "100644"),
    ] {
        let mut rows = fixture(SHA1);
        rows.push(row(path, mode, empty));
        assert_incomplete(root, rows, 0, RecursiveFallback::InvalidTree);
    }
    for path in [
        "/App/x", "App/x/", "App//x", "App/../x", "App\\x", "App/CON", "App/x.",
    ] {
        let mut rows = fixture(SHA1);
        rows.push(row(path, "100644", empty));
        assert_incomplete(root, rows, 0, RecursiveFallback::InvalidPath);
    }
}

#[test]
fn limits_include_the_prefix_and_are_inclusive() {
    let (root, _, empty) = SHA1;
    let cancel = Cancellation::default();
    assert!(matches!(
        validate(root, fixture(SHA1), MAX_DEPTH - 2, &cancel).unwrap(),
        RecursiveListing::Complete(_)
    ));
    assert_incomplete(
        root,
        fixture(SHA1),
        MAX_DEPTH - 1,
        RecursiveFallback::DepthLimit,
    );
    assert_incomplete(
        root,
        Vec::new(),
        MAX_DEPTH + 1,
        RecursiveFallback::DepthLimit,
    );
    let path = "Ю".repeat(MAX_PATH_BYTES / "Ю".len());
    assert_eq!(path.len(), MAX_PATH_BYTES);
    // A row on the path boundary reaches the canonical check.
    assert_incomplete(
        root,
        vec![row(&path, "100644", empty)],
        0,
        RecursiveFallback::InvalidTree,
    );
    let longer = format!("{path}x");
    assert_incomplete(
        root,
        vec![row(&longer, "100644", empty)],
        0,
        RecursiveFallback::PathLimit,
    );
    let many = (0..=MAX_ROWS)
        .map(|index| row(&format!("file{index}"), "100644", empty))
        .collect();
    assert_incomplete(root, many, 0, RecursiveFallback::RowLimit);
}

#[test]
fn cancelled_validation_remains_a_failure_before_any_proof() {
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        validate(SHA1.0, fixture(SHA1), 0, &cancel),
        Err(Error::Cancelled)
    ));
}
