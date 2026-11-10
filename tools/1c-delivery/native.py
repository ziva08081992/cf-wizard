"""Build and read back a CF using only a new, isolated native work directory."""
from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import stat
import subprocess
import time
from typing import Callable
import xml.etree.ElementTree as ET


class NativeBuildError(RuntimeError):
    """A failed native build; receipt is available after work directory creation."""

    def __init__(self, message: str, receipt: dict | None = None):
        super().__init__(message)
        self.receipt = receipt


def _utc() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _start() -> dict:
    return {"startedUtc": _utc(), "monotonicStart": time.monotonic()}


def _finish(timing: dict) -> dict:
    end = time.monotonic()
    return {**timing, "finishedUtc": _utc(), "monotonicEnd": end,
            "seconds": round(end - timing["monotonicStart"], 6)}


def _plain_path(path: Path, *, must_exist: bool = True) -> Path:
    """Reject network paths, symlinks and Windows junction/reparse points."""
    raw = os.fspath(path)
    if raw.startswith(("\\\\", "//")) or PureWindowsPath(raw).drive.startswith("\\\\"):
        raise NativeBuildError(f"Network paths are not allowed: {path}")
    absolute = Path(os.path.abspath(raw))
    for part in (*reversed(absolute.parents), absolute):
        if not os.path.lexists(part):
            continue
        info = part.lstat()
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise NativeBuildError(f"Symlink or reparse point is not allowed: {part}")
    if must_exist and not absolute.exists():
        raise NativeBuildError(f"Path does not exist: {absolute}")
    return absolute.resolve(strict=must_exist)


def _relative(value: str) -> str:
    if not isinstance(value, str) or not value or "\\" in value or ":" in value or "\x00" in value:
        raise NativeBuildError(f"Expected a safe relative POSIX path: {value!r}")
    parts = value.split("/")
    reserved = {"con", "prn", "aux", "nul", *(f"com{i}" for i in range(1, 10)),
                *(f"lpt{i}" for i in range(1, 10))}
    if PurePosixPath(value).is_absolute() or PureWindowsPath(value).drive or any(
        part in ("", ".", "..") or part.endswith((".", " "))
        or part.split(".", 1)[0].casefold() in reserved
        or any(char in '<>"|?*' or ord(char) < 32 for char in part)
        for part in parts
    ):
        raise NativeBuildError(f"Unsafe relative path: {value!r}")
    return value


def _under(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _member(root: Path, relative: str, *, must_exist: bool = True) -> Path:
    candidate = _plain_path(root / _relative(relative), must_exist=must_exist)
    if not _under(candidate, root):
        raise NativeBuildError(f"Path escapes its scope: {candidate}")
    return candidate


def _files(root: Path, *, hashes: bool) -> dict[str, str]:
    _plain_path(root)
    result = {}
    def fail(error):
        raise error
    for current, directories, files in os.walk(root, followlinks=False, onerror=fail):
        for name in sorted(directories + files):
            item = Path(current) / name
            _plain_path(item)
            relative = _relative(item.relative_to(root).as_posix())
            mode = item.stat().st_mode
            if stat.S_ISDIR(mode):
                continue
            if not stat.S_ISREG(mode):
                raise NativeBuildError(f"Non-regular file is not allowed: {item}")
            if hashes:
                result[relative] = hashlib.sha256(item.read_bytes()).hexdigest()
    return result


def _verify_inventory(stage: Path, expected: dict[str, str]) -> None:
    actual = _files(stage, hashes=True)
    missing = sorted(expected.keys() - actual.keys())
    added = sorted(actual.keys() - expected.keys())
    changed = sorted(key for key in expected.keys() & actual.keys() if expected[key] != actual[key])
    if missing or added or changed:
        raise NativeBuildError(f"Staging inventory differs: missing={missing}, added={added}, changed={changed}")


def _xml_signature(path: Path):
    """Expanded XML names and namespace-aware QName values, with meaningful text."""
    namespaces = {}
    stack = [{"xml": "http://www.w3.org/XML/1998/namespace"}]
    pending = {}
    parser = ET.iterparse(path, events=("start-ns", "start", "end"))
    for event, value in parser:
        if event == "start-ns":
            prefix, uri = value
            pending[prefix or ""] = uri
        elif event == "start":
            scope = {**stack[-1], **pending}
            pending = {}
            namespaces[value] = scope
            stack.append(scope)
        else:
            stack.pop()
    qname = re.compile(r"([^\W\d][\w.-]*):([^\W\d][\w.-]*)", re.UNICODE)
    def text_value(value, scope, preserve=False):
        if value is None or (not preserve and value.strip(" \t\r\n") == ""):
            return None
        match = qname.fullmatch(value)
        if match and match[1] in scope:
            return ("qname", scope[match[1]], match[2])
        return ("text", value)
    def node(element, inherited_space=False):
        scope = namespaces[element]
        space = element.attrib.get("{http://www.w3.org/XML/1998/namespace}space")
        preserve = inherited_space if space is None else space == "preserve"
        attributes = tuple(sorted((name, text_value(value, scope, True))
                                  for name, value in element.attrib.items()))
        children = tuple((node(child, preserve), text_value(child.tail, scope, preserve))
                         for child in element)
        return (element.tag, attributes, text_value(element.text, scope, preserve), children)
    return node(parser.root)


def _write_json(root: Path, name: str, value: dict) -> None:
    target = _member(root, name, must_exist=False)
    target.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def _decode(output) -> str:
    if output is None:
        return ""
    if isinstance(output, str):
        return output
    try:
        return output.decode("utf-8")
    except UnicodeDecodeError:
        return output.decode("cp866", errors="replace")


def build_cf(stage: Path, work_dir: Path, ibcmd: Path, platform_version: str,
             configuration_name: str, date_stamp: str, module_paths: list[str],
             xml_paths: list[str], inventory: dict[str, str], command_runner: Callable | None = None,
             *, artifact_kind: str = "cf", task_id: str | None = None, backend: str = "ibcmd") -> dict:
    """Build and read back an artifact in disposable file infobases.

    ``inventory`` is the COMPLETE stage inventory (relative POSIX path -> SHA-256).
    The optional runner implements subprocess.run's callable contract. A failed
    command is never retried. All native state, logs and receipts stay in a newly
    created work_dir. A successful readback is technical proof, not runtime or
    business acceptance. The caller supplies the explicit pinned executable and
    declared platform version; this function never discovers a newest version.
    """
    total = _start()
    if backend not in ("ibcmd", "designer"):
        raise NativeBuildError("Unknown native build backend")
    designer = backend == "designer"
    if designer and (os.name != "nt" or artifact_kind != "cf"):
        raise NativeBuildError("Designer builds require Windows and the CF artifact kind")
    stage = _plain_path(Path(stage))
    executable = _plain_path(Path(ibcmd))
    target = _plain_path(Path(work_dir), must_exist=False)
    if not stage.is_dir() or not executable.is_file():
        raise NativeBuildError("Stage must be a directory and the platform executable must be a file")
    names = ("1cv8.exe",) if designer else ("ibcmd", "ibcmd.exe")
    if executable.name.casefold() not in names:
        raise NativeBuildError(f"An explicit {names[0]} executable is required")
    if os.path.lexists(target):
        raise NativeBuildError(f"Work directory must be new: {target}")
    if any(part.casefold() == ".git" for part in target.parts):
        raise NativeBuildError("Work directory cannot be inside repository metadata")
    if not target.parent.is_dir() or _under(target, stage) or _under(stage, target):
        raise NativeBuildError("Work directory needs an existing parent and must be separate from stage")
    if not isinstance(platform_version, str) or not re.fullmatch(r"\d+\.\d+\.\d+\.\d+", platform_version):
        raise NativeBuildError("An explicit four-part platform version is required")
    _relative(configuration_name)
    if "/" in configuration_name or not isinstance(date_stamp, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", date_stamp):
        raise NativeBuildError("Configuration name and date stamp must be safe filename components")
    if artifact_kind not in ("cf", "cfe"):
        raise NativeBuildError("artifact_kind must be cf or cfe")
    filename = f"{configuration_name}_{date_stamp}.cf"
    if artifact_kind == "cfe":
        if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", task_id):
            raise NativeBuildError("CFE requires an explicit safe task_id")
        if not re.fullmatch(r"\d{6}", date_stamp):
            raise NativeBuildError("CFE date stamp must be DDMMYY")
        import xml.etree.ElementTree as ET
        purpose = ET.parse(stage / "Configuration.xml").find(".//{*}ConfigurationExtensionPurpose")
        if purpose is None or purpose.text not in ("Customization", "AddOn", "Patch"):
            raise NativeBuildError("CFE requires an extension stage")
        suffix = re.sub(r"[^A-Za-z0-9]", "", task_id).upper()
        if not suffix:
            raise NativeBuildError("CFE task_id needs letters or digits")
        filename = f"{configuration_name}_{date_stamp}_{suffix}.cfe"
    if not isinstance(inventory, dict) or not inventory:
        raise NativeBuildError("A complete nonempty staging inventory is required")
    expected = {}
    for key, digest in inventory.items():
        key = _relative(key)
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", digest):
            raise NativeBuildError(f"Invalid SHA-256 for {key}")
        expected[key] = digest.lower()
    for paths, suffix in ((module_paths, ".bsl"), (xml_paths, ".xml")):
        if not isinstance(paths, list) or not all(isinstance(value, str) for value in paths):
            raise NativeBuildError("Readback paths must be lists of strings")
        if len(set(paths)) != len(paths):
            raise NativeBuildError("Readback paths must be lists without duplicates")
        for relative in paths:
            _relative(relative)
            if relative not in expected or not relative.casefold().endswith(suffix):
                raise NativeBuildError(f"Readback path is absent from inventory or has wrong kind: {relative}")
    if not module_paths and not xml_paths:
        raise NativeBuildError("Readback coverage must contain at least one BSL or XML path")
    before = _start()
    _verify_inventory(stage, expected)
    before = {"check": "stage-before", "status": "passed", **_finish(before)}
    target.mkdir()  # Atomic refusal if another process has created the target.
    _member(target, "logs", must_exist=False).mkdir()
    _member(target, "output", must_exist=False).mkdir()
    cf = _member(target, f"output/{filename}", must_exist=False)
    db = _member(target, "db", must_exist=False)
    data = _member(target, "data", must_exist=False)
    readback = _member(target, "readback", must_exist=False)
    receipt = {"status": "running", "workDir": str(target), "stage": str(stage),
               "file": str(cf), "platform": platform_version, "backend": backend, backend: str(executable),
               "configuration": configuration_name, "dateStamp": date_stamp,
               "artifactKind": artifact_kind, "task": task_id,
               "stageFiles": len(expected), "roundtripModules": list(module_paths),
               "roundtripMetadata": list(xml_paths), "steps": [], "checks": [before],
               "stagingUnchanged": False, "existingDatabasesChanged": False,
               "updateDBCfg": False, "runtimeAcceptance": False}
    runner = command_runner or subprocess.run
    def progress():
        _write_json(target, "build-progress.json", receipt)
    def invoke(name, args):
        timing = _start()
        error = None
        output = b""
        code = None
        result_code = None
        log = _member(target, f"logs/{name}.log", must_exist=False)
        argv = [str(executable), *args]
        kwargs = {"stdout": subprocess.PIPE, "stderr": subprocess.STDOUT,
                  "timeout": 240, "shell": False}
        if designer:
            messages = _member(target, f"logs/{name}.out", must_exist=False)
            result = _member(target, f"logs/{name}.result", must_exist=False)
            args = [*args, "/Out", str(messages), "/DumpResult", str(result)]
            argv = [str(executable), *args]
            kwargs["startupinfo"] = subprocess.STARTUPINFO(dwFlags=subprocess.STARTF_USESHOWWINDOW,
                                                           wShowWindow=subprocess.SW_HIDE)
            if args[0] == "CREATEINFOBASE":
                # 1C expects literal quotes inside File="..."; on the Windows command line.
                argv = subprocess.list2cmdline(argv[:2]) + f" {args[1]} " + subprocess.list2cmdline(args[2:])
                kwargs["executable"] = str(executable)
        elif os.name == "nt":
            kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
        try:
            completed = runner(argv, **kwargs)
            code, output = completed.returncode, completed.stdout
        except Exception as exc:
            error = exc
            output = getattr(exc, "output", None) or str(exc)
        log_text = _decode(output)
        if designer:
            if _plain_path(messages, must_exist=False).is_file():
                log_text += "\n" + messages.read_text(encoding="utf-8-sig", errors="replace")
            if _plain_path(result, must_exist=False).is_file():
                result_code = result.read_text(encoding="utf-8-sig", errors="replace").strip()
            log_text += f"\n/DumpResult {result_code or 'отсутствует'}\n"
        step = {"step": name, "args": args, "exitCode": code, "log": str(log), **_finish(timing)}
        if designer:
            step["resultCode"] = result_code
        if error is not None:
            step["error"] = f"{type(error).__name__}: {error}"
        receipt["steps"].append(step)
        with _plain_path(log, must_exist=False).open("x", encoding="utf-8", errors="replace") as handle:
            handle.write(log_text)
        progress()
        _files(target, hashes=False)
        if error is not None:
            raise NativeBuildError(f"{name} failed: {type(error).__name__}: {error}") from error
        if code != 0:
            raise NativeBuildError(f"{name} exited with code {code}; see {step['log']}")
        if designer and result_code != "0":
            raise NativeBuildError(f"{name} returned /DumpResult {result_code or 'missing'}; see {step['log']}")

    def create_designer_base(name, directory):
        invoke(name, ["CREATEINFOBASE", f'File="{directory}";', "/DisableStartupDialogs"])
        database = _member(directory, "1Cv8.1CD")
        if not database.is_file():
            raise NativeBuildError(f"{name} did not create a file infobase")

    def designer_step(name, directory, *args):
        invoke(name, ["DESIGNER", "/F", str(directory), "/DisableStartupDialogs",
                      "/DisableStartupMessages", "/DisableSplash", "/DisableBackgroundIndexBuild", *args])

    progress()
    failure = None
    readback_timing = None
    try:
        if designer:
            create_designer_base("01-create", db)
            designer_step("02-load-xml", db, "/LoadConfigFromFiles", str(stage))
            designer_step("03-save-cf", db, "/DumpCfg", str(cf))
            if not _plain_path(cf, must_exist=False).is_file() or cf.stat().st_size == 0:
                raise NativeBuildError("Designer did not produce a nonempty CF")
            check_db = _member(target, "readback-db", must_exist=False)
            create_designer_base("04-create-readback", check_db)
            designer_step("05-load-cf", check_db, "/LoadCfg", str(cf))
            readback.mkdir()
            designer_step("06-export-xml", check_db, "/DumpConfigToFiles", str(readback), "-Format", "Hierarchical")
        else:
            invoke("01-create", ["infobase", "create", "--db-path=" + str(db), "--data=" + str(data)])
            invoke("02-import", ["config", "import", "--db-path=" + str(db), "--data=" + str(data),
                                 "--out=" + str(cf), str(stage)])
            invoke("03-export", ["config", "export", "--db-path=" + str(db), "--data=" + str(data),
                                 "--file=" + str(cf), str(readback)])
        readback_timing = _start()
        cf = _member(target, cf.relative_to(target).as_posix())
        if not cf.is_file() or cf.stat().st_size == 0:
            raise NativeBuildError("Native import produced no nonempty CF")
        for relative in module_paths:
            original = _member(stage, relative)
            exported = _member(readback, relative)
            if original.read_bytes() != exported.read_bytes():
                raise NativeBuildError(f"BSL readback differs: {relative}")
        for relative in xml_paths:
            if _xml_signature(_member(stage, relative)) != _xml_signature(_member(readback, relative)):
                raise NativeBuildError(f"XML readback differs: {relative}")
        receipt.update(sha256=hashlib.sha256(cf.read_bytes()).hexdigest(), bytes=cf.stat().st_size)
        receipt["checks"].append({"check": "readback", "status": "passed", **_finish(readback_timing)})
    except Exception as exc:
        if readback_timing is not None:
            receipt["checks"].append({"check": "readback", "status": "failed", "error": str(exc),
                                      **_finish(readback_timing)})
        failure = exc
    timing = _start()
    try:
        _verify_inventory(stage, expected)
        receipt["stagingUnchanged"] = True
        receipt["checks"].append({"check": "stage-after", "status": "passed", **_finish(timing)})
    except Exception as exc:
        receipt["checks"].append({"check": "stage-after", "status": "failed", "error": str(exc), **_finish(timing)})
        failure = NativeBuildError(f"{failure}; {exc}" if failure else str(exc))
    receipt.update(_finish(total))
    receipt["status"] = "failed" if failure else "passed"
    if failure is not None:
        receipt["error"] = f"{type(failure).__name__}: {failure}"
    _write_json(target, "build-receipt.json", receipt)
    progress()
    if failure is not None:
        raise NativeBuildError(str(failure), receipt) from failure
    return receipt
