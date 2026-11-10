"""Native Gitea settings form. Credentials are collected in Qt."""

from collections.abc import Callable

from PySide6.QtGui import QColor, QPalette
from PySide6.QtWidgets import (
    QAbstractSpinBox, QCheckBox, QComboBox, QDialog, QFileDialog, QGridLayout, QHBoxLayout, QLabel,
    QLineEdit, QPushButton, QSpinBox, QVBoxLayout, QWidget,
)

from .paths import dialog_location, normalize_path


STYLE = """
QDialog { background: #1e1f22; color: #e1e3e8; }
QLabel, QCheckBox { color: #e1e3e8; }
QLabel[heading="true"] { font-weight: 600; }
QLabel[hint="true"] { color: #a7abb6; }
QLabel#gitTitle, QWidget#gitFooter { background: #2b2d30; }
QLabel#gitTitle { padding: 10px 14px; font-weight: 600; border-bottom: 1px solid #3b3e45; }
QWidget#gitFooter { border-top: 1px solid #3b3e45; }
QLineEdit, QComboBox, QSpinBox {
    background: #1b1c1f; color: #e1e3e8; border: 1px solid #3b3e45;
    border-radius: 5px; padding: 3px 7px; min-height: 24px;
    selection-background-color: #273b60; selection-color: #e1e3e8;
}
QComboBox QLineEdit { border: none; padding: 0; }
QLineEdit:focus, QComboBox:focus, QSpinBox:focus { border-color: #6195f6; }
QPushButton {
    color: #e1e3e8; background: #2b2d30; border: 1px solid #3b3e45;
    border-radius: 5px; padding: 4px 10px; min-height: 22px;
}
QPushButton:hover { background: #353840; }
QPushButton:focus { border-color: #6195f6; }
QPushButton#gitConnect { background: #2e67d0; color: white; font-weight: 600; }
QPushButton#gitConnect:hover { background: #3574e9; }
"""


class GitConnectionDialog(QDialog):
    def __init__(self, parent: QWidget, settings: dict, branches: list[str], forget: Callable[[], None]):
        super().__init__(parent)
        self.setWindowTitle("Подключение Gitea")
        self.setMinimumWidth(540)
        self._forget = forget

        palette = self.palette()
        for role, color in {
            QPalette.ColorRole.Window: "#1e1f22", QPalette.ColorRole.Base: "#1b1c1f",
            QPalette.ColorRole.WindowText: "#e1e3e8", QPalette.ColorRole.Text: "#e1e3e8",
            QPalette.ColorRole.Button: "#2b2d30", QPalette.ColorRole.ButtonText: "#e1e3e8",
            QPalette.ColorRole.Highlight: "#273b60", QPalette.ColorRole.HighlightedText: "#ffffff",
            QPalette.ColorRole.PlaceholderText: "#a7abb6",
        }.items():
            palette.setColor(role, QColor(color))
        self.setPalette(palette)
        self.setStyleSheet(STYLE)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        title = QLabel("Подключение Gitea")
        title.setObjectName("gitTitle")
        layout.addWidget(title)
        body = QWidget()
        layout.addWidget(body)
        content = QVBoxLayout(body)
        content.setContentsMargins(14, 12, 14, 12)
        content.setSpacing(8)

        repository = QLabel("Репозиторий")
        repository.setProperty("heading", True)
        content.addWidget(repository)
        grid = QGridLayout()
        grid.setHorizontalSpacing(10)
        grid.setVerticalSpacing(4)
        grid.setColumnStretch(0, 1)
        grid.setColumnStretch(1, 1)
        content.addLayout(grid)
        self.fields = {name: QLineEdit(str(settings[name])) for name in ("server", "repository", "directory", "cache")}

        def add_field(label: str, widget: QWidget, row: int, column: int = 0, span: int = 1):
            caption = QLabel(label)
            caption.setBuddy(widget)
            grid.addWidget(caption, row, column, 1, span)
            grid.addWidget(widget, row + 1, column, 1, span)

        add_field("Адрес HTTPS", self.fields["server"], 0, span=2)
        add_field("Репозиторий", self.fields["repository"], 2)
        add_field("Каталог выгрузки", self.fields["directory"], 2, 1)
        self.branch = QComboBox()
        self.branch.setEditable(True)
        self.branch.addItems(branches)
        self.branch.setCurrentText(str(settings["branch"]))
        add_field("Ветка при подключении", self.branch, 4)
        self.interval = QSpinBox()
        self.interval.setButtonSymbols(QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.interval.setRange(0, 86400)
        self.interval.setValue(int(settings["interval"]))
        self.interval.setToolTip("0 выключает опрос. Активный интервал от 30 до 86400 секунд.")
        add_field("Проверять SHA, сек.", self.interval, 4, 1)
        hint = QLabel("0 — автоматическая проверка отключена")
        hint.setProperty("hint", True)
        grid.addWidget(hint, 6, 0, 1, 2)

        cache_label = QLabel("Папка кэша")
        cache_label.setBuddy(self.fields["cache"])
        grid.addWidget(cache_label, 7, 0, 1, 2)
        cache_row = QHBoxLayout()
        cache_row.addWidget(self.fields["cache"], 1)
        browse = QPushButton("Выбрать…")
        browse.setAutoDefault(False)
        browse.clicked.connect(self._choose_cache)
        cache_row.addWidget(browse)
        grid.addLayout(cache_row, 8, 0, 1, 2)

        content.addSpacing(6)
        access = QLabel("Доступ")
        access.setProperty("heading", True)
        content.addWidget(access)
        self.secret = QLineEdit()
        self.secret.setEchoMode(QLineEdit.EchoMode.Password)
        self.secret.setPlaceholderText("Вставьте токен или оставьте пустым для сохранённого")
        token_label = QLabel("Персональный токен")
        token_label.setBuddy(self.secret)
        content.addWidget(token_label)
        content.addWidget(self.secret)
        self.remember = QCheckBox("Запомнить на этом компьютере")
        self.remember.setPalette(palette)
        content.addWidget(self.remember)
        self.hint = QLabel("Хранение в Диспетчере учётных данных Windows")
        self.hint.setProperty("hint", True)
        self.hint.setWordWrap(True)
        content.addWidget(self.hint)

        footer = QWidget()
        footer.setObjectName("gitFooter")
        layout.addWidget(footer)
        actions = QHBoxLayout(footer)
        actions.setContentsMargins(14, 9, 14, 9)
        delete = QPushButton("Удалить токен")
        delete.setToolTip(f"Удалить сохранённый токен для {settings['server']}.")
        delete.setAutoDefault(False)
        delete.clicked.connect(self._delete_token)
        cancel = QPushButton("Отмена")
        cancel.setAutoDefault(False)
        cancel.clicked.connect(self.reject)
        connect = QPushButton("Подключить")
        connect.setObjectName("gitConnect")
        connect.setDefault(True)
        connect.clicked.connect(self.accept)
        actions.addWidget(delete)
        actions.addStretch()
        actions.addWidget(cancel)
        actions.addWidget(connect)
        self.resize(620, self.sizeHint().height())
        self.fields["server"].setFocus()

    def connection_settings(self) -> dict:
        return {**{name: field.text().strip() for name, field in self.fields.items()},
                "branch": self.branch.currentText().strip(), "interval": self.interval.value()}

    def _choose_cache(self):
        field = self.fields["cache"]
        location = dialog_location(field.text()).get("initialdir", "")
        folder = QFileDialog.getExistingDirectory(self, "Папка кэша Gitea", location)
        if folder:
            field.setText(normalize_path(folder))

    def _delete_token(self):
        try:
            self._forget()
        except (OSError, ValueError) as error:
            self.hint.setText(str(error))
            return
        self.secret.clear()
        self.remember.setChecked(False)
        self.hint.setText("Локальный токен Gitea удалён.")
