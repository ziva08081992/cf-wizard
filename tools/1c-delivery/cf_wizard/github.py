"""Read-only GitHub.com strategy. All content is pinned to Git object IDs."""
from concurrent.futures import CancelledError
import json
from urllib.parse import quote, urlencode
from urllib.request import build_opener

from .git_api import (Blob, Tree, GitError, GitTransport, OID, MAX_COMMIT_PAGES,
                      _NoRedirect, connection_server, relative_path)

JSON_LIMIT = 32 * 1024 * 1024
BLOB_LIMIT = 100 * 1024 * 1024
BATCH_BYTES = 2 * 1024 * 1024


def _oid(value):
    if not isinstance(value, str) or not OID.fullmatch(value) or len(value) != 40:
        raise GitError("GitHub вернула неверный SHA объекта.")
    return value


def _cancel(event):
    if event is not None and event.is_set():
        raise CancelledError


class GitHubClient(GitTransport):
    provider = "github"
    label = "GitHub"
    _auth_scheme = "Bearer"

    def __init__(self, server, repository, token):
        self.server = connection_server(server, self.provider)
        self.repository = relative_path(repository.strip())
        if len(self.repository.split("/")) != 2:
            raise ValueError("Укажите репозиторий в формате владелец/репозиторий.")
        if not token or not all(33 <= ord(ch) <= 126 for ch in token):
            raise ValueError("Введите персональный токен GitHub.")
        self._token = token
        self._base = "https://api.github.com/repos/" + quote(self.repository, safe="/")
        self._headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2026-03-10"}
        self._opener = build_opener(_NoRedirect())
        self._trees, self._commits = {}, {}

    def _get(self, endpoint, params, operation, *, limit=JSON_LIMIT + 1, headers=None):
        url = self._base + "/" + endpoint
        if params:
            url += "?" + urlencode(params)
        return self._request(url, operation, limit=limit, headers=headers)

    @staticmethod
    def _decode(data):
        if len(data) > JSON_LIMIT:
            raise GitError("Ответ GitHub превышает допустимый размер.")
        try:
            return json.loads(data)
        except (UnicodeError, ValueError):
            raise GitError("GitHub вернула неверный JSON.") from None

    def _json(self, endpoint, params, operation):
        return self._decode(self._get(endpoint, params, operation))

    def branches(self):
        result = []
        seen = set()
        for page in range(1, 10001):
            rows = self._json("branches", {"per_page": 100, "page": page}, "Получение веток")
            if not isinstance(rows, list):
                raise GitError("GitHub вернула неверный список веток.")
            if not rows:
                return result
            for row in rows:
                name = row.get("name") if isinstance(row, dict) else None
                if not isinstance(name, str) or not name or name in seen:
                    raise GitError("GitHub вернула неверную или повторную ветку.")
                seen.add(name)
                result.append(name)
        raise GitError("Превышен предел страниц списка веток GitHub.")

    def branch_sha(self, branch):
        if not branch or any(ord(ch) < 32 for ch in branch):
            raise ValueError("Выберите ветку GitHub.")
        row = self._json("git/ref/heads/" + quote(branch, safe="/"), {}, "Чтение ревизии ветки")
        obj = row.get("object") if isinstance(row, dict) else None
        if not isinstance(row, dict) or row.get("ref") != "refs/heads/" + branch or not isinstance(obj, dict) or obj.get("type") != "commit":
            raise GitError("GitHub не подтвердила выбранную ветку.")
        return _oid(obj.get("sha"))

    def _commit(self, revision):
        _oid(revision)
        if revision not in self._commits:
            row = self._json("git/commits/" + revision, {}, "Чтение объекта коммита")
            if not isinstance(row, dict) or row.get("sha") != revision or not isinstance(row.get("tree"), dict):
                raise GitError("GitHub не подтвердила объект коммита.")
            tree = _oid(row["tree"].get("sha"))
            parents = row.get("parents")
            if not isinstance(parents, list) or any(not isinstance(p, dict) for p in parents):
                raise GitError("GitHub не подтвердила родителей коммита.")
            parents = tuple(_oid(p.get("sha")) for p in parents)
            if len(self._commits) >= 256:
                self._commits.pop(next(iter(self._commits)))
            self._commits[revision] = tree, parents
        return self._commits[revision]

    def tree(self, oid):
        _oid(oid)
        if oid in self._trees:
            return self._trees[oid]
        # Omit recursive entirely: even recursive=false enables it in GitHub.
        data = self._json("git/trees/" + oid, {}, "Чтение каталога Git")
        if (not isinstance(data, dict) or data.get("sha") != oid
                or data.get("truncated") is not False or not isinstance(data.get("tree"), list)):
            raise GitError("GitHub вернула неполный или неверный каталог.")
        entries = {}
        for row in data["tree"]:
            name = row.get("path") if isinstance(row, dict) else None
            if (not isinstance(name, str) or not name or name in (".", "..")
                    or "/" in name or "\0" in name or name in entries):
                raise GitError("GitHub вернула неверное или повторное имя файла.")
            # Windows rules apply when GitFiles materializes the selected export,
            # not to unrelated entries elsewhere in a Git repository.
            entry_oid = _oid(row.get("sha"))
            kind, mode, size = row.get("type"), row.get("mode"), row.get("size")
            if kind == "tree" and mode == "040000":
                entries[name] = Tree(entry_oid)
            elif kind == "blob" and mode in ("100644", "100755") and type(size) is int and size >= 0:
                entries[name] = Blob(entry_oid, size, mode)
            else:
                entries[name] = None  # Rejected if the requested export uses it.
        if len(self._trees) >= 512:
            self._trees.pop(next(iter(self._trees)))
        self._trees[oid] = entries
        return entries

    def _directory(self, root, directory, cancelled=None):
        for name in relative_path(directory).split("/"):
            _cancel(cancelled)
            entry = self.tree(root).get(name)
            if not isinstance(entry, Tree):
                return None
            root = entry.oid
        _cancel(cancelled)
        return root

    def config_tree(self, revision, directory):
        root, _ = self._commit(revision)
        oid = self._directory(root, directory)
        if oid is None:
            raise GitError("Каталог XML-выгрузки отсутствует в выбранной ревизии GitHub.")
        return self.tree(oid)

    def _blob(self, revision, path):
        parent, _, name = relative_path(path).rpartition("/")
        root, _ = self._commit(revision)
        directory = self._directory(root, parent) if parent else root
        entry = self.tree(directory).get(name) if directory else None
        if not isinstance(entry, Blob):
            raise GitError("XML-файл отсутствует в выбранной ревизии GitHub.")
        return entry

    def file(self, revision, path, blob):
        _oid(revision)
        relative_path(path)
        _oid(blob.oid)
        if blob.size > BLOB_LIMIT:
            raise GitError("GitHub Blob API не поддерживает файлы больше 100 МБ.")
        data = self._get("git/blobs/" + blob.oid, {}, "Чтение файла Git",
                         limit=blob.size + 1, headers={"Accept": "application/vnd.github.raw+json"})
        if not blob.matches(data):
            raise GitError("Байты файла отличаются от описи GitHub.")
        return data

    def _xml_batch(self, revision, batch):
        owner, repo = self.repository.split("/")
        variables = {"owner": owner, "repo": repo}
        arguments, objects = [], []
        for i, (_, blob) in enumerate(batch):
            variables[f"oid{i}"] = blob.oid
            arguments.append(f"$oid{i}: GitObjectID!")
            objects.append(f"b{i}: object(oid: $oid{i}) {{ __typename ... on Blob {{ oid byteSize isBinary isTruncated text }} }}")
        query = ("query($owner: String!, $repo: String!, " + ", ".join(arguments) + ") { "
                 "repository(owner:$owner, name:$repo) { " + " ".join(objects) +
                 " } rateLimit { cost remaining resetAt } }")
        payload = json.dumps({"query": query, "variables": variables}).encode("utf8")
        data = self._decode(self._request("https://api.github.com/graphql", "Чтение XML-описаний",
                            data=payload, limit=JSON_LIMIT + 1, headers={"Content-Type": "application/json"}))
        if not isinstance(data, dict) or data.get("errors"):
            raise GitError("GitHub GraphQL не завершила чтение XML. Проверьте доступ и лимит API.")
        result = data.get("data")
        repository = result.get("repository") if isinstance(result, dict) else None
        if not isinstance(repository, dict):
            raise GitError("GitHub GraphQL не подтвердила доступ к репозиторию.")
        rate = result.get("rateLimit")
        if isinstance(rate, dict):
            self.rate_limit = {k: v for k, v in rate.items() if k in ("cost", "remaining") and type(v) is int}
        contents = {}
        for i, (path, blob) in enumerate(batch):
            if f"b{i}" not in repository:
                raise GitError("GitHub GraphQL вернула неполный пакет XML.")
            row = repository[f"b{i}"]
            if row is None:
                raise GitError("GitHub GraphQL не вернула запрошенный объект XML.")
            if (not isinstance(row, dict) or row.get("__typename") != "Blob"
                    or row.get("oid") != blob.oid or type(row.get("byteSize")) is not int
                    or row["byteSize"] != blob.size):
                raise GitError("GitHub GraphQL не подтвердила идентичность XML.")
            text = row.get("text")
            try:
                raw = text.encode("utf8") if isinstance(text, str) else None
            except UnicodeError:
                raw = None
            if (raw is None or row.get("isBinary") is not False or row.get("isTruncated") is not False
                    or not blob.matches(raw)):
                raw = self.file(revision, path, blob)
            contents[path] = raw
        return contents

    def files(self, revision, paths):
        _oid(revision)
        if len(set(paths)) != len(paths):
            raise GitError("В пакете XML GitHub повторяются пути.")
        contents, batch, size = {}, [], 0
        for path in paths:
            blob = self._blob(revision, path)
            if batch and (len(batch) >= 20 or size + blob.size > BATCH_BYTES):
                contents.update(self._xml_batch(revision, batch))
                batch, size = [], 0
            if blob.size > BATCH_BYTES:
                contents[path] = self.file(revision, path, blob)
            else:
                batch.append((path, blob))
                size += blob.size
        if batch:
            contents.update(self._xml_batch(revision, batch))
        return contents

    def commits(self, revision, *, since, until, cancelled, report):
        _oid(revision)
        params = {"sha": revision, "per_page": 100}
        if since is not None:
            params["since"] = since.isoformat()
        if until is not None:
            params["until"] = until.isoformat()
        seen = set()
        for page in range(1, MAX_COMMIT_PAGES + 1):
            _cancel(cancelled)
            rows = self._json("commits", {**params, "page": page}, "Чтение истории коммитов")
            _cancel(cancelled)
            if not isinstance(rows, list):
                raise GitError("GitHub вернула неверный список коммитов.")
            if not rows:
                return
            entries = []
            for row in rows:
                sha = _oid(row.get("sha") if isinstance(row, dict) else None)
                commit = row.get("commit")
                message = commit.get("message") if isinstance(commit, dict) else None
                if not isinstance(message, str) or sha in seen:
                    raise GitError("GitHub вернула неверную или повторную запись истории.")
                seen.add(sha)
                entries.append((sha, message))
            report(len(seen))
            yield from entries
        raise GitError("Достигнут предел страниц истории GitHub.")

    def commit_paths(self, sha, *, cancelled=None, directory=None):
        _cancel(cancelled)
        current, parents = self._commit(sha)
        _cancel(cancelled)
        previous = self._commit(parents[0])[0] if parents else None
        _cancel(cancelled)
        prefix = ""
        if directory:
            current = self._directory(current, directory, cancelled)
            previous = self._directory(previous, directory, cancelled) if previous else None
            prefix = relative_path(directory) + "/"
        pending, paths = [(previous, current, prefix)], set()
        while pending:
            _cancel(cancelled)
            old, new, prefix = pending.pop()
            if old == new:
                continue
            before = self.tree(old) if old else {}
            _cancel(cancelled)
            after = self.tree(new) if new else {}
            _cancel(cancelled)
            for name in before.keys() | after.keys():
                left, right = before.get(name), after.get(name)
                if (name in before and left is None) or (name in after and right is None):
                    raise GitError("Неподдерживаемый объект в изменённом каталоге GitHub.")
                if left == right:
                    continue
                path = prefix + name
                if isinstance(left, Blob) or isinstance(right, Blob):
                    paths.add(path)
                if isinstance(left, Tree) or isinstance(right, Tree):
                    pending.append((left.oid if isinstance(left, Tree) else None,
                                    right.oid if isinstance(right, Tree) else None, path + "/"))
        _cancel(cancelled)
        return sorted(paths)