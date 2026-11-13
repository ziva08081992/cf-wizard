"""Immutable proof of the metadata used by a dependency index."""
from dataclasses import dataclass, field
from hashlib import sha256
import json
import re


MANIFEST_VERSION = "1"
_OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")


@dataclass(frozen=True, order=True)
class GitIndexEntry:
    path: str
    oid: str
    size: int
    owner: str
    roles: tuple[str, ...]

    def row(self):
        return (self.path, self.oid, self.size, self.owner, ",".join(self.roles))


@dataclass(frozen=True)
class GitIndexInputs:
    source_key: str
    revision: str
    entries: tuple[GitIndexEntry, ...]
    _fingerprint: str = field(init=False, repr=False, compare=False)

    def __post_init__(self):
        if not self.source_key.startswith("git:") or not _OID.fullmatch(self.revision):
            raise ValueError("Не подтверждён источник индекса Git.")
        ordered = tuple(sorted(self.entries))
        paths = [entry.path.casefold() for entry in ordered]
        if len(set(paths)) != len(paths):
            raise ValueError("Повтор пути во входах индекса Git.")
        for entry in ordered:
            if (not _OID.fullmatch(entry.oid) or type(entry.size) is not int or entry.size < 0
                    or not entry.roles or tuple(sorted(set(entry.roles))) != entry.roles
                    or set(entry.roles) - {"configuration", "descriptor", "registrar"}
                    or entry.path.startswith("/") or "\\" in entry.path
                    or any(part in ("", ".", "..") for part in entry.path.split("/"))):
                raise ValueError("Некорректные входы индекса Git.")
        configuration = [entry for entry in ordered if "configuration" in entry.roles]
        if len(configuration) != 1 or configuration[0].path != "Configuration.xml":
            raise ValueError("Во входах индекса нет Configuration.xml.")
        object.__setattr__(self, "entries", ordered)
        payload = [MANIFEST_VERSION, self.source_key, [entry.row() for entry in ordered]]
        digest = sha256(json.dumps(payload, ensure_ascii=True, separators=(",", ":")).encode("ascii")).hexdigest()
        object.__setattr__(self, "_fingerprint", digest)

    @property
    def fingerprint(self):
        return self._fingerprint

    @property
    def context(self):
        return {"git_manifest": MANIFEST_VERSION, "git_source": self.source_key,
                "git_fingerprint": self.fingerprint}

    def inventory(self):
        return {entry.path: entry.row()[1:] for entry in self.entries}
