"""Provider-neutral Git contracts, credential identity and safe HTTP transport."""
from dataclasses import dataclass
from hashlib import new as hash_new, sha256
from http.client import HTTPException
import re
from urllib.error import HTTPError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, Request

DEFAULT_GITEA = {
    "provider": "gitea", "server": "", "repository": "",
    "directory": "config", "branch": "", "interval": 60,
}
OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
MAX_COMMIT_PAGES = 10_000
DEVICE = re.compile(r"(?:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?\Z", re.I)


def relative_path(value):
    if not isinstance(value, str) or any(
        not part or part in (".", "..") or part.endswith((" ", ".")) or DEVICE.fullmatch(part)
        or any(ord(ch) < 32 or ch in '<>:"\\|?*' for ch in part)
        for part in value.split("/")
    ):
        raise ValueError("Недопустимый путь файла в Git.")
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
        raise ValueError("Укажите базовый HTTPS-адрес Git-сервиса.")
    return urlunsplit(("https", url.netloc.lower(), url.path, "", ""))


def _key(*parts):
    return sha256("\0".join(parts).encode("utf-8")).hexdigest()[:24]


def provider_name(value):
    if value not in ("gitea", "github"):
        raise ValueError("Выберите API Gitea или GitHub.")
    return value


def connection_server(server, provider="gitea"):
    provider_name(provider)
    address = server_address(server)
    if provider == "github":
        if address not in ("https://github.com", "https://api.github.com"):
            raise ValueError("Для GitHub укажите https://github.com. GitHub Enterprise пока не поддерживается.")
        return "https://github.com"
    return address


def credential_target(server, provider="gitea"):
    address = connection_server(server, provider)
    label = "Gitea" if provider == "gitea" else "GitHub"
    return "CF-Wizard/" + label + "/" + _key(address)


def create_client(settings, token):
    provider = provider_name(settings.get("provider", "gitea"))
    server = connection_server(settings["server"], provider)
    if provider == "github":
        from .github import GitHubClient
        return GitHubClient(server, settings["repository"], token)
    from .gitea import GiteaClient
    return GiteaClient(server, settings["repository"], token)


class GitError(ValueError):
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
    mode: str = "100644"

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


# Preserve the old exception identity for existing integrations.
GiteaError = GitError


class GitTransport:
    """Header-only authentication, fixed API origin, and no redirect forwarding."""

    def _request(self, url, operation, *, limit=None, data=None, headers=None):
        if urlsplit(url)[:2] != urlsplit(self._base)[:2]:
            raise GitError("Запрос за пределы выбранного Git API запрещён.")
        request = Request(url, data=data, headers={
            **getattr(self, "_headers", {}), **(headers or {}),
            "Authorization": f"{self._auth_scheme} {self._token}",
        }, method="POST" if data is not None else "GET")
        try:
            with self._opener.open(request, timeout=20) as response:
                return response.read() if limit is None else response.read(limit)
        except HTTPError as error:
            code = error.code
            wait = error.headers.get("Retry-After", "") if error.headers else ""
            error.close()
            retry = f" Повторите через {int(wait)} сек." if wait.isascii() and wait.isdigit() and len(wait) < 6 else ""
            hint = " Лимит API или доступ запрещены." if code in (403, 429) else ""
            raise GitError(f"{self.label} вернула HTTP {code}. {operation}.{hint}{retry}", code) from None
        except (OSError, HTTPException, ValueError):
            raise GitError(f"Не удалось прочитать {self.label}. {operation}.") from None
