"""Persist per-user path choices and resolve native dialog start locations."""

from dataclasses import asdict, dataclass, field
import json
import os
from pathlib import Path
from tempfile import NamedTemporaryFile


PATH_FIELDS = ("export", "destination", "platform", "designer", "logs", "index_directory")
BUILD_MODES = ("auto", "ibcmd", "designer")


def normalize_path(value: str) -> str:
    text = value.strip().strip('"')
    return str(Path(text)) if text else ""


def _existing_directory(value: str) -> str:
    if value:
        path = Path(value).absolute()
        try:
            if path.is_dir():
                return str(path)
            if path.is_file():
                return str(path.parent)
        except (OSError, ValueError):
            pass
    return ""


def dialog_location(value: str, last_directory: str = "", *, filename: str | None = None) -> dict[str, str]:
    current = normalize_path(value)
    directory = _existing_directory(current)
    options = {}
    if filename:
        options["initialfile"] = filename
        if current and directory == str(Path(current).absolute().parent):
            options["initialfile"] = Path(current).name
    start = directory or _existing_directory(last_directory)
    if start:
        options["initialdir"] = start
    return options


@dataclass(frozen=True)
class PathPreferences:
    values: dict[str, str] = field(default_factory=dict)
    last_dirs: dict[str, str] = field(default_factory=dict)
    index_directories: dict[str, str] = field(default_factory=dict)
    build_mode: str = "auto"
    source_mode: str = "local"
    git_settings: dict[str, str | int] = field(default_factory=dict)


def _path_fields(data) -> dict[str, str]:
    if not isinstance(data, dict):
        return {}
    return {key: normalize_path(data[key]) for key in PATH_FIELDS if isinstance(data.get(key), str)}


class PathSettingsFile:
    def __init__(self, path: Path):
        self.path = path

    @classmethod
    def for_user(cls):
        folder = os.environ.get("LOCALAPPDATA")
        directory = Path(folder) / "CF-Wizard" if folder else Path.home() / ".cf-wizard"
        return cls(directory / "paths.json")

    def load(self) -> PathPreferences:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return PathPreferences()
        if not isinstance(data, dict):
            return PathPreferences()
        indexes = data.get("index_directories")
        directories = {}
        if isinstance(indexes, dict):
            directories = {key: normalize_path(value) for key, value in indexes.items() if isinstance(value, str)}
        mode = data.get("build_mode")
        source = "git" if data.get("source_mode") == "git" else "local"
        stored_git = data.get("git_settings")
        git = {}
        if isinstance(stored_git, dict):
            git = {key: stored_git[key] for key in ("provider", "server", "repository", "directory", "branch", "cache")
                   if isinstance(stored_git.get(key), str)}
            seconds = stored_git.get("interval")
            if type(seconds) is int and (seconds == 0 or 30 <= seconds <= 86400):
                git["interval"] = seconds
        return PathPreferences(_path_fields(data.get("values")), _path_fields(data.get("last_dirs")), directories,
                               mode if mode in BUILD_MODES else "auto", source, git)

    def save(self, preferences: PathPreferences) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        data = json.dumps(asdict(preferences), ensure_ascii=False, indent=2) + "\n"
        draft = None
        try:
            with NamedTemporaryFile("w", encoding="utf-8", dir=self.path.parent,
                                    prefix=".paths-", delete=False) as file:
                draft = Path(file.name)
                file.write(data)
            draft.replace(self.path)
        finally:
            if draft is not None:
                draft.unlink(missing_ok=True)
