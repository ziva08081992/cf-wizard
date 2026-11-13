"""Connect the approved HTML view to the desktop wizard use cases."""

from collections.abc import Callable
import os
from pathlib import Path
from threading import Event
from time import perf_counter

from PySide6.QtCore import QObject, QRunnable, Qt, QThreadPool, QTimer, QUrl, Signal, Slot
from PySide6.QtGui import QColor, QDesktopServices
from PySide6.QtWebChannel import QWebChannel
from PySide6.QtWebEngineCore import QWebEnginePage, QWebEngineProfile, QWebEngineSettings
from PySide6.QtWebEngineWidgets import QWebEngineView
from PySide6.QtWidgets import QDialog, QFileDialog, QMainWindow, QMessageBox

from . import __version__
from .adapters import BUILD_EXECUTABLES, BUILD_LABELS, PROJECT_PLATFORM_VERSION, preferred_platform
from .application import GitTaskQuery, GitTaskResult, IndexUpdate, JournaledError, Preview, Wizard
from .dependency_index import IndexReader
from .git_api import DEFAULT_GITEA, GitError, connection_server, create_client, credential_target, interval_seconds, relative_path
from .git_source import ExportSourceFactory
from .git_credentials import WindowsTokenStore
from .helper_worker import HELPER_SHA256
from .journals import ErrorJournal, create_journal, journal_directory, write_journal
from .paths import BUILD_MODES, PATH_FIELDS, PathPreferences, dialog_location, normalize_path
from .presentation import TYPE_LABELS, group_label, object_groups, object_label
from .qt_git_dialog import GitConnectionDialog


UI = Path(__file__).resolve().parent / "ui"
PAGE_SIZE = 150
PICKERS = {
    "cf-source": ("export", None),
    "Configuration.xml": ("export", "Configuration.xml"),
    "cf-result": ("destination", None),
    "cf-logs": ("logs", None),
    "cf-index": ("index_directory", None),
}
PLATFORM_FIELDS = {"ibcmd": "platform", "designer": "designer"}
LINKS = {
    "repository": "https://github.com/alonehobo/1c-partial-cf",
    "licenses": "https://doc.qt.io/qtforpython-6/licenses.html",
}


class TaskSignals(QObject):
    finished = Signal(object, object)


class Task(QRunnable):
    def __init__(self, operation):
        super().__init__()
        self.operation = operation
        self.signals = TaskSignals()

    def run(self):
        try:
            result = self.operation()
        except Exception as exc:
            self.signals.finished.emit(None, exc)
        else:
            self.signals.finished.emit(result, None)


def _object(key: str) -> dict[str, str]:
    kind, _, name = key.partition(".")
    return {"key": key, "name": name, "type": TYPE_LABELS.get(kind, (kind, kind))[1]}


def _stub_groups(preview) -> list[dict]:
    groups: dict[str, dict] = {}
    for stub in preview.dependencies:
        for cause in stub.causes:
            kind = cause.owner.partition(".")[0]
            groups.setdefault(kind, {}).setdefault(stub, []).append(cause.owner)
    kinds = (*TYPE_LABELS, *sorted(groups.keys() - TYPE_LABELS.keys()))
    return [
        {"kind": kind, "label": group_label(kind), "rows": [
            {**_object(stub.name), "required": stub.required,
             "reason": ", ".join(_object(owner)["name"] or owner for owner in owners),
             "owners": "\n".join(object_label(owner) for owner in owners)}
            for stub, owners in sorted(groups[kind].items(), key=lambda entry: entry[0].name)
        ]}
        for kind in kinds if kind in groups
    ]


class WizardBridge(QObject):
    changed = Signal("QVariantMap")
    stubsChanged = Signal("QVariantList")
    gitTaskProgress = Signal(int)
    journalOpened = Signal(str, str)
    notice = Signal(str, str)

    def __init__(self, window, wizard, platforms, paths, save_paths, default_logs, diagnostics):
        super().__init__(window)
        self.window, self.wizard, self.save_paths = window, wizard, save_paths
        self.platforms = platforms
        self.default_index_directory = str(default_logs.parent / "indexes")
        self.shared_index_directory = paths.values.get("index_directory") or self.default_index_directory
        self.index_directories = paths.index_directories.copy()
        self.sources = ExportSourceFactory(self.wizard.open_export, default_logs.parent / "git-cache")
        self.source_mode = paths.source_mode
        self.git_settings = {**DEFAULT_GITEA, "cache": str(default_logs.parent / "git-cache"), **paths.git_settings}
        if self.git_settings["cache"]:
            self.sources = ExportSourceFactory(self.wizard.open_export, Path(self.git_settings["cache"]))
        self.git_client = None
        self.git_branches = []
        self.git_poll_info = ""
        self.git_poll_task = None
        self.git_task_filter: GitTaskResult | None = None
        self.git_task_revision = 0
        self.git_task_stop: Event | None = None
        self.git_task_scope = ""
        self.git_timer = QTimer(self)
        self.git_timer.timeout.connect(self._poll_git)
        defaults = {field: str(preferred_platform(platforms, method) or "") for method, field in PLATFORM_FIELDS.items()}
        self.values = {**dict.fromkeys(PATH_FIELDS, ""), "logs": str(default_logs),
                       **defaults, **paths.values, "task": "", "build_mode": paths.build_mode}
        self.values["index_directory"] = self.shared_index_directory
        self.opened_identity = None
        self.index_file = None
        self.index_found = False
        self.index_check_task = None
        self.index_check_revision = 0
        self.index_info = "Откройте XML-выгрузку, чтобы создать индекс."
        self.last_dirs = paths.last_dirs.copy()
        self.default_logs = default_logs
        self.diagnostics = diagnostics
        self.catalog = self.preview = self.package = self.last_log = None
        self.selected: set[str] = set()
        self.excluded: set[str] = set()
        self.catalog_revision = 0
        self._query = None
        self._groups = {}
        self.stub_groups: list[dict] = []
        self.stub_total = 0
        self.task: Task | None = None
        self.status = "Укажите папку XML-выгрузки или её Configuration.xml."
        self.status_kind = "idle"

    @Slot(result="QVariantMap")
    def snapshot(self):
        busy = self.task is not None
        mode = self.values["build_mode"]
        platform_path = self._platform_path()
        platform = Path(normalize_path(platform_path))
        backend = next((method for method, filename in BUILD_EXECUTABLES.items()
                        if platform.name.casefold() == filename), None)
        can_use_platform = backend is not None and mode in ("auto", backend)
        placeholder = (f"Конфигуратор {PROJECT_PLATFORM_VERSION} не найден" if mode == "auto"
                       else f"Укажите {BUILD_EXECUTABLES[mode]}")
        git_task_ready = self._git_task_available()
        result = self.git_task_filter if git_task_ready else None
        return {
            "values": {**self.values, "platform_path": platform_path},
            "platforms": [str(path) for path in self.platforms if path.name.casefold() == BUILD_EXECUTABLES.get(mode)],
            "platformPlaceholder": placeholder,
            "platformLabel": f"{BUILD_LABELS[backend]} · {platform.parent.parent.name}" if can_use_platform else "",
            "configuration": self.catalog.configuration if self.catalog else "",
            "activeGit": self.catalog is not None and self.catalog.git is not None,
            "activeSource": (f"{self.catalog.git.client.label} · {self.catalog.git.branch} · {self.catalog.git.revision[:12]}"
                             if self.catalog and self.catalog.git else "Локальная папка"),
            "catalogRevision": self.catalog_revision,
            "sourceMode": self.source_mode,
            "gitBranches": self.git_branches or [self.git_settings["branch"]],
            "gitBranch": self.git_settings["branch"],
            "gitLocation": f"{self.git_settings['repository']} / {self.git_settings['directory']}",
            "gitPollInfo": self.git_poll_info,
            "gitTaskReady": git_task_ready,
            "gitTaskRevision": self.git_task_revision,
            "gitTaskScope": self.git_task_scope,
            "gitTaskCancelling": self.git_task_stop is not None and self.git_task_stop.is_set(),
            "gitTaskResult": {
                "scope": self._git_task_caption(result.query),
                "commits": result.commits, "files": result.files, "unmapped": result.unmapped,
                "objects": len(result.objects), "canMark": not self.selected.issuperset(result.objects),
            } if result is not None else None,
            "gitOpenText": "Обновить ветку" if self.catalog and self.catalog.git and
                           self.catalog.git.branch == self.git_settings["branch"] else "Открыть ветку",
            "selected": [_object(key) for key in sorted(self.selected)],
            "excluded": sorted(self.excluded), "stubTotal": self.stub_total, "busy": busy,
            "status": self.status, "statusKind": self.status_kind,
            "canPreview": bool(self.selected) and not busy
                          and (self.catalog.git is None or self.index_file is not None),
            "canBuild": self.preview is not None and can_use_platform
                        and bool(self.values["destination"].strip()) and not busy,
            "canMarkStubs": self.preview is not None and not busy,
            "canOpenPackage": self.package is not None and not busy,
            "canOpenJournal": self.last_log is not None and not busy,
            "canRefreshIndex": self.catalog is not None and self.wizard.indexes is not None and not busy,
            "indexExists": self.index_found,
            "indexInfo": self.index_info,
        }

    def _platform_path(self):
        mode = self.values["build_mode"]
        if mode == "auto":
            return str(preferred_platform(self.platforms, mode) or "")
        return self.values[PLATFORM_FIELDS[mode]]

    def _emit(self):
        self.changed.emit(self.snapshot())

    def _filtered(self, query, selected_only):
        result = self.git_task_filter if self._git_task_available() else None
        if selected_only:
            selection = self.selected if result is None else self.selected & result.objects
            return object_groups(tuple(sorted(selection)), query)
        cache_key = (query, result is not None)
        if cache_key != self._query:
            self._query = cache_key
            objects = self.catalog.objects if self.catalog else ()
            if result is not None:
                objects = tuple(sorted(result.objects))
            self._groups = object_groups(objects, query)
        return self._groups

    def _git_task_available(self):
        git = self.catalog.git if self.catalog else None
        return (self.source_mode == "git" and git is not None and git.client is self.git_client
                and git.branch == self.git_settings["branch"])

    def _git_task_caption(self, query):
        git = self.catalog.git
        if query.since is None:
            period = "Вся история"
        else:
            start = query.since.astimezone().strftime("%d.%m.%Y %H:%M")
            end = query.until.astimezone().strftime("%d.%m.%Y %H:%M")
            period = f"{start} — {end}"
        return f"{query.key} · {git.branch} · {git.revision[:12]} · {period}"

    def _set_git_task_filter(self, result):
        self.git_task_filter = result
        self.git_task_revision += 1
        self._query = None

    @Slot(str, bool, result="QVariantList")
    def groups(self, query, selected_only=False):
        return [{"kind": kind, "label": group_label(kind), "count": len(keys)}
                for kind, keys in self._filtered(query, selected_only).items()]

    @Slot(str, str, int, bool, result="QVariantList")
    def rows(self, kind, query, offset, selected_only=False):
        keys = self._filtered(query, selected_only).get(kind, ())
        return [_object(key) for key in keys[offset:offset + PAGE_SIZE]] if offset >= 0 else []

    def _invalidate_preview(self):
        if self.preview is not None:
            self.wizard.discard(self.preview)
        self.preview = self.package = None
        self.excluded.clear()
        self.stub_total = 0
        if self.stub_groups:
            self.stub_groups = []
            self.stubsChanged.emit([])
        if self.catalog:
            self.status = "Проверьте отмеченные объекты и сформируйте предпросмотр."
            self.status_kind = "idle"

    def _invalidate_catalog(self):
        self.catalog = None
        self.catalog_revision += 1
        self._set_git_task_filter(None)
        self.selected.clear()
        self._invalidate_preview()
        self.status = "Откройте XML-выгрузку."
        self.status_kind = "idle"
        self._sync_index()

    def _sync_index(self, *, preserve_log=False):
        self.index_check_revision += 1
        generation = self.index_check_revision
        self.index_file = None
        self.index_found = False
        self.index_info = "Откройте XML-выгрузку, чтобы создать индекс."
        if self.catalog is None:
            return
        try:
            path = self.wizard.index_path(self.catalog, self.values["index_directory"])
            self.index_found = bool(path and path.is_file())
            if self.catalog.git is not None:
                catalog, logs = self.catalog, self.values["logs"]
                self.index_info = "Проверяю сохранённый индекс…"

                def check():
                    started = perf_counter()
                    git = catalog.git
                    log = create_journal(journal_directory(logs or self.default_logs, catalog.root))
                    write_journal(log, f"Проверка индекса: {getattr(git.client, 'provider', 'gitea')}, источник {git.preference_key}, ветка {git.branch}, "
                                      f"SHA {git.revision}, входов {len(git.index_inputs.entries)}.")
                    try:
                        checked = IndexReader.open(path, catalog.identity, HELPER_SHA256,
                                                   inputs=git.index_inputs, strict=True) if path and path.is_file() else None
                    except Exception as exc:
                        write_journal(log, f"Ошибка проверки за {perf_counter() - started:.3f} с. {exc}")
                        raise JournaledError(str(exc), log) from exc
                    ready = checked is not None
                    if checked is not None:
                        checked.close()
                    message = "Индекс готов · использован сохранённый индекс" if ready else "Нужно обновить индекс по кнопке"
                    write_journal(log, f"{message}. Время {perf_counter() - started:.3f} с. "
                                      "XML не индексировались, факты не извлекались.")
                    return ready, log, message

                task = Task(check)
                self.index_check_task = task

                def completed(result, error):
                    if self.catalog is not catalog or generation != self.index_check_revision:
                        return
                    self.index_check_task = None
                    if error is not None:
                        self.index_info = f"Ошибка проверки индекса: {error}"
                        self.diagnostics.error("Проверка индекса", error)
                        self.last_log = getattr(error, "log", None)
                    else:
                        ready, log, self.index_info = result
                        if not preserve_log:
                            self.last_log = log
                        self.index_file = path if ready else None
                    self._emit()

                task.signals.finished.connect(completed, Qt.ConnectionType.QueuedConnection)
                QThreadPool.globalInstance().start(task)
                return
            if path and path.is_file():
                self.index_file = path
                self.index_info = f"{path.name} · записи сверяются с XML при использовании"
            else:
                self.index_info = "Для этой выгрузки в выбранной папке ещё нет индекса."
        except (ValueError, OSError) as exc:
            self.index_info = str(exc)

    @Slot(str, str)
    def editField(self, name, value):
        if name == "platform_path":
            name = PLATFORM_FIELDS.get(self.values["build_mode"])
        if self.task is not None or name not in self.values or self.values[name] == value:
            return
        if name == "build_mode" and value not in BUILD_MODES:
            return
        self.values[name] = value
        if name == "export":
            if self.catalog is None or self.catalog.git is None:
                self._invalidate_catalog()
        elif name == "index_directory":
            directory = normalize_path(value) or self.default_index_directory
            if self.catalog is not None:
                self.index_directories[self.catalog.preference_key] = directory
            else:
                self.shared_index_directory = directory
            self._sync_index()
            self._invalidate_preview()
        elif name in ("destination", "task", "build_mode"):
            self.package = None
        elif name not in PLATFORM_FIELDS.values():
            if name == "logs":
                self.diagnostics.set_directory(value, self.values["export"])
            self._invalidate_preview()
        self._emit()

    @Slot(str)
    def pickPath(self, picker):
        if self.task is not None:
            return
        if picker == "cf-platform":
            mode = self.values["build_mode"]
            if mode == "auto":
                return
            name, filename = PLATFORM_FIELDS[mode], BUILD_EXECUTABLES[mode]
        elif picker in PICKERS:
            name, filename = PICKERS[picker]
        else:
            return
        location = dialog_location(self.values[name], self.last_dirs.get(name, ""), filename=filename)
        folder = location.get("initialdir", "")
        if filename:
            initial = str(Path(folder) / location.get("initialfile", filename)) if folder else filename
            path, _ = QFileDialog.getOpenFileName(self.window, f"Выберите {filename}", initial, f"{filename} ({filename})")
        else:
            path = QFileDialog.getExistingDirectory(self.window, "Выберите папку", folder)
        if path:
            path = normalize_path(path)
            self.last_dirs[name] = str(Path(path).parent if filename else Path(path))
            self.editField(name, path)
            if name == "export" and self.source_mode == "local":
                self._open_export()

    @Slot(str, bool, result="QVariantMap")
    def markObject(self, key, checked):
        if self.task is not None or self.catalog is None or key not in self.catalog.objects:
            return self.snapshot()
        if checked:
            self.selected.add(key)
        else:
            self.selected.discard(key)
        self._invalidate_preview()
        return self.snapshot()

    @Slot(str, bool)
    def markStub(self, key, checked):
        if self.task is not None or self.preview is None:
            return
        excluded = self.excluded - {key} if checked else self.excluded | {key}
        try:
            self.excluded = set(self.preview.composition.exclusions(excluded))
        except ValueError:
            self._emit()
            return
        self.status = f"В CF включено заглушек {self.stub_total - len(self.excluded)}."
        self._emit()

    @Slot(str)
    def command(self, name):
        if name == "about":
            self.notice.emit("О программе", f"Мастер технической CF\nВерсия программы {__version__}\n"
                             "Спасибо alonehobo за скрипт partial_cf.py.")
        elif name == "cancel-git-task" and self.git_task_stop is not None:
            self.git_task_stop.set()
            self.status = "Отменяю поиск после текущего запроса…"
            self._emit()
        elif self.task is None:
            if name == "clear-git-task" and self.git_task_filter is not None:
                self._set_git_task_filter(None)
                self.status, self.status_kind = "Отбор по задаче сброшен.", "ready"
                self._emit()
            elif name == "mark-git-task" and self._git_task_available() and self.git_task_filter is not None:
                objects = self.git_task_filter.objects
                if not self.selected.issuperset(objects):
                    self.selected.update(objects)
                    self._invalidate_preview()
                    self._emit()
            elif name == "open-source":
                self._open_export()
            elif name == "git-connect":
                self._connect_git()
            elif name == "refresh-index" and self.catalog is not None and self.wizard.indexes is not None:
                self._refresh_index()
            elif name == "preview" and self.selected:
                self._prepare()
            elif name == "build" and self.preview is not None:
                self._build()
            elif name == "package":
                self._open_path(self.package)
            elif name == "journal":
                self._open_journal()
            elif name == "logs":
                folder = Path(normalize_path(self.values["logs"]) or self.default_logs)
                if folder.is_dir():
                    self._open_path(folder)
                else:
                    self.notice.emit("Папка журналов", f"Папка ещё не создана.\n{folder}")

    @Slot(str)
    def openLink(self, name):
        if name == "icons":
            self.notice.emit("Лицензия значков", (UI / "LICENSE-lucide.txt").read_text(encoding="utf-8"))
        elif name in LINKS:
            QDesktopServices.openUrl(QUrl(LINKS[name]))

    def _open_path(self, path):
        if path is not None:
            try:
                os.startfile(str(path))
            except OSError as exc:
                self.diagnostics.error(f"Открытие файла или папки {path}.", exc)
                self.notice.emit("Открытие файла или папки", str(exc))

    def _open_journal(self):
        log = self.last_log
        if log is None:
            return
        status = self.status, self.status_kind

        def read():
            try:
                return log.read_text(encoding="utf-8", errors="replace")
            except OSError as exc:
                raise ValueError(f"Не удалось открыть журнал.\n{log}\n{exc.strerror or exc}") from exc

        def opened(text):
            self.status, self.status_kind = status
            self.journalOpened.emit(str(log), text)

        self._run("Читаю журнал…", read, opened, operation_log=log)

    def _run(self, status, operation, success, *, operation_log=None, accept_result=None):
        self.task = Task(operation)
        self.status, self.status_kind = status, "busy"
        self.diagnostics.step(status, operation_log)
        self._emit()

        def completed(result, error):
            self.task = None
            if accept_result is not None and not accept_result():
                self._emit()
                return
            log = operation_log
            if error is None:
                if isinstance(result, (IndexUpdate, Preview)):
                    self.last_log = log = result.log
                    self.last_dirs["logs"] = str(log.parent)
                self.status_kind = "ready"
                success(result)
                self.diagnostics.step(self.status, log)
            else:
                if isinstance(error, JournaledError):
                    self.last_log = log = error.log
                report = self.diagnostics.error(status, error, log)
                self.status = "Операция остановлена. Проверьте сообщение и журнал."
                self.status_kind = "error"
                message = str(error) + (f"\n\nЖурнал ошибок {report}" if report else "")
                self.notice.emit("Мастер технической CF", message)
            self._emit()

        self.task.signals.finished.connect(completed, Qt.ConnectionType.QueuedConnection)
        QThreadPool.globalInstance().start(self.task)

    @Slot(str, bool, str)
    def searchGitTask(self, value, all_history, days="90"):
        if self.task is not None or not self._git_task_available():
            return
        try:
            query = GitTaskQuery.create(value, all_history, days)
        except ValueError as error:
            self.notice.emit("Поиск по задаче Git", str(error))
            return

        catalog = self.catalog
        stop = self.git_task_stop = Event()
        self.git_task_scope = self._git_task_caption(query)

        def find():
            try:
                return self.wizard.find_git_task(catalog, query, stop, self.gitTaskProgress.emit)
            except (OSError, ValueError) as error:
                raise ValueError(f"Поиск по задаче {query.key} не завершён. {error}") from None

        def accept_result():
            self.git_task_stop = None
            self.git_task_scope = ""
            if stop.is_set():
                self.status, self.status_kind = "Поиск по задаче отменён.", "idle"
                return False
            if self.catalog is not catalog or not self._git_task_available():
                self.status, self.status_kind = "Открытая ревизия изменилась. Повторите поиск.", "idle"
                return False
            return True

        def found(result):
            self._set_git_task_filter(result)
            self.status = f"Поиск по задаче {query.key} завершён."

        self._run(f"Ищу коммиты по задаче {query.key}…", find, found, accept_result=accept_result)

    @Slot(str)
    def setSourceMode(self, mode):
        if self.task is None and mode in ("local", "git"):
            self.source_mode = mode
            self._emit()

    @Slot(str)
    def setGitBranch(self, branch):
        if self.task is None and branch in self.git_branches:
            self.git_settings["branch"] = branch
            self._emit()

    def _connect_git(self):
        dialog = GitConnectionDialog(self.window, self.git_settings, self.git_branches, self._forget_git)
        try:
            if dialog.exec() != QDialog.DialogCode.Accepted:
                return
            settings = dialog.connection_settings()
            token = dialog.secret.text().strip()
            save_token = dialog.remember.isChecked()
        finally:
            dialog.secret.clear()
            dialog.deleteLater()

        try:
            settings["server"] = connection_server(settings["server"], settings.get("provider", "gitea"))
            settings["directory"] = relative_path(settings["directory"])
            settings["interval"] = interval_seconds(settings["interval"])
            vault = WindowsTokenStore(credential_target(settings["server"], settings.get("provider", "gitea")))
            if not token:
                token = vault.load()
            client = create_client(settings, token)
        except (ValueError, OSError) as error:
            self.notice.emit("Подключение Git", str(error))
            return

        def connected(branches):
            if not branches:
                self.status, self.status_kind = f"{client.label} не вернула доступных веток.", "error"
                return
            self.git_client = client
            self.git_branches = branches
            if settings["branch"] not in branches:
                settings["branch"] = branches[0]
            self.git_settings = settings
            cache = Path(normalize_path(settings["cache"]) or self.default_logs.parent / "git-cache").absolute()
            self.sources = ExportSourceFactory(self.wizard.open_export, cache)
            self.status = f"Подключено {client.repository}. Выберите ветку и нажмите «Открыть ветку»."
            if save_token:
                try:
                    vault.save(token)
                except OSError:
                    self.notice.emit("Подключение Git", "Токен доступен в сеансе. Windows не сохранила запись.")

        self._run(f"Проверяю подключение и читаю ветки {client.label}…", client.branches, connected)

    def _forget_git(self, settings=None):
        settings = settings or self.git_settings
        target = credential_target(settings["server"], settings.get("provider", "gitea"))
        WindowsTokenStore(target).delete()

        def matches(client):
            return client is not None and credential_target(client.server, client.provider) == target

        if matches(self.git_client):
            self.git_client = None
            self.git_branches = []
        if self.catalog is not None and self.catalog.git is not None and matches(self.catalog.git.client):
            self._invalidate_catalog()
            self.git_timer.stop()
            self.git_poll_info = ""
        self.status = "Локальный токен выбранного Git-сервиса удалён."
        self._emit()

    def _open_git(self):
        settings = dict(self.git_settings)
        client = self.git_client
        if client is None:
            try:
                token = WindowsTokenStore(credential_target(settings["server"], settings.get("provider", "gitea"))).load()
                if token:
                    client = create_client(settings, token)
            except (ValueError, OSError):
                client = None
        if client is None:
            self._connect_git()
            return

        seed_path = self.values["export"]
        directories = dict(self.index_directories)
        sources = self.sources

        def read():
            seed_root = seed_index = None
            if seed_path and self.wizard.indexes is not None:
                try:
                    local = self.wizard.open_export(seed_path)
                    seed_root = local.root
                    seed_index = self.wizard.index_path(local, directories.get(local.preference_key, ""))
                except (OSError, ValueError):
                    pass
            cache = sources.cache.absolute()
            if seed_root is not None and (cache == seed_root or seed_root in cache.parents):
                raise ValueError("Выберите папку кэша вне локальной XML-выгрузки.")
            branches = client.branches() if not self.git_branches else None
            catalog = sources.strategy("git")(
                client, settings["branch"], settings["directory"], seed_root, seed_index,
            )
            return catalog, branches

        def opened(result):
            catalog, branches = result
            self.git_client = client
            if branches is not None:
                self.git_branches = branches
            self._publish_catalog(catalog)

        self._run("Читаю Configuration.xml и опись Git…", read, opened)

    def _poll_git(self):
        if self.task is not None or self.git_poll_task is not None or self.catalog is None:
            return
        catalog = self.catalog
        git = catalog.git
        if git is None or self.source_mode != "git":
            return
        task = self.git_poll_task = Task(lambda: git.client.branch_sha(git.branch))

        def completed(sha, error):
            self.git_poll_task = None
            if self.catalog is not catalog or not self.git_timer.isActive() or self.source_mode != "git":
                return
            if error is not None:
                self.git_poll_info = "Проверка ревизии Git недоступна."
                if isinstance(error, GitError) and error.status in (401, 403, 429):
                    self.git_timer.stop()
            else:
                self.git_poll_info = "Доступно обновление" if sha != git.revision else ""
            self._emit()

        task.signals.finished.connect(completed, Qt.ConnectionType.QueuedConnection)
        QThreadPool.globalInstance().start(task)

    def _publish_catalog(self, catalog):
        previous = self.opened_identity
        result = self.git_task_filter if self.catalog == catalog and catalog.git is not None else None
        self._invalidate_catalog()
        self.catalog = catalog
        self.git_task_filter = result
        self.catalog_revision += 1
        self._query = None
        self.opened_identity = catalog.preference_key
        self.git_timer.stop()
        self.git_poll_info = ""
        if catalog.git is None:
            self.last_dirs["export"] = str(catalog.root)
            self.status = f"Открыта конфигурация {catalog.configuration}. Отметьте нужные объекты."
        else:
            self.status = f"Открыта {catalog.configuration} из {catalog.git.branch} ({catalog.git.revision[:12]})."
            if self.git_settings["interval"]:
                self.git_timer.start(1000 * int(self.git_settings["interval"]))
        self.values["index_directory"] = self.index_directories.get(catalog.preference_key, self.shared_index_directory)
        self._sync_index()
        if previous is not None and previous != catalog.preference_key and self.wizard.indexes is not None:
            label = f"{catalog.git.client.label} · {catalog.git.branch}" if catalog.git is not None else "Локальная выгрузка"
            dialog = QMessageBox(
                QMessageBox.Icon.Question, "Смена источника XML",
                f"{label}. Используется отдельный индекс SQLite.\n\nПапка индекса\n{self.values['index_directory']}",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, self.window,
            )
            dialog.button(QMessageBox.StandardButton.Yes).setText("Выбрать папку")
            dialog.button(QMessageBox.StandardButton.No).setText("Оставить")
            dialog.setDefaultButton(QMessageBox.StandardButton.No)
            dialog.setEscapeButton(QMessageBox.StandardButton.No)
            if dialog.exec() == QMessageBox.StandardButton.Yes:
                self.pickPath("cf-index")

    def _open_export(self):
        if self.source_mode == "git":
            self._open_git()
            return
        path = self.values["export"] = normalize_path(self.values["export"])
        if not path:
            self.pickPath("cf-source")
            return
        self.diagnostics.set_directory(self.values["logs"], path)
        self._run("Читаю состав XML-выгрузки…", lambda: self.sources.strategy("local")(path), self._publish_catalog)

    def _refresh_index(self):
        catalog = self.catalog
        directory, logs = self.values["index_directory"], self.values["logs"]
        self._invalidate_preview()

        def refreshed(update):
            if self.catalog != update.source:
                self.catalog = update.source
                self.catalog_revision += 1
                self._set_git_task_filter(None)
                self.selected.intersection_update(update.source.objects)
            self.last_dirs["index_directory"] = str(update.path.parent)
            self.index_directories[update.source.preference_key] = str(update.path.parent)
            self.values["index_directory"] = str(update.path.parent)
            self._sync_index(preserve_log=True)
            self.status = (f"Индекс готов · добавлено {update.added} · изменено {update.changed} · "
                           f"удалено {update.removed} · переиспользовано {update.unchanged}")

        self._run("Проверяю XML и обновляю индекс зависимостей…",
                  lambda: self.wizard.refresh_index(catalog, directory, logs), refreshed)

    def _prepare(self):
        catalog, selected = self.catalog, self.selected.copy()
        task, output, logs, index_directory = (self.values[key] for key in ("task", "destination", "logs", "index_directory"))
        self._invalidate_preview()

        def prepared(preview):
            self.preview = preview
            self.stub_groups = _stub_groups(preview)
            self.stub_total = len(preview.dependencies)
            self.status = f"Предпросмотр готов · {len(preview.selected)} объектов · {self.stub_total} заглушек"
            self.stubsChanged.emit(self.stub_groups)
            if preview.warnings:
                self.notice.emit("Предпросмотр", "\n".join(preview.warnings))

        self._run("Рассчитываю зависимости и заглушки…",
                  lambda: self.wizard.preview(catalog, task, selected, output, logs, index_directory), prepared)

    def _build(self):
        preview = self.preview
        mode = self.values["build_mode"]
        platform = normalize_path(self._platform_path())
        if not platform or (mode != "auto" and Path(platform).name.casefold() != BUILD_EXECUTABLES[mode]):
            self.notice.emit("Платформа 1С", "Укажите исполняемый файл выбранного способа сборки.")
            return
        field = PLATFORM_FIELDS.get(mode)
        destination, task = self.values["destination"], self.values["task"]
        excluded = frozenset(self.excluded)
        self.preview = None

        def work():
            try:
                return self.wizard.build(preview, platform, excluded, destination=destination, task=task)
            except Exception as exc:
                raise ValueError(f"{exc}\nЖурнал {preview.log}\nДля повтора сформируйте новый предпросмотр.") from exc

        def built(package):
            self.package = package
            self.last_dirs["destination"] = str(package.parent)
            if field is not None:
                self.last_dirs[field] = str(Path(platform).parent)
            self.status = f"CF проверена обратной выгрузкой. Комплект {package}"

        self._run("Готовлю XML-комплект, собираю CF и проверяю обратную выгрузку…", work, built,
                  operation_log=preview.log)

    def close(self):
        self.git_timer.stop()
        if self.preview is not None:
            self.wizard.discard(self.preview)
            self.preview = None
        values = {key: normalize_path(self.values[key]) for key in PATH_FIELDS}
        values["index_directory"] = self.shared_index_directory
        self.save_paths(PathPreferences(values, self.last_dirs, self.index_directories, self.values["build_mode"],
                                        self.source_mode, self.git_settings))


class LocalPage(QWebEnginePage):
    def __init__(self, profile, url, parent, diagnostics):
        super().__init__(profile, parent)
        self.start_url = url
        self.diagnostics = diagnostics
        self.renderProcessTerminated.connect(self._renderer_terminated)

    def acceptNavigationRequest(self, url, navigation_type, is_main_frame):
        return is_main_frame and url == self.start_url

    def javaScriptConsoleMessage(self, level, message, line_number, source_id):
        if level == self.JavaScriptConsoleMessageLevel.ErrorMessageLevel:
            self.diagnostics.error(f"Ошибка интерфейса JavaScript. {source_id}, строка {line_number}.\n{message}")
        super().javaScriptConsoleMessage(level, message, line_number, source_id)

    def _renderer_terminated(self, status, exit_code):
        if status != self.RenderProcessTerminationStatus.NormalTerminationStatus:
            self.diagnostics.error(f"Процесс Qt WebEngine завершился аварийно. {status.name}. Код {exit_code}.")


class QtWizardWindow(QMainWindow):
    def __init__(self, wizard: Wizard, platforms: tuple[Path, ...],
                 paths: PathPreferences, save_paths: Callable[[PathPreferences], None], default_logs: Path,
                 diagnostics: ErrorJournal):
        super().__init__()
        self.diagnostics = diagnostics
        self.setWindowTitle("Мастер технической CF")
        self.setMinimumSize(700, 600)
        self.resize(1180, 790)
        self.view = QWebEngineView()
        self.setCentralWidget(self.view)
        self.profile = QWebEngineProfile(self)
        url = QUrl.fromLocalFile(str(UI / "index.html"))
        page = LocalPage(self.profile, url, self.view, diagnostics)
        page.setBackgroundColor(QColor("#1e1f22"))
        page.settings().setAttribute(QWebEngineSettings.WebAttribute.LocalContentCanAccessRemoteUrls, False)
        page.settings().setAttribute(QWebEngineSettings.WebAttribute.JavascriptCanOpenWindows, False)
        self.view.setPage(page)
        self.view.setContextMenuPolicy(Qt.ContextMenuPolicy.NoContextMenu)
        self.bridge = WizardBridge(self, wizard, platforms, paths, save_paths, default_logs, diagnostics)
        channel = QWebChannel(page)
        channel.registerObject("wizard", self.bridge)
        page.setWebChannel(channel)
        self.view.loadFinished.connect(self._loaded)
        self.view.load(url)

    def _loaded(self, ok):
        if not ok:
            message = "Не удалось загрузить интерфейс мастера."
            self.diagnostics.error(message)
            QMessageBox.critical(self, "Мастер технической CF", message)

    def closeEvent(self, event):
        if self.bridge.task is not None:
            QMessageBox.information(self, "Мастер технической CF", "Дождитесь завершения текущего шага.")
            event.ignore()
            return
        try:
            self.bridge.close()
        except OSError as exc:
            self.diagnostics.error("Сохранение путей.", exc)
            QMessageBox.warning(self, "Сохранение путей", str(exc))
        event.accept()
