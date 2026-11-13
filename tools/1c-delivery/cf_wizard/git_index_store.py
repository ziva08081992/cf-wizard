"""Atomic, OID-based updates of the persistent Git metadata index."""
from collections import Counter
from contextlib import closing, contextmanager
from hashlib import sha256
import os
from pathlib import Path
import sqlite3
from tempfile import TemporaryDirectory

try:
    from .dependency_index import APPLICATION_ID, SCHEMA, SCHEMA_VERSION, _facts, _header, _readonly, _records
    from .git_index_inputs import MANIFEST_VERSION, GitIndexEntry, GitIndexInputs
except ImportError:
    from dependency_index import APPLICATION_ID, SCHEMA, SCHEMA_VERSION, _facts, _header, _readonly, _records
    from git_index_inputs import MANIFEST_VERSION, GitIndexEntry, GitIndexInputs


INPUT_SCHEMA = """CREATE TABLE IF NOT EXISTS git_inputs (
    path TEXT PRIMARY KEY, oid TEXT NOT NULL, size INTEGER NOT NULL,
    owner TEXT NOT NULL, roles TEXT NOT NULL
) WITHOUT ROWID"""


def read_manifest(db, header):
    if header.get("git_manifest") != MANIFEST_VERSION:
        raise ValueError("Обновите формат индекса Git по кнопке.")
    rows = db.execute("SELECT path,oid,size,owner,roles FROM git_inputs ORDER BY path")
    entries = tuple(GitIndexEntry(path, oid, size, owner, tuple(roles.split(",")))
                    for path, oid, size, owner, roles in rows)
    inputs = GitIndexInputs(header["git_source"], header["indexed_revision"], entries)
    if inputs.fingerprint != header.get("git_fingerprint"):
        raise ValueError("Манифест индекса Git повреждён. Обновите индекс.")
    return inputs


@contextmanager
def _writer(path):
    """OS locks are released even if the updating process crashes."""
    with open(path.with_suffix(path.suffix + ".lock"), "a+b") as stream:
        if stream.tell() == 0:
            stream.write(b"\0")
            stream.flush()
        stream.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise ValueError("Индекс обновляется в другом окне. Повторите позже.") from None
        try:
            yield
        finally:
            stream.seek(0)
            if os.name == "nt":
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(stream, fcntl.LOCK_UN)


def _stamp(path):
    try:
        info = path.stat()
        return info.st_size, info.st_mtime_ns, info.st_ctime_ns
    except FileNotFoundError:
        return None


def _changes(db, inputs, old_inputs, git, helper):
    old = {entry.path: entry for entry in old_inputs.entries} if old_inputs else {}
    owners = {entry.owner for entry in inputs.entries if "descriptor" in entry.roles}
    known = dict(db.execute("SELECT name,content_hash FROM objects"))
    removed = known.keys() - owners
    db.executemany("DELETE FROM objects WHERE name=?", ((key,) for key in removed))
    documents = {Path(entry.path).name for entry in inputs.entries if "registrar" in entry.roles}
    old_documents = {name for name, in db.execute("SELECT file FROM documents")}
    db.executemany("DELETE FROM documents WHERE file=?", ((name,) for name in old_documents - documents))
    counts = Counter(removed=len(removed), inputs=len(inputs.entries))
    current = {entry.path: entry for entry in inputs.entries}
    counts["input_added"] = len(current.keys() - old.keys())
    counts["input_removed"] = len(old.keys() - current.keys())
    counts["input_changed"] = sum(old[path] != current[path] for path in old.keys() & current.keys())
    git.metadata({}, paths={entry.path for entry in inputs.entries if old.get(entry.path) != entry})
    for entry in inputs.entries:
        if old.get(entry.path) == entry:
            if "descriptor" in entry.roles:
                counts["unchanged"] += 1
            counts["reused_inputs"] += 1
            continue
        data = git.ensure(entry.path).read_bytes()
        git.verify(entry.path, data)
        counts["xml_reads"] += 1
        counts["xml_hashes"] += 1
        if "descriptor" in entry.roles:
            digest = sha256(data).digest()
            if known.get(entry.owner) == digest:
                counts["unchanged"] += 1
            else:
                db.execute("INSERT OR REPLACE INTO objects VALUES (?,?,?)",
                           (entry.owner, digest, _facts(data, helper)))
                counts["facts"] += 1
                counts["changed" if entry.owner in known else "added"] += 1
        if "registrar" in entry.roles:
            name = Path(entry.path).name
            fragment = _records(data)
            db.execute("DELETE FROM documents WHERE file=?", (name,))
            db.execute("INSERT INTO documents VALUES (?,?)", (name, sha256(fragment).digest()))
            refs = helper["scan"](fragment.decode("utf-8", "surrogateescape"))
            db.executemany("INSERT INTO registrar_links VALUES (?,?,?)",
                           ((name, i, helper["key_of"](ref.kind, ref.name)) for i, ref in enumerate(refs)))
    return counts


def update(path, source, descriptors, helper, helper_sha, source_check):
    git, inputs = source.git, source.git.index_inputs
    if {entry.owner: entry.path for entry in inputs.entries if "descriptor" in entry.roles} != descriptors:
        raise ValueError("Состав XML не соответствует закреплённому манифесту Git.")
    with _writer(path), TemporaryDirectory(prefix=".cf-git-index-", dir=path.parent) as folder:
        before = _stamp(path)
        draft = Path(folder) / "index.sqlite3"
        old_inputs = None
        seeds = [(path, None)]
        if git.seed_root is not None and git.seed_index is not None:
            seeds.append((git.seed_index, os.path.normcase(str(git.seed_root))))
        for candidate, seed_identity in seeds:
            if not candidate.is_file():
                continue
            try:
                with closing(_readonly(candidate)) as db:
                    db.execute("PRAGMA trusted_schema=OFF")
                    db.execute("BEGIN")
                    header = _header(db)
                    if header.get("schema") != SCHEMA_VERSION or header.get("helper") != helper_sha:
                        continue
                    if "git_manifest" in header:
                        previous = read_manifest(db, header)
                        if previous.source_key != inputs.source_key:
                            continue
                    else:
                        previous = None
                        old_source = header.get("source", "")
                        if seed_identity is not None:
                            if old_source != seed_identity:
                                continue
                        elif Path(old_source).parent.parent != source.root.parent.parent:
                            continue
                    if db.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                        continue
                    with closing(sqlite3.connect(draft)) as copy:
                        db.backup(copy)
                    old_inputs = previous
                break
            except (OSError, ValueError, KeyError, TypeError, sqlite3.Error):
                draft.unlink(missing_ok=True)
        with closing(sqlite3.connect(draft)) as db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("PRAGMA trusted_schema=OFF")
            if not db.execute("PRAGMA application_id").fetchone()[0]:
                db.execute(f"PRAGMA application_id={APPLICATION_ID}")
                db.executescript(SCHEMA)
            db.execute("BEGIN IMMEDIATE")
            db.execute(INPUT_SCHEMA)
            counts = _changes(db, inputs, old_inputs, git, helper)
            source_check()
            header = {"schema": SCHEMA_VERSION, "helper": helper_sha, "source": source.identity,
                      **inputs.context, "indexed_revision": inputs.revision}
            db.execute("DELETE FROM metadata")
            db.executemany("INSERT INTO metadata VALUES (?,?)", header.items())
            old = {entry.path: entry for entry in old_inputs.entries} if old_inputs else {}
            current = {entry.path: entry for entry in inputs.entries}
            if old_inputs is None:
                db.execute("DELETE FROM git_inputs")
            else:
                db.executemany("DELETE FROM git_inputs WHERE path=?", ((path,) for path in old.keys() - current.keys()))
            db.executemany("INSERT OR REPLACE INTO git_inputs VALUES (?,?,?,?,?)",
                           (entry.row() for entry in inputs.entries if old.get(entry.path) != entry))
            db.commit()
            if db.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                raise ValueError("Созданный индекс не прошёл проверку SQLite.")
        if _stamp(path) != before:
            raise ValueError("Индекс изменился в другом окне. Повторите обновление.")
        draft.replace(path)
        return counts
