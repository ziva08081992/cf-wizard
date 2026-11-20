//! Gitea recursive listing by tree OID. A collected listing is not a proof.
use crate::{
    git::{
        Entry, EntryKind, Http, HttpFailure, RecursiveFallback, RecursiveListing, RecursiveRow, oid,
    },
    git_tree_cache::{TreeEntries, validate_tree},
    source::relative_path,
};
use cf_application::{Cancellation, profile};
use cf_domain::{Error, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "git_recursive_tests.rs"]
mod tests;

const PAGE_ROWS: usize = 1_000;
const MAX_PAGES: usize = 4;
const MAX_ROWS: usize = 4_000;
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_DEPTH: usize = 128;

#[derive(Deserialize)]
struct Page {
    sha: String,
    page: usize,
    total_count: usize,
    truncated: bool,
    tree: PageTree,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PageTree {
    Rows(Vec<WireRow>),
    Null,
}

#[derive(Deserialize)]
struct WireRow {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
    mode: String,
    size: Option<u64>,
}

impl WireRow {
    fn into_row(self) -> std::result::Result<RecursiveRow, RecursiveFallback> {
        let (kind, size) = match (self.kind.as_str(), self.mode.as_str()) {
            ("tree", "040000") => (EntryKind::Tree, 0),
            ("blob", "100644" | "100755") => (
                EntryKind::Blob,
                self.size.ok_or(RecursiveFallback::InvalidResponse)?,
            ),
            _ => return Err(RecursiveFallback::InvalidTree),
        };
        let name = self
            .path
            .rsplit('/')
            .next()
            .expect("a split has at least one part");
        let entry = Entry {
            name: name.to_owned(),
            oid: self.sha,
            size,
            kind,
            mode: self.mode,
        };
        Ok(RecursiveRow {
            path: self.path,
            entry,
        })
    }
}

pub(super) fn gitea_listing(
    http: &dyn Http,
    id: &str,
    cancel: &Cancellation,
) -> Result<RecursiveListing<Vec<RecursiveRow>>> {
    use RecursiveFallback::*;
    use RecursiveListing::{Complete, ProofIncomplete};

    cancel.check()?;
    let path = format!("git/trees/{}", oid(id, false)?);
    let mut rows = Vec::new();
    let mut paths = BTreeSet::new();
    let mut total = None;
    let mut body_bytes = 0;
    for page in 1..=MAX_PAGES {
        cancel.check()?;
        let mut request = profile::span(
            "provider.tree_recursive_page",
            "provider.tree_recursive",
            0,
            None,
        );
        let body = http.bytes_typed(
            &path,
            &[
                ("recursive".into(), "true".into()),
                ("page".into(), page.to_string()),
                ("per_page".into(), PAGE_ROWS.to_string()),
            ],
            None,
            MAX_PAGE_BYTES,
            false,
            cancel,
        );
        cancel.check()?;
        let body = match body {
            Ok(body) => {
                request.success();
                body
            }
            Err(HttpFailure::ResponseTooLarge) => return Ok(ProofIncomplete(BodyLimit)),
            Err(error) => return Err(error.into_domain_error()),
        };
        drop(request);
        if body.len() > MAX_PAGE_BYTES || MAX_BODY_BYTES - body_bytes < body.len() {
            return Ok(ProofIncomplete(BodyLimit));
        }
        body_bytes += body.len();
        let page_data: Page = match serde_json::from_slice(&body) {
            Ok(data) => data,
            Err(_) => {
                cancel.check()?;
                return Ok(ProofIncomplete(InvalidResponse));
            }
        };
        cancel.check()?;
        if page_data.sha != id
            || page_data.page != page
            || total.is_some_and(|total| total != page_data.total_count)
        {
            return Ok(ProofIncomplete(InconsistentPages));
        }
        let count = page_data.total_count;
        if count > MAX_ROWS {
            return Ok(ProofIncomplete(RowLimit));
        }
        total = Some(count);
        let page_rows = match page_data.tree {
            PageTree::Rows(rows) => rows,
            PageTree::Null if page == 1 && count == 0 && !page_data.truncated => {
                return Ok(Complete(rows));
            }
            PageTree::Null => return Ok(ProofIncomplete(InconsistentPages)),
        };
        if page_rows.len() > PAGE_ROWS
            || page_rows.len() > count - rows.len()
            || page_rows.is_empty() && (count != 0 || page_data.truncated)
        {
            return Ok(ProofIncomplete(InconsistentPages));
        }
        for row in page_rows {
            cancel.check()?;
            if row.path.len() > MAX_PATH_BYTES {
                return Ok(ProofIncomplete(PathLimit));
            }
            if !paths.insert(row.path.clone()) {
                return Ok(ProofIncomplete(InconsistentPages));
            }
            match row.into_row() {
                Ok(row) => rows.push(row),
                Err(reason) => return Ok(ProofIncomplete(reason)),
            }
        }
        if rows.len() == count {
            if page_data.truncated {
                profile::note(
                    "provider.tree_recursive_terminal_true_seen",
                    "provider.tree_recursive",
                    1,
                );
            }
            return Ok(Complete(rows));
        }
        if !page_data.truncated {
            return Ok(ProofIncomplete(InconsistentPages));
        }
    }
    Ok(ProofIncomplete(PageLimit))
}

pub(super) type VerifiedTrees = BTreeMap<String, TreeEntries>;

struct Directory {
    oid: String,
    children: Vec<Entry>,
}

fn same_tree(a: &TreeEntries, b: &TreeEntries) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((a_name, a), (b_name, b))| {
            a_name == b_name && a.kind == b.kind && a.oid == b.oid && a.mode == b.mode
        })
}

pub(super) fn validate(
    root_oid: &str,
    rows: Vec<RecursiveRow>,
    prefix_depth: usize,
    cancel: &Cancellation,
) -> Result<RecursiveListing<VerifiedTrees>> {
    use RecursiveFallback::*;
    use RecursiveListing::{Complete, ProofIncomplete};

    cancel.check()?;
    if rows.len() > MAX_ROWS {
        return Ok(ProofIncomplete(RowLimit));
    }
    if prefix_depth > MAX_DEPTH {
        return Ok(ProofIncomplete(DepthLimit));
    }
    let mut directories = BTreeMap::from([(
        String::new(),
        Directory {
            oid: root_oid.to_owned(),
            children: Vec::new(),
        },
    )]);
    for row in &rows {
        cancel.check()?;
        if row.path.len() > MAX_PATH_BYTES {
            return Ok(ProofIncomplete(PathLimit));
        }
        if relative_path(&row.path).ok() != Some(row.path.as_str()) {
            return Ok(ProofIncomplete(InvalidPath));
        }
        if row.path.split('/').count() > MAX_DEPTH - prefix_depth {
            return Ok(ProofIncomplete(DepthLimit));
        }
        let basename = row
            .path
            .rsplit('/')
            .next()
            .expect("a split has at least one part");
        if row.entry.name != basename || row.entry.oid.len() != root_oid.len() {
            return Ok(ProofIncomplete(InvalidTree));
        }
        if row.entry.kind == EntryKind::Tree
            && directories
                .insert(
                    row.path.clone(),
                    Directory {
                        oid: row.entry.oid.clone(),
                        children: Vec::new(),
                    },
                )
                .is_some()
        {
            return Ok(ProofIncomplete(InvalidTree));
        }
    }
    for row in rows {
        cancel.check()?;
        let parent = row.path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let Some(directory) = directories.get_mut(parent) else {
            return Ok(ProofIncomplete(InvalidTree));
        };
        directory.children.push(row.entry);
    }

    // Every parent sorts before paths prefixed by that parent and a slash.
    let mut verified = BTreeMap::new();
    for (_, directory) in directories.into_iter().rev() {
        cancel.check()?;
        let entries = match validate_tree(&directory.oid, directory.children, cancel) {
            Ok(entries) => entries,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => return Ok(ProofIncomplete(InvalidTree)),
        };
        match verified.entry(directory.oid) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entries);
            }
            std::collections::btree_map::Entry::Occupied(slot) => {
                // Size hints are deliberately outside the canonical structure.
                if !same_tree(slot.get(), &entries) {
                    return Ok(ProofIncomplete(InvalidTree));
                }
            }
        }
    }
    cancel.check()?;
    Ok(Complete(verified))
}
