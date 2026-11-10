"""Package the wizard and a local Windows CPython/Tk runtime without an installer."""

import argparse
from hashlib import sha256
import os
from pathlib import Path
import sys
import tempfile
from zipfile import ZIP_DEFLATED, ZipFile

from cf_wizard.adapters import BUNDLED_HELPER_ENTRY, HELPER_SHA256, WORKSPACE_HELPER


OMIT_DIRS = {"__pycache__", "site-packages", "test", "tests", "ensurepip", "idlelib", "venv"}


def add_runtime(archive: ZipFile) -> None:
    home = Path(sys.base_prefix)
    abi = f"python{sys.version_info.major}{sys.version_info.minor}"
    for name in ("python.exe", "python3.dll", abi + ".dll", "LICENSE.txt"):
        archive.write(home / name, "runtime/" + name)
    for path in sorted(home.glob("vcruntime*.dll")):
        archive.write(path, f"runtime/{path.name}")
    for name in ("DLLs", "Lib", "tcl"):
        root = home / name
        if not root.is_dir():
            raise ValueError(f"Установленный Python не содержит {root}")
        for directory, dirs, files in os.walk(root):
            dirs[:] = sorted(d for d in dirs if d not in OMIT_DIRS)
            for filename in sorted(files):
                path = Path(directory) / filename
                if path.suffix.lower() not in (".pyc", ".pyo", ".lib"):
                    archive.write(path, "runtime/" + path.relative_to(home).as_posix())
    archive.writestr(f"runtime/{abi}._pth", ".\nDLLs\nLib\n..\n")


def build_archive(destination: Path, *, include_helper: bool = False) -> None:
    if os.name != "nt":
        raise ValueError("Портативный комплект собирается на Windows.")
    if destination.exists():
        raise FileExistsError(destination)
    helper = WORKSPACE_HELPER.read_bytes() if include_helper else None
    if helper is not None and sha256(helper).hexdigest() != HELPER_SHA256:
        raise ValueError("Контрольная сумма partial_cf.py отличается от закреплённой.")
    root = Path(__file__).resolve().parent
    fd, temporary = tempfile.mkstemp(suffix=".zip", dir=destination.parent)
    os.close(fd)
    draft = Path(temporary)
    try:
        with ZipFile(draft, "w", ZIP_DEFLATED, compresslevel=6) as archive:
            for name in ("Start-CfWizard.cmd", "native.py", "stage.py"):
                archive.write(root / name, name)
            archive.write(root / "README-wizard.md", "README.md")
            for path in sorted((root / "cf_wizard").glob("*.py")):
                if not path.stem.startswith("dump_index"):
                    archive.write(path, path.relative_to(root).as_posix())
            if helper is not None:
                archive.writestr(BUNDLED_HELPER_ENTRY, helper)
            add_runtime(archive)
        draft.rename(destination)
    finally:
        draft.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("zip", type=Path)
    parser.add_argument("--include-helper", action="store_true", help="Вложить закреплённый сборщик в локальный ZIP")
    args = parser.parse_args()
    build_archive(args.zip.resolve(), include_helper=args.include_helper)
    print(args.zip.resolve())


if __name__ == "__main__":
    main()
