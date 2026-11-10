"""Designer XML index input, comparison reports and manual startup keys."""

from dataclasses import asdict
import hashlib
import io
import json
from pathlib import Path
import re
import tempfile
import xml.etree.ElementTree as ET

from stage import _no_links, _sha

from .adapters import _xml
from .dump_indexes import (
    CHECK_UPDATE, EXPORT_LIST, INDEX_NAME, IndexDiff, IndexEntry, IndexExport, IndexSnapshot, PreparedDiff, UpdateCheck,
)


NAMESPACE = "{http://v8.1c.ru/8.3/xcf/dumpinfo}"
CHANGE_LINE = re.compile(r"(?:New|Modified|Deleted|Removed):\s*(\S+)")


def _quote_path(path: Path) -> str:
    value = str(path)
    trailing = len(value) - len(value.rstrip("\\"))
    return '"' + value + "\\" * trailing + '"'


def _dump_arguments(folder: Path, option: str = "", value: Path | None = None) -> str:
    arguments = f"/DumpConfigToFiles {_quote_path(folder)} -Format Hierarchical"
    if option:
        arguments += " " + option
    return arguments if value is None else f"{arguments} {_quote_path(value)}"


def _with_diagnostics(arguments: str, log: Path, result: Path) -> str:
    return f"{arguments} /Out {_quote_path(log)} /DumpResult {_quote_path(result)}"


def _existing_folder(folder: Path) -> Path:
    _no_links(folder.absolute())
    folder = folder.resolve()
    if not folder.is_dir():
        raise ValueError(f"Папка не найдена. {folder}")
    if any(part.casefold() == ".git" for part in folder.parts):
        raise ValueError("Выберите папку вне .git.")
    return folder


def _outside_working_folder(working_folder: Path, destination: Path) -> Path:
    folder = _existing_folder(destination)
    if folder == working_folder or working_folder in folder.parents:
        raise ValueError("Выберите папку вне рабочей XML-выгрузки.")
    return folder


def _new_run(working_folder: Path, destination: Path) -> Path:
    return Path(tempfile.mkdtemp(prefix="CF-index-", dir=_outside_working_folder(working_folder, destination)))


class DesignerIndexFiles:
    def read(self, path: Path) -> IndexSnapshot:
        _no_links(path.absolute())
        path = path.resolve()
        if not path.is_file():
            raise ValueError(f"Индекс выгрузки не найден: {path}")
        data = path.read_bytes()
        if b"<!DOCTYPE" in data.upper():
            raise ValueError(f"Индекс с DTD не поддерживается: {path}")
        nodes = ET.iterparse(io.BytesIO(data), events=("start", "end"))
        _, root = next(nodes)
        version = root.get("version")
        if root.tag != NAMESPACE + "ConfigDumpInfo" or root.get("format") != "Hierarchical" or not version:
            raise ValueError(f"Ожидался ConfigDumpInfo.xml в формате Hierarchical: {path}")

        entries = {}
        for event, node in nodes:
            if event == "end":
                node.clear()
                continue
            if node.tag != NAMESPACE + "Metadata":
                continue
            name, identity = node.get("name", ""), node.get("id", "")
            parts = name.split(".")
            if len(parts) < 2 or not identity or any(not part.isidentifier() for part in parts):
                raise ValueError(f"Некорректная запись метаданных в индексе: {name or path}")
            entry = IndexEntry(identity, node.get("configVersion"))
            if entries.setdefault(name, entry) != entry:
                raise ValueError(f"В индексе разные версии одного адреса: {name}")
        configurations = [name for name in entries if name.startswith("Configuration.") and name.count(".") == 1]
        if len(configurations) != 1 or not entries[configurations[0]].version:
            raise ValueError(f"В индексе должен быть один корень основной конфигурации: {path}")
        return IndexSnapshot(path, version, configurations[0], hashlib.sha256(data).hexdigest(), entries)

    def first_export(self, working_folder: Path) -> str:
        folder = _existing_folder(working_folder)
        if any(folder.iterdir()):
            raise ValueError("Для первой полной выгрузки выберите пустую папку.")
        return _dump_arguments(folder)

    def prepare_export(self, working_folder: Path, destination: Path) -> IndexExport:
        folder = _outside_working_folder(_existing_folder(working_folder), destination)
        if any(folder.iterdir()):
            raise ValueError("Для выгрузки индекса из базы выберите пустую папку.")
        return IndexExport(folder / INDEX_NAME, _dump_arguments(folder, "-configDumpInfoOnly"))

    def save_diff(self, working_folder: Path, destination: Path, old: IndexSnapshot,
                  fresh: IndexSnapshot, diff: IndexDiff) -> PreparedDiff:
        working_folder = _existing_folder(working_folder)
        working_index = old.path == working_folder / INDEX_NAME
        if working_index:
            root = _xml(working_folder / "Configuration.xml").find("{*}Configuration")
            if root is None or root.get("uuid") != old.entries[old.configuration].identity:
                raise ValueError("Индекс рабочей папки и Configuration.xml относятся к разным конфигурациям.")
        run = _new_run(working_folder, destination)
        logs = run / "logs"
        logs.mkdir()
        objects = run / "dump-objects.txt"
        objects.write_text("".join(name + "\n" for name in diff.dump_objects), encoding="utf-8-sig")
        commands = {}
        if diff.dump_objects:
            delta = run / "delta"
            delta.mkdir()
            arguments = _dump_arguments(delta, "-listFile", objects)
            commands[EXPORT_LIST] = _with_diagnostics(arguments, logs / "list-file.log", logs / "list-file.result.txt")
        check = None
        if diff.changes and working_index:
            check = UpdateCheck(working_folder, old.sha256, logs / "platform-changes.txt",
                                logs / "get-changes.result.txt", logs / "get-changes.log")
            arguments = _dump_arguments(working_folder, "-getChanges", check.changes)
            commands[CHECK_UPDATE] = _with_diagnostics(arguments, check.log, check.result)
        report = {
            "working_folder": str(working_folder),
            "baseline": {"file": str(old.path), "sha256": old.sha256},
            "current": {"file": str(fresh.path), "sha256": fresh.sha256},
            "format_version": fresh.format_version,
            "configuration": fresh.configuration,
            "changes": [
                {"kind": change.kind, "before": change.before, "after": change.after,
                 "old_entry": asdict(old.entries[change.before]) if change.before else None,
                 "new_entry": asdict(fresh.entries[change.after]) if change.after else None}
                for change in diff.changes
            ],
            "dump_objects": diff.dump_objects,
            "commands": commands,
            "platform_check": {name: str(getattr(check, name)) for name in ("changes", "result", "log")} if check else None,
        }
        (run / "changes.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return PreparedDiff(run, diff, commands, check, objects)

    def update_arguments(self, check: UpdateCheck) -> str | None:
        index = check.working_folder / INDEX_NAME
        if not index.is_file() or _sha(index) != check.index_sha256:
            raise ValueError("Индекс рабочей папки изменился. Сравните индексы заново.")
        for path in (check.changes, check.result):
            _no_links(path)

        lines = None
        if check.changes.is_file():
            lines = [line.strip() for line in check.changes.read_text(encoding="utf-8-sig").splitlines()
                     if line.strip()]
            if any(line.casefold() == "fulldump" for line in lines):
                raise ValueError("Конфигуратор вернул FullDump. Ключ -update заблокирован.")
        if not check.result.is_file():
            raise ValueError(f"Выполните ключ -getChanges и дождитесь файла результата {check.result}")
        raw = check.result.read_bytes()
        encoding = "utf-16" if raw.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig"
        code = raw.decode(encoding).strip()
        if code != "0":
            raise ValueError(f"Конфигуратор вернул код {code or 'без значения'}. Проверьте журнал {check.log}")
        if lines is None:
            raise ValueError(f"Отчёт -getChanges не найден. {check.changes}")
        for line in lines:
            match = CHANGE_LINE.fullmatch(line)
            if not match or len(match[1].split(".")) < 2 or any(not part.isidentifier() for part in match[1].split(".")):
                raise ValueError(f"Формат ответа 1С не распознан. {check.changes}")
        if not lines:
            return None
        logs = check.log.parent
        arguments = _dump_arguments(check.working_folder, "-update")
        return _with_diagnostics(arguments, logs / "update.log", logs / "update.result.txt")
