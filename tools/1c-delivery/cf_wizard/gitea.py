"""Read-only Gitea transport and commit-pinned, sparse export files."""

from base64 import b64decode
from dataclasses import dataclass
from hashlib import new as hash_new, sha256
from http.client import HTTPException
import json
from pathlib import Path
import re
from tempfile import NamedTemporaryFile
from urllib.error import HTTPError
from urllib.parse import quote, urlencode, urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from stage import _no_links


DEFAULT_GITEA = {
    "server": "", "repository": "",
    "directory": "config", "branch": "", "interval": 60,
}
OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
DEVICE = re.compile(r"(?:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?\Z", re.I)


def relative_path(value):
    if not isinstance(value, str) or any(
        not part or part in (".", "..") or part.endswith((" ", ".")) or DEVICE.fullmatch(part)
        or any(ord(ch) < 32 or ch in '<>:"\\|?*' for ch in part)
        for part in value.split("/")
    ):
        raise ValueError("Недопустимый путь файла в Gitea.")
    return value


def interval_seconds(value):
    try:
        seconds = int(value)
    except (TypeError, ValueError):
        raise ValueError("Введите интервал опроса в секундах.") from None
    if seconds != 0 and not 30 <= seconds <= 86400:
        raise ValueError("Интервал должен быть 0 или от 30 до 86400 секунд.")
    return seconds


def server_address(value):
    raw = value.strip().rstrip("/")
    url = urlsplit(raw)
    if (url.scheme != "https" or not url.hostname or url.username is not None or url.query
            or url.fragment or "\\" in raw or any(ch.isspace() for ch in raw)):
        raise ValueError("Укажите базовый HTTPS-адрес Gitea.")
    return urlunsplit(("https", url.netloc.lower(), url.path, "", ""))


def _key(*parts):
    return sha256("\0".join(parts).encode("utf-8")).hexdigest()[:24]


def credential_target(server):
    return "CF-Wizard/Gitea/" + _key(server_address(server))


class GiteaError(ValueError):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


@dataclass(frozen=True)
class Blob:
    oid: str
    size: int

    def matches(self, data):
        if len(data) != self.size:
            return False
        digest = hash_new("sha1" if len(self.oid) == 40 else "sha256")
        digest.update(f"blob {len(data)}\0".encode("ascii"))
        digest.update(data)
        return digest.hexdigest() == self.oid


@dataclass(frozen=True)
class Tree:
    oid: str


class GiteaClient:
    def __init__(self, server, repository, token):
        self.server = server_address(server)
        self.repository = relative_path(repository.strip())
        if len(self.repository.split("/")) != 2:
            raise ValueError("Укажите репозиторий в формате владелец/репозиторий.")
        if not token or not all(33 <= ord(ch) <= 126 for ch in token):
            raise ValueError("Введите персональный токен Gitea.")
        self._token = token
        self._base = self.server + "/api/v1/repos/" + quote(self.repository, safe="/")
        self._opener = build_opener(_NoRedirect())
        self._trees = {}

    def _get(self, endpoint, params, operation, *, limit=None):
        url = self._base + "/" + endpoint
        if params:
            url += "?" + urlencode(params)
        request = Request(url, headers={"Authorization": f"token {self._token}"}, method="GET")
        try:
            with self._opener.open(request, timeout=20) as response:
                return response.read() if limit is None else response.read(limit)
        except HTTPError as error:
            error.close()
            raise GiteaError(f"Gitea вернула HTTP {error.code}. {operation}.", error.code) from None
        except (OSError, HTTPException, ValueError):
            raise GiteaError(f"Не удалось прочитать Gitea. {operation}.") from None

    def _json(self, endpoint, params, operation):
        try:
            return json.loads(self._get(endpoint, params, operation))
        except (UnicodeError, json.JSONDecodeError):
            raise GiteaError(f"Gitea вернула неверный ответ. {operation}.") from None

    def branches(self):
        result = {}
        for page in range(1, 10001):
            rows = self._json("branches", {"page": page, "limit": 50}, "Получение веток")
            if not isinstance(rows, list) or any(not isinstance(row, dict) or
                    not isinstance(row.get("name"), str) or not row["name"] for row in rows):
                raise GiteaError("Gitea вернула неверный список веток.")
            if not rows:
                return list(result)
            count = len(result)
            result.update(dict.fromkeys(row["name"] for row in rows))
            if len(result) == count:
                raise GiteaError("Gitea повторила страницу списка веток.")
        raise GiteaError("Превышен предел страниц списка веток.")

    def branch_sha(self, branch):
        if not branch:
            raise ValueError("Выберите ветку Gitea.")
        ref = "refs/heads/" + branch
        rows = self._json("git/refs/heads/" + quote(branch, safe="/"), {}, "Чтение ревизии ветки")
        if isinstance(rows, dict):
            rows = [rows]
        if not isinstance(rows, list):
            raise GiteaError("Gitea вернула неверную ссылку ветки.")
        for row in rows:
            if isinstance(row, dict) and row.get("ref") == ref:
                obj = row.get("object")
                sha = obj.get("sha") if isinstance(obj, dict) else None
                if isinstance(sha, str) and OID.fullmatch(sha):
                    return sha
        raise GiteaError("Ответ Gitea не подтверждает SHA выбранной ветки.")

    def config_tree(self, revision, directory):
        tree = self.tree(revision)
        for name in relative_path(directory).split("/"):
            entry = tree.get(name)
            if not isinstance(entry, Tree):
                raise GiteaError("Каталог XML-выгрузки отсутствует в выбранной ревизии Gitea.")
            tree = self.tree(entry.oid)
        return tree

    def tree(self, oid):
        if not OID.fullmatch(oid):
            raise ValueError("Недопустимый SHA дерева Gitea.")
        if oid in self._trees:
            return self._trees[oid]

        entries = {}
        total = None
        for page in range(1, 10001):
            data = self._json("git/trees/" + oid, {"page": page, "per_page": 1000}, "Чтение каталога Git")
            rows = data.get("tree") if isinstance(data, dict) else None
            if (not isinstance(rows, list) or type(data.get("truncated")) is not bool
                    or data.get("page") != page or data.get("sha") != oid):
                raise GiteaError("Gitea вернула неверную страницу каталога.")
            count = data.get("total_count")
            if type(count) is not int or count < 0 or (total is not None and total != count):
                raise GiteaError("Размер каталога Gitea изменился при чтении.")
            total = count
            for row in rows:
                name = row.get("path") if isinstance(row, dict) else None
                if not isinstance(name, str) or not name or "/" in name or name in entries:
                    raise GiteaError("В каталоге Gitea повторяется или отсутствует имя.")
                kind, mode, entry_oid, size = row.get("type"), row.get("mode"), row.get("sha"), row.get("size")
                if not isinstance(entry_oid, str) or not OID.fullmatch(entry_oid):
                    raise GiteaError("В каталоге Gitea указан неверный идентификатор объекта.")
                if kind == "tree" and mode == "040000":
                    entries[name] = Tree(entry_oid)
                elif kind == "blob" and mode in ("100644", "100755") and type(size) is int and size >= 0:
                    entries[name] = Blob(entry_oid, size)
                else:
                    entries[name] = None
            if not data["truncated"]:
                if len(entries) != total:
                    raise GiteaError("Gitea вернула неполный каталог.")
                if len(self._trees) >= 128:
                    self._trees.pop(next(iter(self._trees)))
                self._trees[oid] = entries
                return entries
            if not rows:
                raise GiteaError("Gitea вернула пустую страницу неполного каталога.")
        raise GiteaError("Превышен предел страниц каталога Gitea.")

    def file(self, revision, path, blob):
        return self._get("raw/" + quote(path, safe="/"), {"ref": revision}, f"Чтение {path}",
                         limit=blob.size + 1)

    def files(self, revision, paths):
        data = self._json(
            "file-contents", {"ref": revision, "body": json.dumps({"files": paths}, ensure_ascii=False)},
            "Чтение XML-описаний",
        )
        if not isinstance(data, list):
            raise GiteaError("Gitea вернула неверный список файлов.")
        result = {}
        for row in data:
            if not isinstance(row, dict) or row.get("path") not in paths or row.get("encoding") != "base64":
                continue
            path = row["path"]
            if path in result:
                raise GiteaError("Gitea вернула повтор файла в пакете.")
            try:
                result[path] = b64decode(row["content"], validate=True)
            except (TypeError, ValueError, KeyError):
                raise GiteaError("Неверное содержимое XML в ответе Gitea.") from None
        return result


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
        repo_key = _key(client.server, client.repository, directory)
        self.preference_key = "git:" + _key(repo_key, branch)
        root = cache.absolute() / repo_key / self.preference_key[4:] / revision / "config"
        _no_links(root)
        self.root = root.resolve()
        self.blobs = cache.absolute() / repo_key / "blobs"
        self.seed_root, self.seed_index = seed_root, seed_index
        self._directories = {"": self._checked_tree(tree)}

    @staticmethod
    def _checked_tree(tree):
        seen = set()
        for name, entry in tree.items():
            relative_path(name)
            folded = name.casefold()
            if folded in seen:
                raise GiteaError("Коллизия путей в каталоге Gitea для Windows.")
            if not isinstance(entry, (Tree, Blob)):
                raise GiteaError(f"Ожидался обычный файл или каталог Git. {name}.")
            seen.add(folded)
        return tree

    def relative(self, path):
        return Path(str(path).removeprefix("\\\\?\\")).relative_to(self.root).as_posix()

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
            raise GiteaError(f"Байты файла отличаются от описи Git. {rel}.")

    def _put(self, rel, data):
        self.verify(rel, data)
        blob = self.blob(rel)
        if data.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
            raise GiteaError(f"Файл хранится в Git LFS. {rel}.")
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
            raise GiteaError(f"Файл отсутствует в зафиксированной ревизии Gitea. {rel}.")
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

    def metadata(self, descriptors):
        paths = set(descriptors.values())
        paths.update(f"Documents/{name}" for name, entry in self._entries("Documents").items()
                     if isinstance(entry, Blob) and name.endswith(".xml"))
        missing = {rel for rel in paths if self.blob(rel) is None}
        if missing:
            raise GiteaError("В Git-выгрузке отсутствуют XML-описания. " + ", ".join(sorted(missing)[:5]))
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
                encoded = urlencode({"body": json.dumps(batch, ensure_ascii=False)})
                if len(batch) >= 20 or len(encoded) >= 4500:
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
        return self.open_export(str(files.ensure("Configuration.xml"))).with_git(files)
