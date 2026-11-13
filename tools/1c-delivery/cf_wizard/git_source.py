"""Commit-pinned sparse Git export, independent of a hosting API."""
from dataclasses import dataclass
from pathlib import Path
from tempfile import NamedTemporaryFile

from stage import _no_links
from .git_api import Blob, Tree, GitError, _key, relative_path
from .git_index_inputs import GitIndexEntry, GitIndexInputs

@dataclass(frozen=True)
class Entry:
    name: str
    path: str
    directory: bool

    def is_dir(self):
        return self.directory

    def is_file(self):
        return not self.directory


def _atomic_bytes(path, data):
    _no_links(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with NamedTemporaryFile(dir=path.parent, prefix=".cf-git-", delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(data)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


class GitFiles:
    """A local materialization of only the files read at this commit."""

    def __init__(self, client, directory, branch, revision, tree, cache, seed_root=None, seed_index=None):
        self.client, self.directory, self.branch, self.revision = client, directory, branch, revision
        parts = (client.server, client.repository, directory)
        repo_key = _key(*parts) if getattr(client, "provider", "gitea") == "gitea" else _key("github", *parts)
        self.preference_key = "git:" + _key(repo_key, branch)
        root = cache.absolute() / repo_key / self.preference_key[4:] / revision / "config"
        _no_links(root)
        self.root = root.resolve()
        self.blobs = cache.absolute() / repo_key / "blobs"
        self.seed_root, self.seed_index = seed_root, seed_index
        self._directories = {"": self._checked_tree(tree)}
        self._index_inputs = None

    @staticmethod
    def _checked_tree(tree):
        seen = set()
        for name, entry in tree.items():
            relative_path(name)
            folded = name.casefold()
            if folded in seen:
                raise GitError("Коллизия путей в каталоге Git для Windows.")
            if not isinstance(entry, (Tree, Blob)):
                raise GitError(f"Ожидался обычный файл или каталог Git. {name}.")
            seen.add(folded)
        return tree

    def relative(self, path):
        return Path(str(path).removeprefix("\\\\?\\")).relative_to(self.root).as_posix()

    def prepare_index_inputs(self, descriptors):
        """Resolve the complete input inventory in the opening worker, never in Qt."""
        owners = {relative: owner for owner, relative in descriptors.items()}
        paths = set(owners) | {"Configuration.xml"}
        paths.update(f"Documents/{name}" for name, entry in self._entries("Documents").items()
                     if isinstance(entry, Blob) and name.endswith(".xml"))
        entries = []
        for relative in sorted(paths):
            blob = self.blob(relative)
            if blob is None:
                raise GitError(f"В Git-выгрузке отсутствует XML-описание. {relative}.")
            roles = []
            if relative == "Configuration.xml":
                roles.append("configuration")
            if relative in owners:
                roles.append("descriptor")
            if relative.startswith("Documents/") and relative.count("/") == 1:
                roles.append("registrar")
            entries.append(GitIndexEntry(relative, blob.oid, blob.size, owners.get(relative, ""), tuple(roles)))
        inputs = GitIndexInputs(self.preference_key, self.revision, tuple(entries))
        self.verify("Configuration.xml", (self.root / "Configuration.xml").read_bytes())
        if self._index_inputs is not None and self._index_inputs != inputs:
            raise GitError("Состав закреплённой ревизии Git изменился.")
        self._index_inputs = inputs
        return inputs

    @property
    def index_inputs(self):
        if self._index_inputs is None:
            raise GitError("Входы индекса Git ещё не проверены. Откройте ветку заново.")
        return self._index_inputs

    def _entries(self, relative):
        if relative not in self._directories:
            parent, _, name = relative.rpartition("/")
            entry = self._entries(parent).get(name)
            self._directories[relative] = self._checked_tree(self.client.tree(entry.oid)) if isinstance(entry, Tree) else {}
        return self._directories[relative]

    def blob(self, relative):
        parent, _, name = relative_path(relative).rpartition("/")
        entry = self._entries(parent).get(name)
        return entry if isinstance(entry, Blob) else None

    def listing(self, path):
        relative = self.relative(path)
        return [Entry(name, str(Path(path) / name), isinstance(entry, Tree)) for name, entry in
                sorted(self._entries("" if relative == "." else relative).items())]

    def owned_files(self, descriptors):
        files = set(descriptors)
        folders = [path.removesuffix(".xml") for path in descriptors]
        while folders:
            folder = folders.pop()
            for name, entry in self._entries(folder).items():
                relative = f"{folder}/{name}"
                if isinstance(entry, Tree):
                    folders.append(relative)
                else:
                    files.add(relative)
        return files

    def _cached(self, rel):
        blob = self.blob(rel)
        places = [self.root / rel, self.blobs / blob.oid]
        if self.seed_root is not None:
            places.append(self.seed_root / rel)
        for path in places:
            _no_links(path)
            if path.is_file() and path.stat().st_size == blob.size:
                data = path.read_bytes()
                if blob.matches(data):
                    return data
        return None

    def verify(self, rel, data):
        blob = self.blob(rel)
        if blob is None or not blob.matches(data):
            raise GitError(f"Байты файла отличаются от описи Git. {rel}.")

    def _put(self, rel, data):
        self.verify(rel, data)
        blob = self.blob(rel)
        if data.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
            raise GitError(f"Файл хранится в Git LFS. {rel}.")
        target = self.root / rel
        cached = self.blobs / blob.oid
        _no_links(cached)
        _no_links(target)
        if not cached.is_file() or not blob.matches(cached.read_bytes()):
            _atomic_bytes(cached, data)
        if not target.is_file() or not blob.matches(target.read_bytes()):
            _atomic_bytes(target, data)
        return target

    def ensure(self, relative):
        rel = relative_path(relative)
        blob = self.blob(rel)
        if blob is None:
            raise GitError(f"Файл отсутствует в зафиксированной ревизии Git. {rel}.")
        target = self.root / rel
        _no_links(target)
        if target.is_file() and target.stat().st_size == blob.size:
            data = target.read_bytes()
            if blob.matches(data):
                return target
        data = self._cached(rel)
        if data is None:
            data = self.client.file(self.revision, f"{self.directory}/{rel}", blob)
        return self._put(rel, data)

    def metadata(self, descriptors, *, paths=None):
        if paths is None:
            paths = set(descriptors.values())
            paths.update(f"Documents/{name}" for name, entry in self._entries("Documents").items()
                         if isinstance(entry, Blob) and name.endswith(".xml"))
        else:
            paths = set(paths)
        missing = {rel for rel in paths if self.blob(rel) is None}
        if missing:
            raise GitError("В Git-выгрузке отсутствуют XML-описания. " + ", ".join(sorted(missing)[:5]))
        batch = []

        def download():
            names = [f"{self.directory}/{rel}" for rel in batch]
            contents = self.client.files(self.revision, names)
            for rel, name in zip(batch, names):
                if name in contents:
                    self._put(rel, contents[name])
                else:
                    self.ensure(rel)
            batch.clear()

        for rel in sorted(paths):
            data = self._cached(rel)
            if data is not None:
                self._put(rel, data)
            else:
                batch.append(rel)
                if len(batch) >= 20:
                    download()
        if batch:
            download()


class ExportSourceFactory:
    def __init__(self, open_export, cache):
        self.open_export, self.cache = open_export, cache

    def strategy(self, mode):
        return {"local": self.open_export, "git": self.git}[mode]

    def git(self, client, branch, directory, seed_root=None, seed_index=None):
        revision = client.branch_sha(branch)
        tree = client.config_tree(revision, directory)
        files = GitFiles(client, directory, branch, revision, tree, self.cache, seed_root, seed_index)
        catalog = self.open_export(str(files.ensure("Configuration.xml")))
        files.prepare_index_inputs(dict(catalog.descriptors))
        return catalog.with_git(files)
