"""Read-only Gitea API strategy; neutral source names remain import-compatible."""
from base64 import b64decode
from concurrent.futures import CancelledError
import json
from urllib.parse import quote, urlencode
from urllib.request import build_opener

from .git_api import (DEFAULT_GITEA, OID, MAX_COMMIT_PAGES, Blob, Tree, GitTransport,
                      GiteaError, credential_target, interval_seconds, relative_path,
                      server_address, _NoRedirect)
from .git_source import GitFiles, ExportSourceFactory

class GiteaClient(GitTransport):
    provider = "gitea"
    label = "Gitea"
    _auth_scheme = "token"

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
        return self._request(url, operation, limit=limit)

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

    def commits(self, revision, *, since, until, cancelled, report):
        """Yield commit IDs and messages from the pinned revision."""
        if not OID.fullmatch(revision):
            raise ValueError("Неверный SHA открытой ревизии Gitea.")
        params = {"sha": revision, "limit": 50, "files": "false", "stat": "false", "verification": "false"}
        if since is not None:
            params["since"] = since.isoformat()
        if until is not None:
            params["until"] = until.isoformat()
        seen = set()
        for page in range(1, MAX_COMMIT_PAGES + 1):
            if cancelled.is_set():
                raise CancelledError
            rows = self._json("commits", {**params, "page": page}, "Чтение истории коммитов")
            if cancelled.is_set():
                raise CancelledError
            if not isinstance(rows, list):
                raise GiteaError("Gitea вернула неверный список коммитов.")
            if not rows:
                return
            entries = []
            for row in rows:
                sha = row.get("sha") if isinstance(row, dict) else None
                commit = row.get("commit") if isinstance(row, dict) else None
                message = commit.get("message") if isinstance(commit, dict) else None
                if not isinstance(sha, str) or not OID.fullmatch(sha) or not isinstance(message, str):
                    raise GiteaError("В истории Gitea не хватает SHA или сообщения коммита.")
                if sha in seen:
                    raise GiteaError("Gitea повторила коммит на страницах истории.")
                seen.add(sha)
                entries.append((sha, message))
            report(len(seen))
            yield from entries
        raise GiteaError("Достигнут предел страниц истории Gitea.")

    def commit_paths(self, sha, *, cancelled=None, directory=None):
        if cancelled is not None and cancelled.is_set():
            raise CancelledError
        if not OID.fullmatch(sha):
            raise ValueError("Неверный SHA коммита Gitea.")
        params = {"files": "true", "stat": "false", "verification": "false"}
        row = self._json(f"git/commits/{sha}", params, "Чтение путей коммита")
        if not isinstance(row, dict) or row.get("sha") != sha or not isinstance(row.get("files"), list):
            raise GiteaError("Gitea не подтвердила список файлов коммита.")
        paths = []
        for item in row["files"]:
            path = item.get("filename") if isinstance(item, dict) else None
            if not isinstance(path, str) or "\0" in path or any(part in ("", ".", "..") for part in path.split("/")):
                raise GiteaError("Gitea вернула неверный путь файла коммита.")
            paths.append(path)
        return paths

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
                    entries[name] = Blob(entry_oid, size, mode)
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

    def _file_batch(self, revision, paths):
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


    def files(self, revision, paths):
        # Only Gitea encodes the file list into a GET URL. GitHub uses a POST query.
        result, batch = {}, []
        for path in paths:
            params = {"ref": revision, "body": json.dumps({"files": batch + [path]}, ensure_ascii=False)}
            if batch and (len(batch) >= 20 or len(urlencode(params)) > 4500):
                result.update(self._file_batch(revision, batch))
                batch = []
            batch.append(path)
        if batch:
            result.update(self._file_batch(revision, batch))
        return result
