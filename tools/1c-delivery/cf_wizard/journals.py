"""Persistent operation journals and fatal error diagnostics."""

import atexit
from datetime import datetime
import faulthandler
import logging
import os
from pathlib import Path
import sys
from tempfile import NamedTemporaryFile, TemporaryDirectory
import threading
import traceback

from stage import _no_links

from . import __version__
from .paths import normalize_path


LOG_OUTPUTS = (
    "partial-cf.log", "selection/partial-cf.log", "native/logs/01-create.log",
    "native/logs/02-import.log", "native/logs/03-export.log",
    "native/logs/02-load-xml.log", "native/logs/03-save-cf.log", "native/logs/04-create-readback.log",
    "native/logs/05-load-cf.log", "native/logs/06-export-xml.log",
)
MAX_OUTPUT_BYTES = 64 * 1024
ERROR_DIRECTORY = "Ошибки"
LOGGER = logging.getLogger(__name__)


def journal_directory(directory: Path, source: Path | None) -> Path:
    directory = Path(os.path.abspath(directory))
    _no_links(directory)
    directory = directory.resolve()
    if source is not None:
        source = source.resolve()
    if directory == source or source in directory.parents or any(part.casefold() == ".git" for part in directory.parts):
        raise ValueError("Укажите папку журналов вне XML-выгрузки и .git.")
    return directory


class ErrorJournal:
    """Keep a pre-opened file for native crashes and handled exceptions."""

    def __init__(self, default_logs: Path):
        self.default_logs = default_logs
        self._stream = None
        self._has_error = False
        self._checkpoint = "Запуск мастера."
        self._lock = threading.RLock()

    def install(self) -> None:
        def uncaught(kind, error, tb):
            self.error("Необработанное исключение Python.", error)
            sys.__excepthook__(kind, error, tb)

        def thread_failed(args):
            if args.exc_type is not SystemExit:
                self.error(f"Исключение в потоке {args.thread.name}.", args.exc_value)
            threading.__excepthook__(args)

        sys.excepthook = uncaught
        threading.excepthook = thread_failed
        atexit.register(self.close)

    def set_directory(self, logs: str, export: str) -> None:
        requested = Path(normalize_path(logs) or self.default_logs)
        export = normalize_path(export)
        source = Path(export) if export else None
        if source is not None and source.name.casefold() == "configuration.xml":
            source = source.parent
        with self._lock:
            try:
                self._switch(journal_directory(requested / ERROR_DIRECTORY, source))
            except (OSError, ValueError, RuntimeError) as exc:
                try:
                    self._switch(journal_directory(self.default_logs / ERROR_DIRECTORY, source))
                except (OSError, ValueError, RuntimeError) as fallback:
                    self.close()
                    LOGGER.warning("Не удалось открыть журнал ошибок. %s. %s", exc, fallback)
                else:
                    self.error(f"Недоступна выбранная папка ошибок {requested / ERROR_DIRECTORY}.", exc)

    def _switch(self, directory: Path) -> None:
        if self._stream is not None and Path(self._stream.name).parent == directory:
            return
        directory.mkdir(parents=True, exist_ok=True)
        prefix = f"CF-Wizard-{datetime.now():%Y-%m-%d_%H-%M-%S}-{os.getpid()}-"
        stream = NamedTemporaryFile("ab", buffering=0, prefix=prefix, suffix=".log", dir=directory, delete=False)
        try:
            faulthandler.enable(file=stream, all_threads=True)
        except (OSError, ValueError, RuntimeError):
            stream.close()
            Path(stream.name).unlink(missing_ok=True)
            raise
        self._release()
        self._stream = stream
        self._has_error = False
        self._write(f"CF-Wizard {__version__}. PID {os.getpid()}. Python {sys.version.split()[0]}.\n{self._checkpoint}")

    def step(self, message: str, operation_log: Path | None = None) -> None:
        with self._lock:
            self._checkpoint = message + (f"\nЖурнал операции {operation_log}" if operation_log else "")
            self._write(self._checkpoint)

    def error(self, message: str, error: BaseException | None = None, operation_log: Path | None = None) -> Path | None:
        with self._lock:
            self._has_error = True
            log = f"\nЖурнал операции {operation_log}" if operation_log else ""
            detail = "\n" + "".join(traceback.format_exception(error)).rstrip() if error is not None else ""
            return self._write(f"Ошибка. {message}{log}{detail}")

    def _write(self, message: str) -> Path | None:
        if self._stream is not None:
            try:
                self._stream.write(f"{datetime.now():%Y-%m-%d %H:%M:%S}  {message}\n".encode("utf-8", "backslashreplace"))
                return Path(self._stream.name)
            except OSError as exc:
                LOGGER.warning("Не удалось записать журнал ошибок. %s", exc)
        return None

    def _release(self) -> None:
        if self._stream is not None:
            stream, self._stream = self._stream, None
            try:
                stream.close()
                if not self._has_error:
                    Path(stream.name).unlink(missing_ok=True)
            except OSError as exc:
                LOGGER.warning("Не удалось закрыть журнал ошибок. %s", exc)

    def close(self) -> None:
        with self._lock:
            if self._stream is not None:
                faulthandler.disable()
                self._release()


def create_journal(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    prefix = datetime.now().strftime("CF-Wizard-%Y-%m-%d_%H-%M-%S-")
    with NamedTemporaryFile(prefix=prefix, suffix=".log", dir=directory, delete=False) as log:
        return Path(log.name)


def write_journal(log: Path, message: str) -> None:
    try:
        with log.open("a", encoding="utf-8") as stream:
            stream.write(f"{datetime.now():%Y-%m-%d %H:%M:%S}  {message}\n")
    except OSError as exc:
        logging.getLogger(__name__).warning("Не удалось записать журнал %s. %s", log, exc)


def finish_workspace(workspace: TemporaryDirectory, log: Path) -> None:
    root = Path(workspace.name)
    try:
        for relative in LOG_OUTPUTS:
            path = root / relative
            try:
                with path.open("rb") as stream:
                    size = stream.seek(0, 2)
                    stream.seek(max(0, size - MAX_OUTPUT_BYTES))
                    output = stream.read(MAX_OUTPUT_BYTES).decode("utf-8", errors="replace")
                detail = " (последние 64 КиБ)" if size > MAX_OUTPUT_BYTES else ""
                write_journal(log, f"{relative}{detail}\n{output}")
            except FileNotFoundError:
                continue
            except OSError as exc:
                write_journal(log, f"Ошибка чтения {relative}. {exc}")
    finally:
        try:
            workspace.cleanup()
        except OSError as exc:
            write_journal(log, f"Ошибка очистки временной папки {root}. {exc}")
        else:
            write_journal(log, "Временные файлы очищены.")
