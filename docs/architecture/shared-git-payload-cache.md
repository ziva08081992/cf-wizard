# Shared Git payload cache

The payload store is `<cache>/<repository-key>/blobs/<Git-blob-OID>`.
It contains the original bytes of the Git blob, without JSON or SQLite wrapping.
The repository key preserves the existing server/repository boundary. Different
branches and pinned revisions reuse a verified blob with the same OID.

A hit is checked against the OID and the applicable size bound, read once and
returned without writing. A new provider response is verified before the existing
atomic publication routine writes it into this store. Concurrent publishers may
produce the same verified bytes; a temporary file is never used as a cache hit.

Old `<cache>/<repository-key>/<source-key>/<revision>/...` payload copies remain
readable for the current pinned revision. A verified legacy-only hit is atomically
promoted into the shared store; the legacy file is left intact. Invalid shared
bytes can be repaired from valid legacy bytes or a verified provider response.
New reads and prefetches create no revision payload copies. No automatic cleanup
or migration scan runs, so old folders continue to occupy disk space until a
separately authorized cleanup.

The former materialized root remains a virtual identity/path boundary even when
its directory does not exist. Source identity, pinned revision, logical paths,
source stamps and index compatibility stay unchanged. Listing the current pinned
tree decides whether a path exists: retaining a blob from an older revision
cannot resurrect a deleted path or substitute another revision's changed file.
NoLinks, cancellation, read limits and blob verification remain enforced.
The cancellation token is checked immediately before atomic persist, after the
temporary file is written and synchronized. Cancellation before that boundary
keeps the previous file and removes the temporary file. A successful persist
completes publication; cancellation does not retroactively turn it into failure.

Rollback to diagnostics.2 can read the same shared blob store. That version may
create revision copies again under its own previous write policy. Neither version
requires destructive cache conversion or a SQLite reindex for this payload change.

SQLite SourceFacts is a separate cache: its `objects.facts` column holds serialized
JSON compressed with zlib in a BLOB. This change does not change that schema or
its reader semantics. Tree listings use the existing versioned JSON tree cache;
recursive API results pass complete canonical Git-tree verification before use.
See [recursive inventory](gitea-recursive-inventory.md) for bounds and fallbacks.

The profiling counters `cache.payload_shared_hit`, `cache.payload_legacy_hit`,
`cache.payload_legacy_promoted`, `cache.payload_published` and
`cache.payload_publish_race_reused` distinguish the paths. An atomic publication
failure is accepted only if a subsequent NoLinks and OID/size check proves that
a concurrent publisher has already stored the expected bytes; otherwise it is
reported.
Their counts describe logical payload events; inclusive timings from concurrent
workers must not be added and presented as elapsed time.
