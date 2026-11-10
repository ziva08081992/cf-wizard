"""Source-only partial-CF staging with mandatory source-to-stage delivery proof."""
from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import shutil
import stat
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

HELPER = ".agents/skills/1c-partial-cf/scripts/partial_cf.py"


class StageError(ValueError):
    def __init__(self, message, receipt=None):
        super().__init__(message)
        self.receipt = receipt


def _relative(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise StageError("Expected a nonempty relative path")
    value = value.replace("\\", "/")
    if PureWindowsPath(value).drive or value.startswith("/") or any(
            part in ("", ".", "..") or ":" in part for part in value.split("/")):
        raise StageError(f"Unsafe relative path: {value}")
    return value


def _no_links(path):
    for part in (path, *path.parents):
        if not part.exists() and not part.is_symlink():
            continue
        info = part.lstat()
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 1024:
            raise StageError(f"Symlink/reparse traversal: {part}")


def _inside(root, relative):
    path = root.joinpath(*_relative(relative).split("/"))
    _no_links(path)
    return path


def _sha(path):
    _no_links(path)
    if not path.is_file():
        raise StageError(f"Expected a regular file: {path}")
    with path.open("rb") as stream:
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
        return digest.hexdigest()


def _inventory(root):
    """Every file, including ConfigDumpInfo.xml and any Git files: no exclusions."""
    _no_links(root)
    if not root.is_dir():
        raise StageError(f"Expected a directory: {root}")
    result = {}
    def failed(error):
        raise StageError(f"Cannot inventory input: {error}") from error
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=failed):
        for name in dirs + files:
            path = Path(directory) / name
            _no_links(path)
            if name in files:
                if not stat.S_ISREG(path.lstat().st_mode):
                    raise StageError(f"Nonregular input: {path}")
                result[path.relative_to(root).as_posix()] = _sha(path)
    return dict(sorted(result.items()))


def _configuration(path):
    data = path.read_bytes()
    if b"<!DOCTYPE" in data.upper():
        raise StageError("DTD is not allowed")
    root = ET.fromstring(data)
    local = lambda n: n.tag.rsplit("}", 1)[-1]
    configs = [n for n in root.iter() if local(n) == "Configuration"]
    names = [n.text for c in configs for p in c if local(p) == "Properties"
             for n in p if local(n) == "Name"]
    if len(configs) != 1 or len(names) != 1 or not names[0]:
        raise StageError("Configuration.xml must identify one configuration")
    return names[0]


def _strings(value, label):
    if not isinstance(value, list) or not value or any(
            not isinstance(v, str) or not v or v.startswith("-")
            or re.search(r"[\s,\x00]", v) for v in value) or len(set(value)) != len(value):
        raise StageError(f"Expected a nonempty unique {label} list")
    return value


def _revision(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{40}", value):
        raise StageError("Unsafe Git revision: since requires a full 40-character commit SHA1")
    return value


def metadata_coverage(stage: Path, selected: list[str]) -> list[str]:
    """Find selected owner descriptors by XML identity and cover all their XML."""
    stage = Path(stage)
    names = _strings(selected, "selected owners")
    inventory = _inventory(stage)
    descriptors = {name: [] for name in names}
    for relative in inventory:
        path = Path(relative)
        if len(path.parts) != 2 or path.suffix.lower() != ".xml":
            continue
        data = _inside(stage, relative).read_bytes()
        if b"<!DOCTYPE" in data.upper():
            raise StageError("DTD is not allowed in metadata")
        try:
            root = ET.fromstring(data)
        except ET.ParseError as exc:
            raise StageError(f"Invalid staged descriptor XML: {relative}") from exc
        local = lambda node: node.tag.rsplit("}", 1)[-1]
        if local(root) != "MetaDataObject":
            continue
        for owner in root:
            properties = [node for node in owner if local(node) == "Properties"]
            owner_names = [node.text for prop in properties for node in prop if local(node) == "Name"]
            if len(owner_names) != 1 or not owner_names[0]:
                continue
            identity = local(owner) + "." + owner_names[0]
            if identity in descriptors and path.stem == owner_names[0]:
                descriptors[identity].append(relative)
    covered = set()
    for name, matches in descriptors.items():
        if len(matches) != 1:
            raise StageError(f"Selected owner needs exactly one matching descriptor: {name}")
        descriptor = matches[0]
        covered.add(descriptor)
        prefix = descriptor[:-4] + "/"
        covered.update(relative for relative in inventory
                       if relative.startswith(prefix) and relative.lower().endswith(".xml"))
    return sorted(covered)


def _decode(value):
    return value.decode("utf-8", "replace") if isinstance(value, bytes) else str(value or "")


def prepare_stage(stage_config: dict, workspace: Path, run_dir: Path,
                  allowed_sources: set[str], command_runner=None) -> dict:
    """Run the pinned --source-only builder; native work belongs to the next gate."""
    kind = stage_config.get("kind", "cf")
    if kind == "cfe":
        from cfe_stage import prepare_cfe_stage
        return prepare_cfe_stage(stage_config, workspace, run_dir, allowed_sources)
    if kind != "cf":
        raise StageError("stage.kind must be cf or cfe")
    started = time.monotonic()
    receipt = {"status": "running", "started_at": datetime.now(timezone.utc).isoformat()}
    workspace, run_dir = Path(os.path.abspath(workspace)), Path(os.path.abspath(run_dir))
    _no_links(workspace)
    _no_links(run_dir)
    try:
        _relative(run_dir.relative_to(workspace).as_posix())
    except ValueError as exc:
        raise StageError("Run directory must stay inside the workspace") from exc
    known = {"kind", "export", "expected_selected", "objects", "since", "until", "patch", "target", "methods",
             "files", "expected_patched_whole"}
    if set(stage_config) - known:
        raise StageError("Unknown stage options; only typed source-only options are allowed")
    export = _inside(workspace, stage_config["export"])
    helper = _inside(workspace, HELPER)
    expected = _strings(stage_config.get("expected_selected"), "expected_selected")
    objects, since = stage_config.get("objects"), stage_config.get("since")
    if bool(objects) == bool(since):
        raise StageError("Choose exactly one of objects or since")
    args = []
    if objects:
        args += ["--objects", *_strings(objects, "objects")]
    else:
        args += ["--since", _revision(since)]
    if "patch" in stage_config and not isinstance(stage_config["patch"], bool):
        raise StageError("patch must be boolean")
    if not since and any(stage_config.get(k) for k in ("until", "patch", "target", "methods")):
        raise StageError("until, patch, target and methods require since")
    if "until" in stage_config:
        raise StageError("until is unsupported in this MVP: export/revision provenance is not proved")
    if stage_config.get("patch"):
        args += ["--patch"]
    if stage_config.get("methods"):
        args += ["--methods", *_strings(stage_config["methods"], "methods")]
    target = _inside(workspace, stage_config["target"]) if stage_config.get("target") else None
    if target:
        args += ["--target", str(target)]
    for root in (export, target):
        if root is not None and (run_dir == root or root in run_dir.parents or run_dir in root.parents):
            raise StageError("Run directory and source/target export must be disjoint")
    export_hashes = _inventory(export)
    target_hashes = _inventory(target) if target else {}
    if "Configuration.xml" not in export_hashes or (target and "Configuration.xml" not in target_hashes):
        raise StageError("A Designer export needs Configuration.xml")
    allowed = set()
    for value in allowed_sources:
        path = Path(value)
        if path.is_absolute():
            try:
                value = path.relative_to(workspace).as_posix()
            except ValueError as exc:
                raise StageError("Allowed source is outside workspace") from exc
        allowed.add(os.path.normcase(_relative(value)))
    files = stage_config.get("files")
    if not isinstance(files, list) or not files:
        raise StageError("Explicit nonempty production files coverage is required")
    deliveries, seen = [], set()
    for item in files:
        source, destination = _relative(item["source"]), _relative(item["target"])
        path = _inside(workspace, source)
        try:
            corresponding = path.relative_to(export).as_posix()
        except ValueError as exc:
            raise StageError("Delivery source must belong to the export root") from exc
        if os.path.normcase(source) not in allowed or destination != corresponding:
            raise StageError("Delivery source is outside scope or target differs from its export path")
        if Path(destination).suffix.lower() not in (".bsl", ".xml"):
            raise StageError("MVP delivery proof supports only BSL/XML files")
        if os.path.normcase(destination) in seen:
            raise StageError("Duplicate delivered target")
        seen.add(os.path.normcase(destination))
        deliveries.append({"source": source, "target": destination, "sha256": _sha(path)})
    production_scope = set()
    for source in allowed:
        try:
            _inside(workspace, source).relative_to(export)
        except ValueError:
            continue
        production_scope.add(source)
    if production_scope != {os.path.normcase(v["source"]) for v in deliveries}:
        raise StageError("All production sources under export need exact files coverage")
    expected_whole = stage_config.get("expected_patched_whole", [])
    if expected_whole:
        _strings(expected_whole, "expected_patched_whole")
    elif expected_whole != []:
        raise StageError("expected_patched_whole must be a list")
    input_paths = {helper}
    input_paths.update(export / p for p in export_hashes)
    if target:
        input_paths.update(target / p for p in target_hashes)
    input_hashes = {p.relative_to(workspace).as_posix(): _sha(p) for p in sorted(input_paths)}
    stage, settings, stubs = run_dir / "staging", run_dir / "merge-settings.xml", run_dir / "stubs.json"
    outputs = [stage, settings, stubs, run_dir / "stage-receipt.json", run_dir / "partial-cf.log",
               run_dir / "partial-cf-report.json", run_dir / "merge-settings.xml.stubs.json", run_dir / "tmp"]
    if any(p.exists() or p.is_symlink() for p in outputs):
        raise StageError("Stage output already exists; use a new run")
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "tmp").mkdir()
    argv = [sys.executable, "-B", str(helper), "--export", str(export), "--source-only", str(stage),
            "--settings", str(settings), *args]
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", PYTHONIOENCODING="utf-8", PYTHONUTF8="1",
               GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    env.update({key: str(run_dir / "tmp") for key in ("TMP", "TEMP", "TMPDIR")})
    receipt.update(command=argv, input_hashes=input_hashes, export_hashes=export_hashes,
                   target_hashes=target_hashes, deliveries=deliveries, log=str(run_dir / "partial-cf.log"),
                   report=str(run_dir / "partial-cf-report.json"))
    error = None
    try:
        result = (command_runner or subprocess.run)(argv, cwd=str(run_dir), env=env,
                     capture_output=True, check=False, shell=False)
        stdout, stderr = _decode(result.stdout), _decode(result.stderr)
        (run_dir / "partial-cf.log").write_text(stdout + "\nSTDERR:\n" + stderr, encoding="utf-8")
        report = json.loads(stdout)
        (run_dir / "partial-cf-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        if result.returncode != 0 or not isinstance(report, dict) or report.get("ok") is not True:
            raise StageError("Pinned source-only builder failed: " + str(report))
        for key in ("conflicts", "patch_skipped", "deleted", "root_changed"):
            if report.get(key):
                raise StageError(f"Source-only report has unresolved {key}")
        actual_whole = [v["object"] if isinstance(v, dict) else v for v in report.get("patched_whole", [])]
        if sorted(actual_whole) != sorted(expected_whole):
            raise StageError("Unexpected whole-object patch fallback")
        selected = report.get("selected")
        if not isinstance(selected, list) or sorted(selected) != sorted(expected):
            raise StageError("Actual selected objects differ from expected_selected")
        if Path(report.get("source", "")) != stage or Path(report.get("merge_settings", "")) != settings:
            raise StageError("Source-only report points outside declared outputs")
        generated_stubs = run_dir / "merge-settings.xml.stubs.json"
        if Path(report.get("stubs_file", "")) != generated_stubs:
            raise StageError("Unexpected stubs output")
        _sha(settings)
        _sha(generated_stubs)
        shutil.copyfile(generated_stubs, stubs)
        stage_hashes = _inventory(stage)
        for item in deliveries:
            if stage_hashes.get(item["target"]) != item["sha256"]:
                raise StageError("Delivered source differs from staged file: " + item["target"])
        configuration = _configuration(stage / "Configuration.xml")
        if configuration != _configuration(export / "Configuration.xml"):
            raise StageError("Configuration identity changed")
        receipt.update(stage=str(stage), stage_hashes=stage_hashes, settings=str(settings),
                       settings_sha256=_sha(settings), stubs=str(stubs), stubs_sha256=_sha(stubs),
                       configuration=configuration, selected=selected, patched=report.get("patched", []),
                       patched_whole=report.get("patched_whole", []),
                       roundtrip_modules=[v["target"] for v in deliveries if v["target"].lower().endswith(".bsl")],
                       roundtrip_xml=sorted(set(metadata_coverage(stage, selected)) |
                                            {v["target"] for v in deliveries if v["target"].lower().endswith(".xml")}))
    except Exception as exc:
        error = exc
    try:
        if _inventory(export) != export_hashes or (target and _inventory(target) != target_hashes):
            raise StageError("Source/target export inventory changed")
        if any(_sha(_inside(workspace, name)) != value for name, value in input_hashes.items()):
            raise StageError("Pinned stage inputs changed")
    except Exception as exc:
        error = StageError(f"{error}; {exc}" if error else str(exc))
    receipt.update(status="failed" if error else "passed", finished_at=datetime.now(timezone.utc).isoformat(),
                   duration_seconds=round(time.monotonic() - started, 6))
    if not (run_dir / "partial-cf.log").exists():
        (run_dir / "partial-cf.log").write_text("Execution failed: " + str(error), encoding="utf-8")
    if error:
        receipt["error"] = f"{type(error).__name__}: {error}"
    (run_dir / "stage-receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if error:
        raise StageError(str(error), receipt) from error
    return receipt
