"""Isolated adapter for dependency tracing in the pinned partial_cf script."""

from contextlib import redirect_stderr, redirect_stdout
from hashlib import sha256
import json
from pathlib import Path
import shutil
import stat
import sys
import traceback

if __package__:
    from .dependency_index import IndexReader
else:
    from dependency_index import IndexReader


HELPER_SHA256 = "efe67eda5f95475757efefd75c979d8144ffd7480934307db487155c87ea5699"
WORKER_FLAG = "--cf-wizard-helper"


def without_bsl_comments(text: str) -> str:
    """Preserve quoted text, including multiline query strings and doubled quotes."""
    chunks = []
    quoted = False
    start = 0
    index = 0
    while index < len(text):
        if text[index] == '"':
            if quoted and text[index:index + 2] == '""':
                index += 2
                continue
            quoted = not quoted
        elif not quoted and text.startswith("//", index):
            chunks.append(text[start:index])
            end = text.find("\n", index)
            if end < 0:
                return "".join(chunks)
            start = index = end
        index += 1
    chunks.append(text[start:])
    return "".join(chunks)


def _instrument(source: str) -> str:
    # The anchors are verified against the same pinned revision as the bundled helper.
    changes = (
        ("    def note(r, owner):\n", "    comment_reference = False\n\n    def note(r, owner):\n"),
        ("        s = res.stubs.get(key)\n",
         "        if not _wizard_dependency(key, owner, not comment_reference):\n"
         "            return\n"
         "        s = res.stubs.get(key)\n"),
        (
            '    def scan_file(path, owner):\n'
            '        if path.endswith(".bsl"):\n'
            '            for key, p in module_values(read_file(path)):\n'
            '                walk({key}, p, 0, owner, 0) if p else note(Ref(*split_key(canonical(key))), owner)\n'
            '            return\n',
            '    def scan_file(path, owner):\n'
            '        nonlocal comment_reference\n'
            '        if path.endswith(".bsl"):\n'
            '            text = read_file(path)\n'
            '            values = module_values(text)\n'
            '            code = _wizard_without_comments(text) if values else text\n'
            '            actual = {(key, tuple(p or ())) for key, p in module_values(code)} if code != text else None\n'
            '            for key, p in values:\n'
            '                comment_reference = actual is not None and (key, tuple(p or ())) not in actual\n'
            '                try:\n'
            '                    walk({key}, p, 0, owner, 0) if p else note(Ref(*split_key(canonical(key))), owner)\n'
            '                finally:\n'
            '                    comment_reference = False\n'
            '            return\n'
        ),
    )
    return _replace_anchors(source, changes)


def _replace_anchors(source, changes):
    for old, new in changes:
        if source.count(old) != 1:
            raise ValueError("Структура закреплённого сборщика изменилась.")
        source = source.replace(old, new, 1)
    return source


def _instrument_index(source: str) -> str:
    return _replace_anchors(source, (
        (
            '    def members(key):\n        m = index.get(key)\n        if m is None:\n'
            '            m = {}\n            text = descriptor(key)\n',
            '    def members(key):\n        m = index.get(key)\n        if m is None:\n'
            '            m = {}\n            text = descriptor(key)\n'
            '            facts = _wizard_index.object(key, text, Member)\n'
            '            if facts is not None:\n'
            '                index[key] = facts[1]\n'
            '                return facts[1]\n',
        ),
        (
            '    def defined_types(key):\n        text = descriptor(key)\n',
            '    def defined_types(key):\n        text = descriptor(key)\n'
            '        facts = _wizard_index.object(key, text, Member)\n'
            '        if facts is not None:\n            return facts[0]\n',
        ),
        (
            '        if registrars is None:\n            registrars = {}\n',
            '        if registrars is None:\n'
            '            docs = _wizard_index.registrars(key, src, native, _listing, _wizard_read_bytes)\n'
            '            if docs is not None:\n                return docs\n'
            '            registrars = {}\n',
        ),
    ))


def _trace_dependencies(excluded: frozenset[str]):
    causes: dict[str, dict[str, bool]] = {}

    def dependency(name: str, owner: str, required: bool) -> bool:
        # Keep the stub's own members without treating self-references as inclusion causes.
        if name == owner:
            return True
        owners = causes.setdefault(name, {})
        owners[owner] = owners.get(owner, False) or required
        return required or name not in excluded

    return causes, dependency


def _helper_source(helper: Path) -> str:
    data = helper.read_bytes()
    if sha256(data).hexdigest() != HELPER_SHA256:
        raise ValueError("Контрольная сумма partial_cf.py отличается от закреплённой.")
    return _instrument(data.decode("utf-8-sig").replace("\r\n", "\n"))


def _index_message(index, path: Path | None) -> str:
    if index is not None:
        hits = index.hits
        return (f"Индекс SQLite: описаний {hits['objects']}, запросов регистраторов {hits['registrars']}, "
                f"переходов на XML {hits['fallback']}.")
    return "Индекс SQLite: не удалось открыть. Используются исходные XML." if path else ""


def _guard_export_reads(namespace, source: Path, git=None) -> dict[str, str]:
    from stage import _no_links

    root = Path(namespace["native"](str(source), ""))
    listing = namespace["_listing"]
    checked = set()
    hashes = {}

    def check(path):
        path = Path(path)
        if path not in checked:
            if ".." in path.parts or not path.is_relative_to(root):
                raise ValueError(f"Путь выходит за пределы XML-выгрузки: {path}")
            _no_links(path)
            checked.add(path)
        return path

    def checked_bytes(path):
        if git is not None:
            relative = git.relative(path)
            path = git.ensure(relative)
        else:
            path = check(path)
            relative = path.relative_to(root).as_posix()
        data = path.read_bytes()
        if git is not None:
            git.verify(relative, data)
        digest = sha256(data).hexdigest()
        if hashes.setdefault(relative, digest) != digest:
            raise ValueError(f"XML-выгрузка изменилась во время чтения: {relative}")
        return data

    def checked_listing(path):
        if git is not None:
            return git.listing(path)
        check(path)
        entries = listing(path)
        for entry in entries:
            info = entry.stat(follow_symlinks=False)
            if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 1024:
                raise ValueError(f"Ссылка или точка повторного анализа в выгрузке: {entry.path}")
            checked.add(Path(entry.path))
        return entries

    namespace.update(read_file=lambda path: checked_bytes(path).decode("utf-8", "surrogateescape"),
                     _listing=checked_listing, _wizard_read_bytes=checked_bytes)
    if git is not None:
        def module_has_text(path):
            relative = git.relative(path)
            return git.blob(relative) is not None and bool(checked_bytes(path).decode(
                "utf-8", "surrogateescape").lstrip("\ufeff").strip(" \t\r\n\x0b\x0c"))

        namespace["has_text"] = module_has_text
        def copy_verified(rel, destination):
            shutil.copyfile(git.ensure(rel), destination)
            git.verify(rel, Path(destination).read_bytes())

        namespace["_wizard_copy"] = copy_verified
        namespace["_wizard_has_blob"] = lambda rel: git.blob(rel) is not None
    return hashes


class DependencyPlan:
    """A checked selection and the suspended writer of the pinned helper."""

    def __init__(self, writer, namespace, files, hashes, result):
        self.writer = writer
        self.namespace = namespace
        self.files = files
        self.hashes = hashes
        self.result = result

    def write(self, stage: Path, settings: Path) -> None:
        try:
            self.writer.send(str(stage))
        except StopIteration as finished:
            res = finished.value
        else:
            raise ValueError("Сборщик не завершил запись XML-комплекта.")
        write = self.namespace["write_file"]
        write(str(settings), self.namespace["merge_settings"](res))
        stubs_file = settings.with_name(settings.name + ".stubs.json")
        write(str(stubs_file), json.dumps(self.result["stubs"], ensure_ascii=False, indent=1))

    def close(self) -> None:
        self.writer.close()
        self.namespace["_children_cache"].clear()


def resolve_dependencies(helper: Path, source: Path, selected: tuple[str, ...],
                         index_path: Path | None, identity: str,
                         excluded: frozenset[str] = frozenset(), *, git=None) -> DependencyPlan:
    causes, dependency = _trace_dependencies(excluded)
    text = _helper_source(helper)
    # The pinned helper has finished dependency closure at this point. Reuse
    # its read-only predefined-code check and return before any file writes.
    warning_start = text.index('    for key in sorted(included()):\n        kind, name = split_key(key)\n'
                               '        rel = TYPE_FOLDERS[kind] + "/" + name + "/Ext/Predefined.xml"\n')
    warning_end = text.index('    cfg = read_file(native(src, "Configuration.xml"))\n', warning_start)
    warnings = text[warning_start:warning_end]
    before_write = "    # Write: chosen objects whole, stubs, languages, root, subsystem.\n"
    text = _replace_anchors(text, (
        ("    if os.path.exists(long_path(dst)) and os.listdir(long_path(dst)):\n",
         "    if dst is not None and os.path.exists(long_path(dst)) and os.listdir(long_path(dst)):\n"),
        (warnings, ""),
        (before_write, warnings + "    dst = yield res, dict(below_cache)\n" + before_write),
    ))
    namespace = {"__name__": "partial_cf_dependencies", "__file__": str(helper),
                 "_wizard_dependency": dependency, "_wizard_without_comments": without_bsl_comments}
    index = IndexReader.open(index_path, identity, HELPER_SHA256, pinned=git is not None) if index_path else None
    if git is not None and index is None:
        raise ValueError("Создайте или обновите индекс выбранной ревизии Git.")
    try:
        if index is not None:
            text = _instrument_index(text)
            namespace["_wizard_index"] = index
        if git is not None:
            text = _replace_anchors(text, (
                ("        shutil.copyfile(native(src, rel), native(dst, rel))\n",
                 "        _wizard_copy(rel, native(dst, rel))\n"),
                ("            for rel in below(key):\n                if rel.endswith(KEPT_MODULES):\n",
                 "            for rel in (below(key) if keep else ()):\n                if rel.endswith(KEPT_MODULES):\n"),
                ("        if rel in below(key):\n            items = predefined_codes(",
                 "        if _wizard_has_blob(rel):\n            items = predefined_codes("),
            ))
        exec(compile(text, str(helper), "exec"), namespace)
        hashes = _guard_export_reads(namespace, source, git)
        writer = namespace["build"](str(source), None, selected)
        result, files = next(writer)
        namespace["_children_cache"].clear()
        return DependencyPlan(writer, namespace, files, hashes, {
            "selected": result.selected, "causes": causes,
            "stubs": [{"object": name, "referred_by": stub.reason} for name, stub in sorted(result.stubs.items())],
            "predefined_code_mismatch": result.code_mismatch, "index_message": _index_message(index, index_path),
        })
    finally:
        if index is not None:
            index.close()


def run_job(path: Path) -> int:
    request = json.loads(path.read_text(encoding="utf-8"))
    helper = Path(request["helper"])
    causes, dependency = _trace_dependencies(frozenset(request["excluded"]))
    with (Path(request["stdout"]).open("w", encoding="utf-8") as stdout,
          Path(request["stderr"]).open("w", encoding="utf-8") as stderr,
          redirect_stdout(stdout), redirect_stderr(stderr)):
        previous = sys.argv
        index = None
        try:
            sys.argv = [str(helper), *request["arguments"]]
            source = _helper_source(helper)
            namespace = {"__name__": "__main__", "__file__": str(helper),
                         "_wizard_dependency": dependency, "_wizard_without_comments": without_bsl_comments,
                         "_wizard_read_bytes": lambda path: Path(path).read_bytes()}
            if index_path := request.get("index"):
                index = IndexReader.open(Path(index_path), request["index_identity"], HELPER_SHA256)
                if index is not None:
                    source = _instrument_index(source)
                    namespace["_wizard_index"] = index
                else:
                    print(_index_message(index, Path(index_path)), file=stderr)
            try:
                exec(compile(source, str(helper), "exec"), namespace)
            except SystemExit as exc:
                code = exc.code or 0
            else:
                code = 0
            if code == 0:
                Path(request["causes"]).write_text(json.dumps(causes, ensure_ascii=False), encoding="utf-8")
            return int(code)
        except Exception:
            traceback.print_exc()
            return 1
        finally:
            if index is not None:
                print(_index_message(index, None), file=stderr)
                index.close()
            sys.argv = previous


if __name__ == "__main__":
    raise SystemExit(run_job(Path(sys.argv[1])))
