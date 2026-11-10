"""Package the Windows Qt wizard as a single executable."""

import argparse
import ast
from hashlib import sha256
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from cf_wizard.adapters import BUNDLED_HELPER_ENTRY, HELPER_SHA256, WORKSPACE_HELPER


def helper_imports(source: bytes) -> list[str]:
    imports = set()
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            imports.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and not node.level and node.module:
            imports.add(node.module)
    return sorted(imports)


def build_exe(destination: Path) -> None:
    if os.name != "nt" or destination.suffix.casefold() != ".exe":
        raise ValueError("Укажите путь к EXE и запустите сборку на Windows.")
    if destination.exists():
        raise FileExistsError(destination)
    helper = WORKSPACE_HELPER.read_bytes()
    if sha256(helper).hexdigest() != HELPER_SHA256:
        raise ValueError("Контрольная сумма partial_cf.py отличается от закреплённой.")
    entrypoint = Path(__file__).with_name("cf_wizard_qt.py").resolve()
    with tempfile.TemporaryDirectory(prefix=".cf-exe-", dir=destination.parent, ignore_cleanup_errors=True) as name:
        work = Path(name)
        args = [sys.executable, "-B", "-m", "PyInstaller", "--onefile", "--windowed", "--noupx",
                "--name", destination.stem, "--distpath", str(work / "dist"), "--workpath", str(work / "build"),
                "--specpath", str(work), "--add-data", str(WORKSPACE_HELPER) + os.pathsep
                + Path(BUNDLED_HELPER_ENTRY).parent.as_posix(),
                "--add-data", str(entrypoint.parent / "cf_wizard" / "ui") + os.pathsep + "cf_wizard/ui"]
        for module in helper_imports(helper):
            args.extend(("--hidden-import", module))
        args.append(str(entrypoint))
        windows = Path(os.environ["SystemRoot"])
        # Resolve Windows DLLs before unrelated tools on the developer's PATH.
        search_path = os.pathsep.join((str(windows / "System32"), str(windows)))
        environment = dict(os.environ, PYINSTALLER_CONFIG_DIR=str(work / "cache"), PATH=search_path)
        subprocess.run(args, cwd=entrypoint.parent, env=environment, check=True)
        (work / "dist" / destination.name).rename(destination)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    args = parser.parse_args()
    build_exe(args.exe.resolve())
    print(args.exe.resolve())


if __name__ == "__main__":
    main()
