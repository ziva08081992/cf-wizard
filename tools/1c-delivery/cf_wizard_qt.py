"""Entry point for the Qt desktop app and the isolated helper subprocess."""

import argparse
from pathlib import Path
import sys

from cf_wizard.helper_worker import WORKER_FLAG, run_job


def main() -> int:
    if sys.argv[1:2] == [WORKER_FLAG]:
        return run_job(Path(sys.argv[2])) if len(sys.argv) == 3 else 2

    from cf_wizard.journals import ErrorJournal
    from cf_wizard.paths import PathSettingsFile

    settings = PathSettingsFile.for_user()
    paths = settings.load()
    logs = settings.path.parent / "logs"
    diagnostics = ErrorJournal(logs)
    diagnostics.set_directory(paths.values.get("logs", ""), paths.values.get("export", ""))
    diagnostics.install()

    from PySide6.QtCore import qVersion
    from PySide6.QtWidgets import QApplication, QFileDialog, QMessageBox

    from cf_wizard.adapters import DEFAULT_HELPER, DesignerDependencyIndexes, DesignerExport, TechnicalPackage, discover_build_platforms
    from cf_wizard.application import Wizard
    from cf_wizard.qt_view import QtWizardWindow

    parser = argparse.ArgumentParser(description="Мастер технической CF")
    parser.add_argument("--helper", type=Path, help="Путь к закреплённому partial_cf.py")
    args = parser.parse_args()
    diagnostics.step(f"Запуск интерфейса Qt {qVersion()}.")
    app = QApplication([sys.argv[0]])
    app.setStyle("Fusion")
    helper = args.helper or DEFAULT_HELPER
    if args.helper is None and not helper.is_file():
        name, _ = QFileDialog.getOpenFileName(None, "Выберите partial_cf.py", "", "partial_cf.py (partial_cf.py)")
        if not name:
            return 0
        helper = Path(name)
    try:
        reader = DesignerExport(helper)
    except (OSError, ValueError) as exc:
        report = diagnostics.error("Проверка сборщика partial_cf.py.", exc)
        message = str(exc) + (f"\n\nЖурнал ошибок {report}" if report else "")
        QMessageBox.critical(None, "Мастер технической CF", message)
        return 1
    platforms = discover_build_platforms((paths.values.get("platform", ""), paths.values.get("designer", "")))
    indexes = DesignerDependencyIndexes(reader, settings.path.parent / "indexes", logs)
    window = QtWizardWindow(Wizard(reader, TechnicalPackage(reader, logs), indexes), platforms,
                            paths, settings.save, logs, diagnostics)
    window.show()
    # Opt-in smoke probe for the frozen distribution, using an isolated profile.
    import os
    if report := os.environ.get("CF_WIZARD_STARTUP_REPORT"):
        import json
        from PySide6.QtCore import QTimer
        from cf_wizard import __version__

        def report_ready(ok):
            Path(report).write_text(json.dumps({"version": __version__, "webengine_loaded": bool(ok)}), encoding="utf8")
            QTimer.singleShot(100, app.quit)

        window.view.loadFinished.connect(report_ready)
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
