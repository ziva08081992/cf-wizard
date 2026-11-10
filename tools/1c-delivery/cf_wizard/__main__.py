"""Composition root for the portable Windows wizard."""

import argparse
from pathlib import Path
import tkinter as tk
from tkinter import filedialog, messagebox

from .adapters import DEFAULT_HELPER, DesignerExport, TechnicalPackage, discover_ibcmd
from .application import Wizard
from .paths import PathSettingsFile
from .view import WizardWindow


def main():
    parser = argparse.ArgumentParser(description="Мастер технической CF")
    parser.add_argument("--helper", type=Path, help="Путь к закреплённому partial_cf.py")
    args = parser.parse_args()

    root = tk.Tk()
    root.withdraw()
    helper = args.helper or DEFAULT_HELPER
    if args.helper is None and not helper.is_file():
        path = filedialog.askopenfilename(parent=root, title="Выберите закреплённый partial_cf.py",
                                          filetypes=[("partial_cf.py", "partial_cf.py")])
        if not path:
            root.destroy()
            return 0
        helper = Path(path)
    try:
        reader = DesignerExport(helper)
    except (OSError, ValueError) as exc:
        messagebox.showerror("Мастер технической CF", str(exc), parent=root)
        root.destroy()
        return 1

    platforms, default_platform = discover_ibcmd()
    settings = PathSettingsFile.for_user()
    default_logs = settings.path.parent / "logs"
    WizardWindow(root, Wizard(reader, TechnicalPackage(reader, default_logs)), platforms, default_platform,
                 paths=settings.load(), save_paths=settings.save, default_logs=default_logs)
    root.deiconify()
    root.mainloop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
