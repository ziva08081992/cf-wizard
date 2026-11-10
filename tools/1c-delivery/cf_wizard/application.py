"""Use cases and ports for choosing objects and building a technical CF."""

from collections.abc import Iterable
from dataclasses import dataclass, field, replace
import os
from pathlib import Path
import re
from typing import Protocol

from .composition import PackageComposition, Stub


@dataclass(frozen=True)
class ExportCatalog:
    root: Path
    configuration: str
    objects: tuple[str, ...]
    git: object | None = field(default=None, compare=False, repr=False)

    @property
    def identity(self) -> str:
        return os.path.normcase(str(self.root))

    @property
    def preference_key(self) -> str:
        return self.git.preference_key if self.git is not None else self.identity

    def with_git(self, files):
        return replace(self, git=files)


@dataclass(frozen=True)
class IndexUpdate:
    source: ExportCatalog
    path: Path
    added: int
    changed: int
    removed: int
    unchanged: int
    log: Path


@dataclass(frozen=True)
class Preview:
    task: str | None
    source: ExportCatalog
    composition: PackageComposition
    warnings: tuple[str, ...]
    snapshot: str
    destination: Path | None
    log: Path
    index: Path | None = None

    @property
    def selected(self) -> tuple[str, ...]:
        return self.composition.selected

    @property
    def dependencies(self) -> tuple[Stub, ...]:
        return self.composition.stubs


class JournaledError(ValueError):
    def __init__(self, message: str, log: Path):
        super().__init__(f"{message}\nЖурнал {log}")
        self.log = log


class PreviewError(JournaledError):
    pass


class ExportReader(Protocol):
    def read(self, path: Path) -> ExportCatalog: ...


class PackageBuilder(Protocol):
    def prepare(self, source: ExportCatalog, task: str | None, selected: tuple[str, ...],
                destination: Path | None, log_directory: Path | None = None, index: Path | None = None) -> Preview: ...

    def build(self, preview: Preview, platform: Path, excluded: frozenset[str]) -> Path: ...

    def discard(self, preview: Preview) -> None: ...


class DependencyIndexes(Protocol):
    def path_for(self, source: ExportCatalog, directory: Path | None) -> Path: ...

    def refresh(self, source: ExportCatalog, directory: Path | None,
                log_directory: Path | None = None) -> IndexUpdate: ...


def _entry_path(value: str, message: str) -> Path:
    path = value.strip().strip('"')
    if not path:
        raise ValueError(message)
    return Path(path)


def _task_label(value: str) -> str | None:
    text = value.strip()
    if re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,15}-[0-9]{1,12}", text):
        text = text.upper()
    return text or None


class Wizard:
    def __init__(self, reader: ExportReader, builder: PackageBuilder, indexes: DependencyIndexes | None = None):
        self.reader = reader
        self.builder = builder
        self.indexes = indexes

    def open_export(self, path: str) -> ExportCatalog:
        return self.reader.read(_entry_path(path, "Укажите папку XML-выгрузки или файл Configuration.xml."))

    def index_path(self, source: ExportCatalog, directory: str) -> Path | None:
        text = directory.strip().strip('"')
        return self.indexes.path_for(source, Path(text) if text else None) if self.indexes else None

    def refresh_index(self, source: ExportCatalog, directory: str, logs: str = "") -> IndexUpdate:
        if self.indexes is None:
            raise ValueError("Индексы зависимостей недоступны.")
        text = directory.strip().strip('"')
        log_directory = logs.strip().strip('"')
        return self.indexes.refresh(source, Path(text) if text else None,
                                    Path(log_directory) if log_directory else None)

    def preview(self, source: ExportCatalog, task: str, selected: set[str],
                destination: str, logs: str = "", index_directory: str = "") -> Preview:
        if not selected or not selected.issubset(source.objects):
            raise ValueError("Отметьте объекты из открытой выгрузки.")
        output = destination.strip().strip('"')
        log_directory = logs.strip().strip('"')
        try:
            index = self.index_path(source, index_directory)
        except (OSError, ValueError):
            index = None
        return self.builder.prepare(source, _task_label(task), tuple(sorted(selected)), Path(output) if output else None,
                                    Path(log_directory) if log_directory else None, index)

    def build(self, preview: Preview, platform: str, excluded: Iterable[str] = (), *,
              destination: str | None = None, task: str | None = None) -> Path:
        try:
            path = _entry_path(platform, "Укажите путь к исполняемому файлу платформы 1С.")
            output = preview.destination if destination is None else _entry_path(destination, "Укажите папку для результата.")
            if output is None:
                raise ValueError("Укажите папку для результата.")
            omission = preview.composition.exclusions(excluded)
        except ValueError:
            self.discard(preview)
            raise
        current = replace(preview, destination=output, task=preview.task if task is None else _task_label(task))
        return self.builder.build(current, path, omission)

    def discard(self, preview: Preview) -> None:
        self.builder.discard(preview)
