"""Designer XML, the pinned source-only helper and isolated platform builds."""

import ctypes
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import runpy
import shutil
import stat
import struct
import sys
import tempfile
from time import perf_counter
import xml.etree.ElementTree as ET

import native
from stage import HELPER, _inventory, _no_links, _sha, metadata_coverage

from .application import ExportCatalog, IndexUpdate, JournaledError, Preview, PreviewError
from .composition import DependencyCause, PackageComposition, Stub
from .dependency_index import IndexReader, descriptor_inventory, update_git_index, update_index
from .helper_worker import DependencyPlan, HELPER_SHA256, resolve_dependencies
from .journals import create_journal, finish_workspace, journal_directory, write_journal
from .windows_process import run_installed_platform


BUNDLED_HELPER_ENTRY = "vendor/partial_cf.py"
WORKSPACE_HELPER = Path(os.path.abspath(Path(__file__).parent / "../../.." / HELPER))
BUNDLED_HELPER = Path(__file__).resolve().parents[1] / BUNDLED_HELPER_ENTRY
DEFAULT_HELPER = BUNDLED_HELPER if BUNDLED_HELPER.is_file() else WORKSPACE_HELPER
TRANSFER_SUBSYSTEM = "Subsystem._Перенос"
PROJECT_PLATFORM_VERSION = "8.3.27.2214"
PLATFORM_VERSION = re.compile(r"\d+\.\d+\.\d+\.\d+")
BUILD_EXECUTABLES = {"ibcmd": "ibcmd.exe", "designer": "1cv8.exe"}
BUILD_LABELS = {"ibcmd": "ibcmd", "designer": "Конфигуратор"}


def _platform_roots() -> tuple[Path, ...]:
    return tuple(dict.fromkeys(
        Path(directory) / "1cv8"
        for variable in ("ProgramW6432", "ProgramFiles", "ProgramFiles(x86)")
        if (directory := os.environ.get(variable))
    ))


def _platform_candidates(filename: str) -> tuple[Path, ...]:
    return tuple(
        binary for root in _platform_roots() for binary in sorted(root.glob(f"*/bin/{filename}"))
        if PLATFORM_VERSION.fullmatch(binary.parent.parent.name) and binary.is_file()
    )


def preferred_platform(candidates: tuple[Path, ...], mode: str) -> Path | None:
    name = BUILD_EXECUTABLES["designer" if mode == "auto" else mode]
    return next((path for path in candidates
                 if path.name.casefold() == name and path.parent.parent.name == PROJECT_PLATFORM_VERSION), None)


def discover_ibcmd() -> tuple[tuple[Path, ...], Path | None]:
    candidates = _platform_candidates(BUILD_EXECUTABLES["ibcmd"])
    return candidates, preferred_platform(candidates, "ibcmd")


def discover_build_platforms(saved: tuple[str, ...] = ()) -> tuple[Path, ...]:
    candidates = dict.fromkeys(
        [Path(value) for value in saved if value]
        + [path for filename in BUILD_EXECUTABLES.values() for path in _platform_candidates(filename)]
    )
    installed = []
    for path in candidates:
        try:
            _validate_platform(path)
        except (OSError, ValueError):
            continue
        installed.append(path)
    return tuple(installed)


def _windows_binary_build(path: Path) -> tuple[str, int] | None:
    """Read the fixed file version and PE machine type without starting the binary."""
    if os.name != "nt":
        return None
    try:
        with path.open("rb") as binary:
            if binary.read(2) != b"MZ":
                return None
            binary.seek(0x3C)
            pe_offset, = struct.unpack("<I", binary.read(4))
            binary.seek(pe_offset)
            signature, machine = struct.unpack("<4sH", binary.read(6))
    except (OSError, struct.error):
        return None
    if signature != b"PE\0\0":
        return None

    version_api = ctypes.windll.version
    filename = ctypes.c_wchar_p(str(path))
    size = version_api.GetFileVersionInfoSizeW(filename, None)
    if not size:
        return None
    data = ctypes.create_string_buffer(size)
    if not version_api.GetFileVersionInfoW(filename, 0, size, data):
        return None
    value, length = ctypes.c_void_p(), ctypes.c_uint()
    if not version_api.VerQueryValueW(data, ctypes.c_wchar_p("\\"), ctypes.byref(value), ctypes.byref(length)):
        return None
    if length.value < 16:
        return None
    signature, _, high, low = struct.unpack("<IIII", ctypes.string_at(value, 16))
    if signature != 0xFEEF04BD:
        return None
    version = f"{high >> 16}.{high & 0xFFFF}.{low >> 16}.{low & 0xFFFF}"
    return version, machine


def _validate_platform(path: Path) -> tuple[str, str]:
    backend = next((method for method, filename in BUILD_EXECUTABLES.items()
                    if filename == path.name.casefold()), None)
    version = path.parent.parent.name
    if (backend is None or not path.is_file() or path.parent.name.casefold() != "bin"
            or not PLATFORM_VERSION.fullmatch(version)):
        raise ValueError("Выберите ibcmd.exe или 1cv8.exe из папки bin установленной платформы 1С.")
    build = _windows_binary_build(path)
    if build is None or build[0] != version or _windows_binary_build(path.parent / "core83.dll") != build:
        raise ValueError(f"Версия и разрядность {path.name} и core83.dll должны совпадать в папке bin версии {version}.")
    return backend, version


def _xml(path: Path) -> ET.Element:
    _no_links(path)
    data = path.read_bytes()
    if b"<!DOCTYPE" in data.upper():
        raise ValueError(f"XML с DTD не поддерживается: {path}")
    return ET.fromstring(data)


def _digest(values: dict) -> str:
    return hashlib.sha256(json.dumps(values, sort_keys=True).encode("utf-8")).hexdigest()


def _source_inventory(root: Path) -> dict[str, tuple[int, int, int]]:
    """Capture the file tree without reading unrelated content over the network."""
    _no_links(root)
    files = {}
    pending = [(root, "")]
    while pending:
        directory, prefix = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                info = entry.stat(follow_symlinks=False)
                relative = prefix + entry.name
                if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 1024:
                    raise ValueError(f"Ссылка или точка повторного анализа в выгрузке: {relative}")
                if stat.S_ISDIR(info.st_mode):
                    pending.append((entry.path, relative + "/"))
                elif stat.S_ISREG(info.st_mode):
                    files[relative] = (info.st_size, info.st_mtime_ns, info.st_ctime_ns)
                else:
                    raise ValueError(f"Ожидался обычный файл: {relative}")
    return files


class DesignerExport:
    def __init__(self, helper: Path = DEFAULT_HELPER):
        self.helper = Path(os.path.abspath(helper))
        self.verify_helper()
        self.folders = runpy.run_path(str(self.helper))["TYPE_FOLDERS"]

    def verify_helper(self) -> None:
        if not self.helper.is_file() or _sha(self.helper) != HELPER_SHA256:
            raise ValueError("Укажите закреплённую проектом версию partial_cf.py.")

    def read(self, path: Path) -> ExportCatalog:
        return self.describe(path)[0]

    def describe(self, path: Path) -> tuple[ExportCatalog, dict[str, str]]:
        root = Path(os.path.abspath(path))
        _no_links(root)
        if root.is_file() and root.name.casefold() == "configuration.xml":
            root = root.parent
        if not root.is_dir() or not (root / "Configuration.xml").is_file():
            raise ValueError("Нужна папка Designer XML или её файл Configuration.xml.")
        root = root.resolve()
        configuration = _xml(root / "Configuration.xml").find("{*}Configuration")
        if configuration is None:
            raise ValueError("Файл Configuration.xml не описывает конфигурацию.")
        if configuration.find("{*}Properties/{*}ConfigurationExtensionPurpose") is not None:
            raise ValueError("Это выгрузка расширения. Мастер принимает XML-выгрузку основной конфигурации для сборки CF.")
        name = configuration.findtext("{*}Properties/{*}Name", "")
        children = configuration.find("{*}ChildObjects")
        if not name.isidentifier() or children is None:
            raise ValueError("В Configuration.xml отсутствует имя или состав конфигурации.")
        descriptors = {}
        for child in children:
            kind, identifier = child.tag.rsplit("}", 1)[-1], child.text or ""
            if kind not in self.folders or not identifier.isidentifier():
                raise ValueError(f"Неподдерживаемое описание объекта: {kind}.{identifier}")
            key = f"{kind}.{identifier}"
            if key in descriptors:
                raise ValueError(f"Повтор объекта в Configuration.xml: {key}")
            descriptors[key] = f"{self.folders[kind]}/{identifier}.xml"
        if TRANSFER_SUBSYSTEM in descriptors:
            raise ValueError("В исходной выгрузке уже есть подсистема _Перенос.")
        objects = tuple(sorted(key for key in descriptors if not key.startswith("Language.")))
        if not objects:
            raise ValueError("В выгрузке нет объектов для переноса.")
        return ExportCatalog(root, name, objects, descriptors=tuple(sorted(descriptors.items()))), descriptors

    def objects_for_paths(self, source: ExportCatalog, paths: set[str]) -> tuple[frozenset[str], int]:
        current, descriptors = self.describe(source.root)
        if current != source:
            raise ValueError("Состав открытой XML-выгрузки изменился. Откройте ветку снова.")
        owners = {descriptors[key]: key for key in current.objects}
        objects = set()
        unmapped = 0
        for path in paths:
            owner = owners.get(_owner_descriptor(path))
            if owner is None:
                unmapped += 1
            else:
                objects.add(owner)
        return frozenset(objects), unmapped


class DesignerDependencyIndexes:
    def __init__(self, reader: DesignerExport, default_directory: Path, default_logs: Path):
        self.reader = reader
        self.default_directory = default_directory
        self.default_logs = default_logs

    def path_for(self, source: ExportCatalog, directory: Path | None) -> Path:
        folder = (directory or self.default_directory).absolute()
        _no_links(folder)
        folder = folder.resolve()
        if folder == source.root or source.root in folder.parents or any(part.casefold() == ".git" for part in folder.parts):
            raise ValueError("Укажите папку индекса вне XML-выгрузки и .git.")
        identity = hashlib.sha256(source.preference_key.encode("utf-8")).hexdigest()[:20]
        path = folder / f"{source.configuration[:64]}-{identity}.sqlite3"
        _no_links(path)
        return path

    def refresh(self, source: ExportCatalog, directory: Path | None,
                log_directory: Path | None = None) -> IndexUpdate:
        started = perf_counter()
        log = create_journal(journal_directory(log_directory or self.default_logs, source.root))
        git = source.git
        origin = (f"{getattr(git.client, 'label', 'Gitea')} {git.client.repository}/{git.directory}, ветка {git.branch}, SHA {git.revision}"
                  if git is not None else f"локальная XML-выгрузка {source.root}")
        write_journal(log, f"Подготовка индекса зависимостей для {source.configuration}. Источник {origin}.")
        try:
            if git is not None:
                inputs = git.index_inputs
                current, descriptors = source, dict(source.descriptors)
            else:
                configuration = source.root / "Configuration.xml"
                original = _sha(configuration)
                current, descriptors = self.reader.describe(source.root)
            path = self.path_for(current, directory)
            action = "Обновление" if path.is_file() else "Создание"
            write_journal(log, f"{action} индекса {path}.")
            self.reader.verify_helper()
            helper = runpy.run_path(str(self.reader.helper))

            def source_check():
                if git is not None:
                    if git.index_inputs != inputs or git.revision != inputs.revision:
                        raise ValueError("Открытая ревизия Git изменилась. Откройте ветку заново.")
                elif _sha(configuration) != original:
                    raise ValueError("Состав XML-выгрузки изменился во время обновления индекса. Повторите операцию.")
                self.reader.verify_helper()

            if git is not None:
                write_journal(log, f"Подготовка XML-описаний из {getattr(source.git.client, 'label', 'Gitea')} и кэша.")
            path.parent.mkdir(parents=True, exist_ok=True)
            write_journal(log, f"Обработка XML и запись SQLite. Объектов в каталоге {len(descriptors)}.")
            if git is not None:
                counts = update_git_index(path, current, descriptors, helper, HELPER_SHA256, source_check)
            else:
                counts = update_index(path, current.root, current.identity, descriptors, helper, HELPER_SHA256, source_check)
        except Exception as exc:
            write_journal(log, f"Ошибка подготовки индекса после {perf_counter() - started:.2f} с. {exc}")
            raise JournaledError(str(exc), log) from exc

        update = IndexUpdate(current, path, counts["added"], counts["changed"], counts["removed"], counts["unchanged"], log)
        write_journal(log, f"Индекс готов за {perf_counter() - started:.2f} с. Добавлено {update.added}, "
                          f"изменено {update.changed}, удалено {update.removed}, переиспользовано {update.unchanged}.")
        if git is not None:
            write_journal(log, f"Входов {counts['inputs']}, пропущено неизменных XML {counts['reused_inputs']}, "
                              f"обработано XML {counts['xml_reads']}, "
                              f"извлечено фактов {counts['facts']}.")
            write_journal(log, f"XML-входы: добавлено {counts['input_added']}, изменено {counts['input_changed']}, "
                              f"удалено {counts['input_removed']}. Счётчик обработки не включает повторные чтения кэша и проверки Git hash.")
        return update


def _object_uuid(path: Path, key: str) -> str:
    kind = key.partition(".")[0]
    descriptor = _xml(path).find(f"{{*}}{kind}")
    value = None if descriptor is None else descriptor.get("uuid")
    if not value:
        raise ValueError(f"В описании {key} отсутствует UUID: {path}")
    return value


def _verify_merge_settings(path: Path, configuration: str, selected: tuple[str, ...],
                           dependencies: tuple[Stub, ...]) -> None:
    settings = _xml(path)
    if settings.findtext("{*}Parameters/{*}AllowMainConfigurationObjectDeletion") != "false":
        raise ValueError("Настройки объединения должны запрещать удаление объектов получателя.")
    expected = {f"Configuration.{configuration}": "DoNotMerge", TRANSFER_SUBSYSTEM: "DoNotMerge"}
    expected.update(dict.fromkeys(selected, "GetFromSecondConfiguration"))
    stubs = {item.name for item in dependencies}
    expected.update(dict.fromkeys(stubs, "MergePrioritizingMainConfiguration"))
    seen = set()
    for obj in settings.findall("{*}Objects/{*}Object"):
        name = obj.get("fullNameInSecondConfiguration", "")
        rule = obj.findtext("{*}MergeRule")
        parts = name.split(".")
        owner = ".".join(parts[:2])
        if name in seen:
            raise ValueError(f"Повтор в настройках объединения: {name}")
        seen.add(name)
        expected_rule = expected.get(name)
        if expected_rule is None and len(parts) == 4 and owner in stubs and parts[2] in ("Form", "Command"):
            expected_rule = "MergePrioritizingMainConfiguration"
        if expected_rule is None or rule != expected_rule or any(
                prop.findtext("{*}MergeRule") != "DoNotMerge" for prop in obj.findall("{*}Properties/{*}Property")):
            raise ValueError(f"Некорректное правило объединения для {name}.")
    if not expected.keys() <= seen:
        raise ValueError("Настройки объединения не покрывают выбранные объекты и заглушки.")


def _owner_descriptor(relative: str) -> str:
    folder, _, rest = relative.partition("/")
    owner, sep, _ = rest.partition("/")
    return f"{folder}/{owner}.xml" if sep else relative


def _workspace_prefix(task: str | None) -> str:
    name = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "_", task or "CF")[:64].rstrip(". ")
    if PureWindowsPath(name).is_reserved():
        name = "_" + name
    return (name or "CF") + "-"


def _source_markers(root: Path) -> dict[str, str]:
    return {rel: _sha(root / rel) for rel in ("Configuration.xml", "ConfigDumpInfo.xml")
            if (root / rel).is_file()}


def _composition(selected, descriptors, rows, causes) -> PackageComposition:
    dependencies = []
    for row in rows:
        name, primary = row["object"], row["referred_by"]
        by_owner = causes.get(name, {})
        if primary not in by_owner or any(not isinstance(value, bool) for value in by_owner.values()):
            raise ValueError(f"Для заглушки {name} не подтверждены причины включения.")
        ordered = (primary, *sorted(set(by_owner) - {primary}))
        dependencies.append(Stub(name, tuple(DependencyCause(owner, by_owner[owner]) for owner in ordered)))
    names = {stub.name for stub in dependencies}
    if len(names) != len(dependencies) or not names <= descriptors.keys() or names.intersection(selected):
        raise ValueError("Список заглушек не соответствует исходной выгрузке.")
    return PackageComposition(selected, tuple(sorted(dependencies, key=lambda stub: stub.name)))


def _verify_plan(plan: DependencyPlan, root: Path, descriptors, inventory, known, *, git=None) -> None:
    owners = {descriptors[key] for key in plan.files}
    listed = {rel for paths in plan.files.values() for rel in paths}
    current = git.owned_files(owners) if git is not None else {rel for rel in inventory if _owner_descriptor(rel) in owners}
    if listed != current or any(
        (git.blob(rel) is None if git is not None else rel not in inventory)
        or (known[rel] if rel in known else _sha(root / rel)) != digest
        for rel, digest in plan.hashes.items()
    ):
        raise ValueError("Исходники зависимостей изменились. Сформируйте новый предпросмотр.")


class TechnicalPackage:
    def __init__(self, reader: DesignerExport, default_logs: Path):
        self.reader = reader
        self.default_logs = default_logs
        self._previews: dict[Path, DependencyPlan] = {}

    def prepare(self, source: ExportCatalog, task: str | None, selected: tuple[str, ...],
                destination: Path | None, log_directory: Path | None = None, index: Path | None = None) -> Preview:
        logs = journal_directory(log_directory or self.default_logs, source.root)
        log = create_journal(logs)
        write_journal(log, f"Предпросмотр выгрузки {source.root}. Выбрано объектов {len(selected)}.")
        plan = None
        try:
            markers = _source_markers(source.root)
            current, descriptors = self.reader.describe(source.root)
            if current != source:
                raise ValueError("Состав XML-выгрузки изменился. Откройте её заново.")
            if source.git is not None:
                checked = IndexReader.open(index, source.identity, HELPER_SHA256,
                                           inputs=source.git.index_inputs) if index and index.is_file() else None
                if checked is None:
                    raise ValueError("Создайте или обновите индекс выбранной ревизии Git.")
                checked.close()
            before = (source.git.index_inputs.inventory() if source.git is not None
                      else descriptor_inventory(source.root, descriptors))
            plan = resolve_dependencies(self.reader.helper, source.root, selected,
                                        index if index and index.is_file() else None, source.identity,
                                         git=source.git)
            result = plan.result
            if tuple(result["selected"]) != selected:
                raise ValueError("Фактический состав предпросмотра отличается от выбора.")
            owners = {descriptors[name] for name in selected}
            files = [native._relative(rel) for key in selected for rel in plan.files[key]]
            if not owners <= set(files) or any(_owner_descriptor(rel) not in owners for rel in files):
                raise ValueError("Сборщик вернул неожиданные файлы выбранных объектов.")
            hashes = {rel: _sha(source.git.ensure(rel) if source.git is not None else source.root / rel)
                      for rel in files}
            after = (source.git.index_inputs.inventory() if source.git is not None
                     else descriptor_inventory(source.root, descriptors))
            if after != before or _source_markers(source.root) != markers:
                raise ValueError("XML-выгрузка изменилась во время расчёта. Повторите предпросмотр.")
            composition = _composition(selected, descriptors, result["stubs"], result["causes"])
            warnings = tuple(f"Проверьте коды предопределённых элементов: {item['object']}"
                             for item in result["predefined_code_mismatch"])
            snapshot = _digest({"descriptors": before, "markers": markers, "selected": hashes})
            preview = Preview(task, source, composition, warnings, snapshot, destination, log, index)
            if result["index_message"]:
                write_journal(log, result["index_message"])
        except Exception as exc:
            if plan is not None:
                plan.close()
            write_journal(log, f"Ошибка предпросмотра. {exc}")
            raise PreviewError(str(exc), log) from exc

        self._previews[log] = plan
        write_journal(log, f"Предпросмотр готов. Заглушек {len(preview.dependencies)}.")
        for warning in preview.warnings:
            write_journal(log, warning)
        return preview

    def discard(self, preview: Preview) -> None:
        plan = self._previews.pop(preview.log, None)
        if plan is not None:
            plan.close()
            write_journal(preview.log, "Предпросмотр закрыт.")

    def _prepare(self, preview: Preview, run: Path, excluded: frozenset[str], plan: DependencyPlan) -> dict:
        source, selected, log, index = preview.source, preview.selected, preview.log, preview.index
        git = source.git
        before = _source_inventory(source.root) if git is None else None
        current, descriptors = self.reader.describe(source.root)
        if current != source:
            raise ValueError("Состав XML-выгрузки изменился. Откройте её заново.")

        markers = _source_markers(source.root)
        owners = {descriptors[name] for name in selected}
        if git is not None:
            top_level = git.index_inputs.inventory()
            selected_files = git.owned_files(owners)
        else:
            missing = set(descriptors.values()) - before.keys()
            if missing:
                raise ValueError("В выгрузке отсутствуют описания объектов: " + ", ".join(sorted(missing)[:5]))
            selected_files = {rel for rel in before if _owner_descriptor(rel) in owners}
            top_level = {rel: before[rel] for rel in descriptors.values()}
            top_level.update({rel: stamp for rel, stamp in before.items()
                              if rel.startswith("Documents/") and rel.count("/") == 1 and rel.endswith(".xml")})
        delivered = {rel: _sha(source.root / rel) for rel in selected_files}
        if _digest({"descriptors": top_level, "markers": markers, "selected": delivered}) != preview.snapshot:
            raise ValueError("XML-выгрузка изменилась. Сформируйте новый предпросмотр.")
        _verify_plan(plan, source.root, descriptors, before, markers | delivered, git=git)

        stage, settings = run / "staging", run / "merge-settings.xml"
        stubs_file = run / "merge-settings.xml.stubs.json"
        current_plan = plan
        if excluded:
            current_plan = resolve_dependencies(self.reader.helper, source.root, selected,
                                                index if index and index.is_file() else None,
                                                source.identity, excluded, git=source.git)
        try:
            result = current_plan.result
            if tuple(result["selected"]) != selected:
                raise ValueError("Фактический состав пакета отличается от выбора.")
            composition = _composition(selected, descriptors, result["stubs"], result["causes"])
            dependencies = composition.stubs
            stub_names = {item.name for item in dependencies}
            if excluded:
                _verify_plan(current_plan, source.root, descriptors, before, markers | delivered, git=git)
                previous = {stub.name for stub in preview.dependencies}
                if stub_names.intersection(excluded) or stub_names - previous:
                    raise ValueError("После исключения заглушек обнаружены обязательные зависимости. "
                                     "Сформируйте новый предпросмотр.")
                write_journal(log, "Исключены заглушки.\n" + "\n".join(sorted(previous - stub_names)))
                if result["index_message"]:
                    write_journal(log, result["index_message"])
            else:
                if composition != preview.composition:
                    raise ValueError("Зависимости изменились. Сформируйте новый предпросмотр.")
                write_journal(log, "Использую рассчитанный план предпросмотра.")
            current_plan.write(stage, settings)
            read_hashes = current_plan.hashes
        finally:
            if current_plan is not plan:
                current_plan.close()
        stubs_hash = _sha(stubs_file)
        _verify_merge_settings(settings, source.configuration, selected, dependencies)
        settings_hash = _sha(settings)
        for name in stub_names:
            rel = descriptors[name]
            if _object_uuid(source.root / rel, name) != _object_uuid(stage / rel, name):
                raise ValueError(f"При подготовке заглушки изменился UUID: {name}")

        stage_hashes = _inventory(stage)
        staged = {rel: sha for rel, sha in stage_hashes.items() if _owner_descriptor(rel) in owners}
        if delivered != staged:
            raise ValueError("Файлы выбранных объектов отличаются от исходной выгрузки.")
        modules = sorted(rel for rel in delivered if rel.casefold().endswith(".bsl"))
        xml = metadata_coverage(stage, list(selected))
        if (source.git is None and _source_inventory(source.root) != before) or any(
                _sha(source.root / rel) != digest for rel, digest in (read_hashes | markers | delivered).items()):
            raise ValueError("XML-выгрузка изменилась во время подготовки. Повторите предпросмотр.")
        self.reader.verify_helper()
        return {"stubs": {name: descriptors[name] for name in sorted(stub_names)},
                "stage_hashes": stage_hashes, "settings_sha256": settings_hash, "stubs_sha256": stubs_hash,
                "roundtrip_modules": modules, "roundtrip_xml": xml}

    def build(self, preview: Preview, platform: Path, excluded: frozenset[str] = frozenset()) -> Path:
        log = preview.log
        try:
            plan = self._previews.pop(log)
        except KeyError as exc:
            raise ValueError("Предпросмотр уже завершён. Сформируйте новый.") from exc
        workspace = None
        try:
            write_journal(log, f"Сборка CF. Платформа {platform}.")
            temporary = Path(tempfile.gettempdir()).resolve()
            if temporary == preview.source.root or preview.source.root in temporary.parents:
                raise ValueError("Системная временная папка находится внутри XML-выгрузки.")
            workspace = tempfile.TemporaryDirectory(prefix=_workspace_prefix(preview.task), dir=temporary)
            package = self._build(preview, Path(workspace.name), platform, excluded, plan)
            write_journal(log, f"CF проверена обратной выгрузкой. Комплект {package}.")
            return package
        except Exception as exc:
            write_journal(log, f"Ошибка сборки. {exc}")
            if isinstance(exc, native.NativeBuildError) and exc.receipt:
                message = str(exc)
                for step in exc.receipt.get("steps", ()):
                    message = message.replace(f"; see {step['log']}", "")
                if message != str(exc):
                    raise native.NativeBuildError(message, exc.receipt) from exc
            raise
        finally:
            plan.close()
            if workspace is not None:
                finish_workspace(workspace, log)

    def _build(self, preview: Preview, run: Path, platform: Path, excluded: frozenset[str],
               plan: DependencyPlan) -> Path:
        if preview.destination is None:
            raise ValueError("Укажите папку для результата.")
        destination = Path(os.path.abspath(preview.destination))
        _no_links(destination)
        destination = destination.resolve()
        if not destination.is_dir() or destination == preview.source.root or preview.source.root in destination.parents:
            raise ValueError("Укажите существующую папку результата вне XML-выгрузки.")
        if any(part.casefold() == ".git" for part in destination.parts):
            raise ValueError("Каталог результата не может находиться внутри .git.")
        platform = Path(os.path.abspath(platform))
        backend, version = _validate_platform(platform)
        write_journal(preview.log, f"Способ сборки {BUILD_LABELS[backend]}. Версия платформы {version}.")
        platform_hash = _sha(platform)
        proof = self._prepare(preview, run, excluded, plan)
        stage, settings = run / "staging", run / "merge-settings.xml"
        if (_sha(settings) != proof["settings_sha256"]
                or _sha(run / "merge-settings.xml.stubs.json") != proof["stubs_sha256"]):
            raise ValueError("Настройки объединения или список заглушек изменились. Повторите сборку.")
        started = datetime.now()
        receipt = native.build_cf(stage, run / "native", platform, version, preview.source.configuration,
                                  started.strftime("%d%m%y"), proof["roundtrip_modules"],
                                  proof["roundtrip_xml"], proof["stage_hashes"],
                                  command_runner=run_installed_platform if getattr(sys, "frozen", False) else None,
                                  task_id=preview.task, backend=backend)
        for name, rel in proof["stubs"].items():
            if _object_uuid(stage / rel, name) != _object_uuid(run / "native" / "readback" / rel, name):
                raise ValueError(f"После обратной выгрузки изменился UUID заглушки: {name}")
        if _sha(platform) != platform_hash:
            raise ValueError("Платформа изменилась во время сборки.")

        _no_links(destination)
        if not destination.is_dir():
            raise ValueError("Папка результата недоступна.")
        cf = Path(receipt["file"])
        name = run.name if preview.task else started.strftime("%Y-%m-%d_%H-%M-%S")
        with tempfile.TemporaryDirectory(prefix=".cf-package-", dir=destination) as temporary:
            draft = Path(temporary)
            for src, filename, digest in ((cf, cf.name, receipt["sha256"]),
                                          (settings, cf.stem + ".merge-settings.xml", proof["settings_sha256"])):
                target = draft / filename
                shutil.copyfile(src, target)
                if _sha(target) != digest:
                    raise ValueError(f"Изменился файл пакета: {filename}")
            package = destination / name
            suffix = 2
            while True:
                try:
                    draft.rename(package)
                    return package
                except FileExistsError:
                    package = destination / f"{name}-{suffix}"
                    suffix += 1
