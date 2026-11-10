"""The XML export page and the prepared Designer startup keys."""

import os
from pathlib import Path
import tempfile
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

from .dump_indexes import CHECK_UPDATE, EXPORT_LIST, INDEX_NAME, IndexWizard
from .presentation import object_label
from .ui_tasks import run_in_background
from .ui_tooltip import bind_tooltip


CHANGE_NAMES = {
    "added": "Добавлено",
    "changed": "Изменено",
    "removed": "Удалено",
    "renamed": "Переименовано",
}
DISPLAY_LIMIT = 1500
HELP_TEXT = (
    "Сравните ConfigDumpInfo.xml рабочей выгрузки и той же конфигурации из базы. "
    "Мастер сохранит список объектов и покажет готовый ключ, если по списку есть что выгружать.\n\n"
    "Копировать -listFile\n"
    "Выполните ключ в Конфигураторе, чтобы получить XML перечисленных объектов в отдельной папке delta.\n\n"
    "Копировать -getChanges\n"
    "Выполните этот ключ первым, если хотите обновить рабочую папку. 1С подготовит отчёт о различиях.\n\n"
    "Проверить ответ 1С и скопировать -update\n"
    "Нажмите после завершения -getChanges. При успешной проверке выполните новый ключ в Конфигураторе. "
    "Он обновит рабочие XML и ConfigDumpInfo.xml. При FullDump или ошибке ключ блокируется.\n\n"
    "Открыть папку сравнения\n"
    "Здесь лежат dump-objects.txt и changes.json. В папке logs будут журналы и коды завершения Конфигуратора. "
    "Подробная инструкция есть в README.md из комплекта."
)


class DumpIndexPage:
    def __init__(self, parent, wizard: IndexWizard):
        self.wizard, self.page = wizard, parent
        self.busy = False
        self.plan = None
        self.working = tk.StringVar(parent)
        self.baseline = tk.StringVar(parent)
        self.current = tk.StringVar(parent)
        self.destination = tk.StringVar(parent, value=tempfile.gettempdir())
        self.arguments = tk.StringVar(parent)
        self.status = tk.StringVar(parent, value="Выберите рабочую XML-папку или пустую папку для первой выгрузки.")

        body = ttk.Frame(parent, padding=14)
        body.pack(fill="both", expand=True)
        ttk.Label(body, text="Обновление XML-выгрузки", font=("Segoe UI", 16, "bold")).pack(anchor="w")
        ttk.Label(body, text="Мастер готовит ключи запуска. Скопируйте ключ и запустите Конфигуратор нужной базы.").pack(
            anchor="w", pady=(2, 10))

        fields = ttk.Frame(body)
        fields.pack(fill="x")
        fields.columnconfigure(1, weight=1)
        self.inputs = []
        self.path_row(fields, 0, "Рабочая папка XML-выгрузки", self.working, directory=True)
        self.path_row(fields, 1, "ConfigDumpInfo.xml из рабочей папки", self.baseline)
        self.path_row(fields, 2, "ConfigDumpInfo.xml из базы 1С", self.current)
        self.path_row(fields, 3, "Папка для отчётов и списка объектов", self.destination, directory=True)

        actions = ttk.Frame(body)
        actions.pack(fill="x", pady=(7, 9))
        self.first_button = ttk.Button(actions, text="Копировать ключ первой выгрузки", command=self.first_export)
        self.index_button = ttk.Button(actions, text="Папка для индекса и ключ выгрузки…", command=self.prepare_export)
        self.compare_button = ttk.Button(actions, text="Сравнить с рабочей выгрузкой", command=self.compare)
        self.help_button = ttk.Button(actions, text="Справка", command=self.show_help)
        for button, hint in (
            (self.first_button, "Подготовит ключ для полной XML-выгрузки в пустую рабочую папку."),
            (self.index_button, "Подготовит ключ для выгрузки только ConfigDumpInfo.xml из базы."),
            (self.compare_button, "Сравнит индексы и сохранит отчёт и список объектов для выгрузки."),
            (self.help_button, "Покажет, как получить отдельную выгрузку или обновить рабочую папку."),
        ):
            button.pack(side="left", padx=(0, 7))
            bind_tooltip(button, hint)

        table = ttk.Frame(body)
        table.pack(fill="both", expand=True)
        table.columnconfigure(0, weight=1)
        table.rowconfigure(0, weight=1)
        self.changes = ttk.Treeview(table, columns=("kind", "object"), show="headings")
        self.changes.heading("kind", text="Изменение")
        self.changes.heading("object", text="Адрес метаданных")
        self.changes.column("kind", width=130, stretch=False)
        self.changes.column("object", width=660)
        scroll = ttk.Scrollbar(table, orient="vertical", command=self.changes.yview)
        self.changes.configure(yscrollcommand=scroll.set)
        self.changes.grid(row=0, column=0, sticky="nsew")
        scroll.grid(row=0, column=1, sticky="ns")

        commands = ttk.LabelFrame(body, text="Ключи для Конфигуратора", padding=8)
        commands.pack(fill="x", pady=(9, 0))
        command_buttons = ttk.Frame(commands)
        command_buttons.pack(fill="x")
        self.list_button = ttk.Button(command_buttons, text="Копировать -listFile",
                                      command=lambda: self.copy_plan_command(EXPORT_LIST))
        self.check_button = ttk.Button(command_buttons, text="Копировать -getChanges",
                                       command=lambda: self.copy_plan_command(CHECK_UPDATE))
        self.update_button = ttk.Button(command_buttons, text="Проверить ответ 1С и скопировать -update",
                                        command=self.copy_update)
        self.open_button = ttk.Button(command_buttons, text="Открыть папку сравнения", command=self.open_result)
        for button, hint in (
            (self.list_button, "Скопирует ключ выгрузки объектов из списка в отдельную папку delta."),
            (self.check_button, "Скопирует ключ проверки 1С перед обновлением рабочей XML-папки."),
            (self.update_button, "После -getChanges проверит отчёт 1С и при успехе скопирует ключ обновления."),
            (self.open_button, "Откроет папку со списком объектов, отчётом и результатами проверки 1С."),
        ):
            button.pack(side="left", padx=(0, 7))
            bind_tooltip(button, hint)
        ttk.Entry(commands, textvariable=self.arguments, state="readonly").pack(fill="x", pady=(6, 0))

        ttk.Label(body, textvariable=self.status, wraplength=790).pack(fill="x", pady=(7, 0))
        self.working.trace_add("write", self.working_changed)
        for variable in (self.baseline, self.current, self.destination):
            variable.trace_add("write", self.clear_result)
        self.update_buttons()

    def show_help(self):
        messagebox.showinfo("Справка по обновлению XML-выгрузки", HELP_TEXT, parent=self.page)

    def path_row(self, parent, row, label, variable, *, directory=False):
        ttk.Label(parent, text=label).grid(row=row, column=0, sticky="w", padx=(0, 8), pady=3)
        entry = ttk.Entry(parent, textvariable=variable)
        entry.grid(row=row, column=1, sticky="ew", pady=3)
        caption = "Выбрать папку…" if directory else "Выбрать файл…"
        button = ttk.Button(parent, text=caption, command=lambda: self.browse(variable, directory))
        button.grid(row=row, column=2, padx=(7, 0), pady=3)
        self.inputs.extend((entry, button))

    def browse(self, variable, directory):
        if directory:
            path = filedialog.askdirectory(parent=self.page, mustexist=True)
        else:
            path = filedialog.askopenfilename(parent=self.page, filetypes=[("XML", "*.xml")])
        if path:
            variable.set(path)

    def working_changed(self, *_):
        folder = self.working.get().strip().strip('"')
        self.baseline.set(str(Path(folder) / INDEX_NAME) if folder else "")

    def choose_working(self):
        if not self.working.get().strip():
            self.browse(self.working, True)
        return self.working.get()

    def clear_result(self, *_):
        self.plan = None
        self.arguments.set("")
        rows = self.changes.get_children()
        if rows:
            self.changes.delete(*rows)
        if not self.busy:
            self.status.set("Для первой выгрузки нужна пустая папка. Для сравнения выберите два индекса.")
        self.update_buttons()

    def update_buttons(self):
        commands = self.plan.commands if self.plan else {}
        can_compare = all(variable.get().strip() for variable in
                          (self.working, self.baseline, self.current, self.destination))
        self.first_button.state(["disabled"] if self.busy else ["!disabled"])
        self.index_button.state(["disabled"] if self.busy else ["!disabled"])
        for button, available in (
            (self.compare_button, can_compare),
            (self.list_button, EXPORT_LIST in commands),
            (self.check_button, CHECK_UPDATE in commands),
            (self.update_button, self.plan and self.plan.update_check),
            (self.open_button, self.plan),
        ):
            button.state(["!disabled"] if available and not self.busy else ["disabled"])

    def run(self, label, work, done):
        if self.busy:
            return
        self.busy = True
        self.status.set(label)
        self.update_buttons()
        for widget in self.inputs:
            widget.state(["disabled"])

        def completed(value, error):
            self.busy = False
            for widget in self.inputs:
                widget.state(["!disabled"])
            if error is None:
                done(value)
            else:
                self.status.set(str(error))
                messagebox.showerror("Обновление XML-выгрузки", str(error), parent=self.page)
            self.update_buttons()

        run_in_background(self.page, work, completed)

    def copy(self, arguments, message):
        self.arguments.set(arguments)
        try:
            self.page.clipboard_clear()
            self.page.clipboard_append(arguments)
        except tk.TclError:
            message = "Буфер обмена занят. Ключ можно выделить и скопировать из поля выше."
        self.status.set(message)

    def first_export(self):
        working = self.choose_working()
        if working:
            self.run("Проверяю папку первой выгрузки…", lambda: self.wizard.first_export(working),
                     lambda args: self.copy(args, "Ключ первой полной выгрузки скопирован. Запустите Конфигуратор."))

    def prepare_export(self):
        working = self.choose_working()
        if not working:
            return
        folder = filedialog.askdirectory(parent=self.page, mustexist=True,
                                         title="Выберите пустую папку для ConfigDumpInfo.xml из базы 1С")
        if not folder:
            return

        def ready(capture):
            self.current.set(str(capture.index))
            self.copy(capture.arguments, f"Ключ выгрузки индекса скопирован. После запуска 1С появится {capture.index}")

        self.run("Готовлю ключ выгрузки индекса…", lambda: self.wizard.prepare_export(working, folder), ready)

    def compare(self):
        working, output = self.working.get(), self.destination.get()
        baseline, current = self.baseline.get(), self.current.get()
        self.clear_result()

        def ready(result):
            self.plan = result
            for change in result.diff.changes[:DISPLAY_LIMIT]:
                name = object_label(change.after or change.before)
                if change.kind == "renamed":
                    name = object_label(change.before) + " → " + name
                self.changes.insert("", "end", values=(CHANGE_NAMES[change.kind], name))
            count = len(result.diff.changes)
            suffix = f" Показаны первые {DISPLAY_LIMIT}." if count > DISPLAY_LIMIT else ""
            arguments = result.commands.get(EXPORT_LIST, "")
            self.arguments.set(arguments)
            location = (f"Список сохранён в {result.object_list}"
                        if arguments else f"Список для выгрузки пуст. Отчёт в {result.directory}")
            self.status.set(f"Различий {count}. Адресов для выгрузки {len(result.diff.dump_objects)}."
                            f"{suffix}\n{location}")

        self.run("Сравниваю индексы рабочей выгрузки и базы…",
                 lambda: self.wizard.compare(working, output, baseline, current), ready)

    def copy_plan_command(self, name):
        if self.plan and name in self.plan.commands:
            message = ("Выполните скопированный ключ в Конфигураторе. "
                       "Затем нажмите «Проверить ответ 1С и скопировать -update»."
                       if name == CHECK_UPDATE else "Ключ выгрузки в папку delta скопирован. Запустите Конфигуратор.")
            self.copy(self.plan.commands[name], message)

    def copy_update(self):
        plan = self.plan
        if not plan or not plan.update_check:
            return
        self.arguments.set("")

        def ready(arguments):
            if arguments:
                self.copy(arguments, "1С подтвердила инкрементальную выгрузку. Ключ -update скопирован.")
            else:
                self.status.set("По ответу 1С различий нет. Обновлять рабочую папку не требуется.")

        self.run("Проверяю отчёт и код завершения 1С…", lambda: self.wizard.update_arguments(plan), ready)

    def open_result(self):
        if self.plan:
            try:
                os.startfile(str(self.plan.directory))
            except OSError as exc:
                messagebox.showerror("Открытие папки сравнения", str(exc), parent=self.page)
