"""SQLite index of the pinned helper's field types and registrar links."""

from collections import Counter
from contextlib import closing
from hashlib import sha256
import json
import os
from pathlib import Path
import sqlite3
import stat
from tempfile import NamedTemporaryFile, TemporaryDirectory
import zlib


APPLICATION_ID = 0x43464958
# Change the version when either the schema or the extracted facts change.
SCHEMA_VERSION = "1"
SCHEMA = """
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
    CREATE TABLE objects (
        name TEXT PRIMARY KEY, content_hash BLOB NOT NULL, facts BLOB NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE documents (file TEXT PRIMARY KEY, records_hash BLOB NOT NULL) WITHOUT ROWID;
    CREATE TABLE registrar_links (
        file TEXT, ordinal INTEGER, target TEXT NOT NULL,
        PRIMARY KEY (file, ordinal), FOREIGN KEY (file) REFERENCES documents(file) ON DELETE CASCADE
    ) WITHOUT ROWID;
    CREATE INDEX registrars_by_target ON registrar_links(target, file, ordinal);
"""


def _readonly(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5)


def _header(db: sqlite3.Connection) -> dict[str, str]:
    if db.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
        raise ValueError("Выбранный файл не является индексом мастера CF.")
    return dict(db.execute("SELECT key, value FROM metadata"))


def _records(data: bytes) -> bytes:
    start = data.find(b"<RegisterRecords>")
    end = data.find(b"</RegisterRecords>")
    return data[start:end] if start >= 0 and end >= 0 else b""


def descriptor_inventory(root: Path, descriptors: dict[str, str], native=os.path.join) -> dict[str, tuple[int, int, int]]:
    expected = set(descriptors.values())
    folders = {path.partition("/")[0] for path in expected} | {"Documents"}
    files = {}
    for folder in sorted(folders):
        directory = native(str(root), folder)
        try:
            info = os.lstat(directory)
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(info.st_mode) or getattr(info, "st_file_attributes", 0) & 1024:
            raise ValueError(f"Ожидалась обычная папка XML-выгрузки: {folder}")
        with os.scandir(directory) as entries:
            for entry in entries:
                relative = f"{folder}/{entry.name}"
                if relative not in expected and not (folder == "Documents" and entry.name.endswith(".xml")):
                    continue
                info = entry.stat(follow_symlinks=False)
                if not stat.S_ISREG(info.st_mode) or getattr(info, "st_file_attributes", 0) & 1024:
                    raise ValueError(f"Ожидался обычный XML-файл: {relative}")
                files[relative] = (info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    missing = expected - files.keys()
    if missing:
        raise ValueError("В выгрузке отсутствуют описания объектов: " + ", ".join(sorted(missing)[:5]))
    return files


def _pack_members(members) -> list:
    return [[name, member.tag, sorted(member.types), _pack_members(member.columns)]
            for name, member in members.items()]


def _unpack_members(rows: list, member_type) -> dict:
    members = {}
    for name, tag, targets, columns in rows:
        member = member_type(tag, set(targets))
        member.columns = _unpack_members(columns, member_type)
        members[name] = member
    return members


def _facts(data: bytes, helper: dict) -> bytes:
    text = data.decode("utf-8", "surrogateescape")
    members, targets = {}, set()
    if "<ChildObjects" in text or "<Type" in text:
        try:
            obj = helper["object_element"](text)
            if obj:
                children = helper["children"](text, obj.inner, obj.inner_end)
                child_objects = helper["find"](children, "ChildObjects")
                if child_objects:
                    members = helper["index_members"](text, child_objects)
                targets = helper["type_keys"](text, obj)
        finally:
            helper["_children_cache"].clear()
    payload = json.dumps([sorted(targets), _pack_members(members)], separators=(",", ":"))
    return zlib.compress(payload.encode("ascii"), level=1)


def _update(db, root, descriptors, helper, source_check, verify_file):
    native = helper["native"]

    def read_descriptor(relative):
        data = Path(native(str(root), relative)).read_bytes()
        if verify_file is not None:
            verify_file(relative, data)
        return data

    before = descriptor_inventory(root, descriptors, native)
    known = dict(db.execute("SELECT name, content_hash FROM objects"))
    removed = known.keys() - descriptors.keys()
    db.executemany("DELETE FROM objects WHERE name=?", ((name,) for name in removed))
    counts = Counter(removed=len(removed))
    records = {}
    for key, relative in descriptors.items():
        data = read_descriptor(relative)
        if relative.startswith("Documents/"):
            records[relative.partition("/")[2]] = _records(data)
        fingerprint = sha256(data).digest()
        if known.get(key) == fingerprint:
            counts["unchanged"] += 1
            continue
        db.execute("INSERT OR REPLACE INTO objects VALUES (?,?,?)", (key, fingerprint, _facts(data, helper)))
        counts["changed" if key in known else "added"] += 1

    for relative in before.keys() - descriptors.values():
        records[relative.partition("/")[2]] = _records(read_descriptor(relative))
    known_records = dict(db.execute("SELECT file, records_hash FROM documents"))
    db.executemany("DELETE FROM documents WHERE file=?", ((name,) for name in known_records.keys() - records.keys()))
    for name, fragment in records.items():
        fingerprint = sha256(fragment).digest()
        if known_records.get(name) == fingerprint:
            continue
        db.execute("DELETE FROM documents WHERE file=?", (name,))
        db.execute("INSERT INTO documents VALUES (?,?)", (name, fingerprint))
        refs = helper["scan"](fragment.decode("utf-8", "surrogateescape"))
        db.executemany("INSERT INTO registrar_links VALUES (?,?,?)",
                       ((name, ordinal, helper["key_of"](ref.kind, ref.name)) for ordinal, ref in enumerate(refs)))
    if descriptor_inventory(root, descriptors, native) != before:
        raise ValueError("Описания объектов изменились во время обновления индекса. Повторите операцию.")
    source_check()
    return counts


def update_index(path: Path, root: Path, identity: str, descriptors: dict[str, str],
                 helper: dict, helper_sha: str, source_check, *, verify_file=None) -> Counter:
    expected = {"schema": SCHEMA_VERSION, "source": identity, "helper": helper_sha}
    previous = None
    if path.exists():
        with closing(_readonly(path)) as db:
            previous = _header(db)
        if previous.get("source") != identity:
            raise ValueError("Этот файл индекса относится к другой XML-выгрузке.")
    if previous == expected:
        with closing(sqlite3.connect(path, timeout=5)) as db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("BEGIN IMMEDIATE")
            if _header(db) != expected:
                raise ValueError("Индекс обновился в другом окне. Повторите операцию.")
            changes = _update(db, root, descriptors, helper, source_check, verify_file)
            db.commit()
            return changes

    with NamedTemporaryFile(dir=path.parent, prefix=".cf-index-", suffix=".tmp", delete=False) as file:
        draft = Path(file.name)
    try:
        with closing(sqlite3.connect(draft)) as db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute(f"PRAGMA application_id={APPLICATION_ID}")
            db.executescript(SCHEMA)
            db.executemany("INSERT INTO metadata VALUES (?,?)", expected.items())
            changes = _update(db, root, descriptors, helper, source_check, verify_file)
            db.commit()
            if db.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                raise ValueError("Созданный индекс не прошёл проверку SQLite.")
        if path.exists():
            with closing(_readonly(path)) as db:
                if previous is None or _header(db) != previous:
                    raise ValueError("Индекс обновился в другом окне. Повторите операцию.")
        draft.replace(path)
        return changes
    finally:
        draft.unlink(missing_ok=True)


def update_git_index(path, source, descriptors, helper, helper_sha, source_check):
    """Build the new index privately, optionally preserving previously checked facts."""
    git = source.git

    def stamp():
        try:
            info = path.stat()
            return info.st_size, info.st_mtime_ns, info.st_ctime_ns
        except FileNotFoundError:
            return None

    before = stamp()
    with TemporaryDirectory(prefix=".cf-git-index-", dir=path.parent) as folder:
        draft = Path(folder) / "index.sqlite3"
        seeds = [(path, None)]
        if git.seed_root is not None and git.seed_index is not None:
            seeds.append((git.seed_index, os.path.normcase(str(git.seed_root))))
        for candidate, seed_identity in seeds:
            if not candidate.is_file():
                continue
            try:
                with closing(_readonly(candidate)) as db:
                    db.execute("PRAGMA trusted_schema=OFF")
                    header = _header(db)
                    old = header.get("source", "")
                    same_branch = Path(old).parent.parent == source.root.parent.parent
                    if (header.get("schema") != SCHEMA_VERSION or header.get("helper") != helper_sha
                            or (old != seed_identity if seed_identity is not None else not same_branch)
                            or db.execute("PRAGMA quick_check").fetchone()[0] != "ok"):
                        continue
                    with closing(sqlite3.connect(draft)) as copy:
                        db.backup(copy)
                        copy.execute("PRAGMA trusted_schema=OFF")
                        copy.execute("UPDATE metadata SET value=? WHERE key='source'", (source.identity,))
                        copy.commit()
                break
            except (OSError, ValueError, sqlite3.Error):
                draft.unlink(missing_ok=True)
        counts = update_index(draft, source.root, source.identity, descriptors, helper, helper_sha,
                              source_check, verify_file=git.verify)
        if stamp() != before:
            raise ValueError("Индекс изменился в другом окне. Повторите обновление.")
        draft.replace(path)
        return counts


class IndexReader:
    """Read a consistent database snapshot, verifying the XML behind each fact."""

    def __init__(self, db: sqlite3.Connection, *, pinned=False):
        self.db = db
        self._objects = {}
        self._registrars = {}
        self._pinned = pinned
        self._valid_registrars: bool | None = True if pinned else None
        self.hits = Counter()

    @classmethod
    def open(cls, path: Path, identity: str, helper_sha: str, *, pinned=False):
        db = None
        try:
            db = _readonly(path)
            db.execute("PRAGMA trusted_schema=OFF")
            db.execute("BEGIN")
            if _header(db) == {"schema": SCHEMA_VERSION, "source": identity, "helper": helper_sha}:
                return cls(db, pinned=pinned)
        except (OSError, ValueError, sqlite3.Error):
            pass
        if db is not None:
            db.close()
        return None

    def close(self):
        self.db.close()

    def object(self, name, text, member_type):
        if name not in self._objects:
            result = None
            try:
                row = self.db.execute("SELECT content_hash, facts FROM objects WHERE name=?", (name,)).fetchone()
                if row and row[0] == sha256(text.encode("utf-8", "surrogateescape")).digest():
                    targets, members = json.loads(zlib.decompress(row[1]))
                    result = set(targets), _unpack_members(members, member_type)
            except (TypeError, ValueError, sqlite3.Error, zlib.error):
                pass
            self.hits["objects" if result is not None else "fallback"] += 1
            self._objects[name] = result
        return self._objects[name]

    def registrars(self, key, src, native, listing, read_bytes):
        if key in self._registrars:
            return self._registrars[key]
        try:
            if self._valid_registrars is None:
                known = dict(self.db.execute("SELECT file, records_hash FROM documents"))
                current = {entry.name: sha256(_records(read_bytes(entry.path))).digest()
                           for entry in listing(native(src, "Documents"))
                           if entry.name.endswith(".xml") and entry.is_file()}
                self._valid_registrars = current == known
                if not self._valid_registrars:
                    self.hits["fallback"] += 1
            if self._valid_registrars:
                rows = self.db.execute("SELECT file FROM registrar_links WHERE target=? ORDER BY file, ordinal", (key,))
                documents = ["Document." + file[:-4] for (file,) in rows]
                self._registrars[key] = documents
                self.hits["registrars"] += 1
                return documents
        except (OSError, sqlite3.Error):
            if self._pinned:
                raise ValueError("Не удалось прочитать регистраторы из индекса Gitea. Обновите индекс.") from None
            self._valid_registrars = False
            self.hits["fallback"] += 1
        return None
