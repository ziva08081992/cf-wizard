"""Compare 1C export indexes and prepare the developer's export plan."""

from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from .application import _entry_path


INDEX_NAME = "ConfigDumpInfo.xml"
EXPORT_LIST = "Выгрузить объекты по списку"
CHECK_UPDATE = "Проверить инкрементальное обновление"


@dataclass(frozen=True, slots=True)
class IndexEntry:
    identity: str
    version: str | None


@dataclass(frozen=True)
class IndexSnapshot:
    path: Path
    format_version: str
    configuration: str
    sha256: str
    entries: dict[str, IndexEntry]


@dataclass(frozen=True)
class IndexChange:
    kind: str
    before: str | None
    after: str | None


@dataclass(frozen=True)
class IndexDiff:
    changes: tuple[IndexChange, ...]
    dump_objects: tuple[str, ...]


@dataclass(frozen=True)
class IndexExport:
    index: Path
    arguments: str


@dataclass(frozen=True)
class UpdateCheck:
    working_folder: Path
    index_sha256: str
    changes: Path
    result: Path
    log: Path


@dataclass(frozen=True)
class PreparedDiff:
    directory: Path
    diff: IndexDiff
    commands: dict[str, str]
    update_check: UpdateCheck | None
    object_list: Path


class IndexFiles(Protocol):
    def read(self, path: Path) -> IndexSnapshot: ...

    def first_export(self, working_folder: Path) -> str: ...

    def update_arguments(self, check: UpdateCheck) -> str | None: ...

    def prepare_export(self, working_folder: Path, destination: Path) -> IndexExport: ...

    def save_diff(self, working_folder: Path, destination: Path, old: IndexSnapshot,
                  fresh: IndexSnapshot, diff: IndexDiff) -> PreparedDiff: ...


class IndexWizard:
    def __init__(self, files: IndexFiles):
        self.files = files

    def first_export(self, working_folder: str | Path) -> str:
        folder = _entry_path(str(working_folder), "Выберите папку XML-выгрузки.")
        return self.files.first_export(folder)

    def prepare_export(self, working_folder: str | Path, destination: str) -> IndexExport:
        folder = _entry_path(str(working_folder), "Выберите папку XML-выгрузки.")
        return self.files.prepare_export(folder, _entry_path(destination, "Выберите папку для выгрузки индекса."))

    def compare(self, working_folder: str | Path, destination: str, baseline: str, current: str) -> PreparedDiff:
        folder = _entry_path(str(working_folder), "Выберите папку XML-выгрузки.")
        output = _entry_path(destination, "Укажите папку для отчётов.")
        old = self.files.read(_entry_path(baseline, "Укажите индекс рабочей XML-папки."))
        fresh = self.files.read(_entry_path(current, "Укажите индекс из базы."))
        return self.files.save_diff(folder, output, old, fresh, compare_indexes(old, fresh))

    def update_arguments(self, plan: PreparedDiff) -> str | None:
        if plan.update_check is None:
            raise ValueError("Для обновления сравните индексы с ConfigDumpInfo.xml из рабочей XML-папки.")
        return self.files.update_arguments(plan.update_check)


STANDALONE = frozenset(("Form", "Template", "Command", "Subsystem", "Table"))


def _owner(address: str) -> str:
    parts = address.split(".")
    end = 2
    while end + 1 < len(parts) and parts[end] in STANDALONE:
        end += 2
    return ".".join(parts[:end])


def _by_identity(entries: dict[str, IndexEntry], names: set[str]) -> dict[str, list[str]]:
    result = defaultdict(list)
    for name in names:
        result[entries[name].identity].append(name)
    return result


def _changes(old: dict[str, IndexEntry], fresh: dict[str, IndexEntry]) -> tuple[IndexChange, ...]:
    removed, added = old.keys() - fresh.keys(), fresh.keys() - old.keys()
    ids = {old[name].identity for name in removed} & {fresh[name].identity for name in added}
    previous = _by_identity(old, {name for name, entry in old.items() if entry.identity in ids})
    current = _by_identity(fresh, {name for name, entry in fresh.items() if entry.identity in ids})
    changes = []
    for identity in ids:
        if len(previous[identity]) == len(current[identity]) == 1:
            before, after = previous[identity][0], current[identity][0]
            if before.partition(".")[0] == after.partition(".")[0]:
                changes.append(IndexChange("renamed", before, after))
                removed.remove(before)
                added.remove(after)
    changes.extend(IndexChange("removed", name, None) for name in removed)
    changes.extend(IndexChange("added", None, name) for name in added)
    changes.extend(IndexChange("changed", name, name) for name in old.keys() & fresh.keys()
                   if old[name] != fresh[name])
    return tuple(sorted(changes, key=lambda change: change.after or change.before))


def _dump_objects(fresh: IndexSnapshot, changes: tuple[IndexChange, ...]) -> tuple[str, ...]:
    entries = fresh.entries
    candidates = set()
    for change in changes:
        name = change.after
        if name is None:
            continue
        owner = _owner(name)
        candidate = name if entries[name].version else owner
        if candidate not in entries or not entries[candidate].version:
            raise ValueError(f"Для {name} в свежем индексе отсутствует адрес выгрузки.")
        candidates.add(candidate)
        if change.kind in ("added", "renamed") and name == owner and name != fresh.configuration:
            # Adding a standalone object also changes the parent's ChildObjects composition.
            parent = name.rsplit(".", 2)[0] if name.count(".") > 1 else fresh.configuration
            if parent not in entries or not entries[parent].version:
                raise ValueError(f"В свежем индексе отсутствует описание владельца {name}.")
            candidates.add(parent)
    return tuple(sorted(name for name in candidates if name == _owner(name) or _owner(name) not in candidates))


def compare_indexes(old: IndexSnapshot, fresh: IndexSnapshot) -> IndexDiff:
    if old.format_version != fresh.format_version:
        raise ValueError(f"Версии формата индексов различаются ({old.format_version} и {fresh.format_version}).")
    if old.entries[old.configuration].identity != fresh.entries[fresh.configuration].identity:
        raise ValueError("Индексы относятся к разным конфигурациям. Сверьте UUID корня.")
    changes = _changes(old.entries, fresh.entries)
    return IndexDiff(changes, _dump_objects(fresh, changes))
