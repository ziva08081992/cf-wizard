"""Keep WebEngine runtime files, omit its optional DevTools debug source map."""
from pathlib import Path
from PyInstaller.utils.hooks.qt import add_qt6_dependencies, pyside6_library_info

hiddenimports, binaries, datas = add_qt6_dependencies(__file__)
engine_binaries, engine_datas = pyside6_library_info.collect_qtwebengine_files()
binaries += engine_binaries
debug_name = 'qtwebengine_devtools_resources.debug.pak'
for source, destination in engine_datas:
    folder = Path(source)
    if folder.is_dir() and (folder / debug_name).is_file():
        # PyInstaller supplies the entire resources directory, so filtering
        # just the source tuple's basename would still include the debug pak.
        datas += [(str(file), str(Path(destination) / file.relative_to(folder).parent))
                  for file in folder.rglob('*') if file.is_file() and file.name != debug_name]
    elif folder.name != debug_name:
        datas.append((source, destination))
hiddenimports += ['PySide6.QtPrintSupport']
