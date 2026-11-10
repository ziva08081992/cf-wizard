"""Windows desktop view. I/O use cases run outside the Tk event loop."""

import os
from pathlib import Path
import tkinter as tk
from collections.abc import Callable
from dataclasses import replace
from tkinter import filedialog, messagebox, ttk

from .about import show_about
from .application import PreviewError, Wizard
from .paths import PathPreferences, dialog_location, normalize_path
from .presentation import group_label, object_groups, object_label
from .theme import apply_theme
from .ui_tasks import run_in_background
from .ui_tooltip import bind_tooltip


SEARCH_EXPAND_LIMIT = 800


class WizardWindow:
    def __init__(self, root: tk.Tk, wizard: Wizard, platforms: tuple[Path, ...] = (),
                 default_platform: Path | None = None, *, paths: PathPreferences | None = None,
                 save_paths: Callable[[PathPreferences], None] | None = None, default_logs: Path | None = None):
        self.root, self.wizard = root, wizard
        preferences = paths or PathPreferences()
        self.preferences = preferences
        self.save_paths = save_paths
        self.last_dirs = preferences.last_dirs.copy()
        apply_theme(root)
        self.catalog = self.preview = self.package = self.last_log = None
        self.default_logs = default_logs
        self.selected = set()
        self.groups = {}
        self.match_count = 0
        self.busy = False
        self.export = tk.StringVar(root, value=preferences.values.get("export", ""))
        self.destination = tk.StringVar(root, value=preferences.values.get("destination", ""))
        self.task = tk.StringVar(root)
        self.platform = tk.StringVar(root, value=preferences.values.get("platform", str(default_platform or "")))
        self.logs = tk.StringVar(root, value=preferences.values.get("logs", str(default_logs or "")))
        self.path_fields = {"export": self.export, "destination": self.destination,
                            "platform": self.platform, "logs": self.logs}
        self.search = tk.StringVar()
        self.count = tk.StringVar(value="Откройте выгрузку, чтобы увидеть объекты.")
        self.status = tk.StringVar(value="Укажите папку выгрузки или её Configuration.xml.")

        root.title("Мастер технической CF")
        root.minsize(850, 610)
        root.geometry("1180x760")
        root.protocol("WM_DELETE_WINDOW", self.close)
        body = ttk.Frame(root, padding=(16, 10))
        body.pack(fill="both", expand=True)
        header = ttk.Frame(body)
        header.pack(fill="x", pady=(0, 10))
        ttk.Button(header, text="О программе", command=lambda: show_about(root)).pack(side="right", anchor="n")
        ttk.Label(header, text="Мастер технической CF", style="Title.TLabel").pack(anchor="w")
        ttk.Label(header, text="Выборочная сборка из XML-выгрузки 1С", style="Muted.TLabel").pack(anchor="w", pady=(2, 0))

        fields = ttk.LabelFrame(body, text="Исходники и результат", padding=(10, 5))
        fields.pack(fill="x")
        fields.columnconfigure(1, weight=1)
        self.inputs = []
        self.field(fields, 0, "XML-выгрузка", self.export, [
            ("Папка…", lambda: self.browse_folder("export", self.open_export)),
            ("Configuration.xml…", self.browse_configuration),
            ("Открыть", self.open_export),
        ], path_field="export")
        self.field(fields, 1, "Задача или комментарий (необязательно)", self.task)
        self.field(fields, 2, "Папка результата", self.destination, [
            ("Выбрать…", lambda: self.browse_folder("destination")),
        ], path_field="destination")
        self.field(fields, 3, "ibcmd.exe", self.platform, [("Выбрать ibcmd.exe…", self.browse_platform)],
                   choices=tuple(str(path) for path in platforms), path_field="platform")
        self.field(fields, 4, "Папка журналов", self.logs, [
            ("Выбрать…", lambda: self.browse_folder("logs")),
            ("Открыть папку", self.open_logs_folder),
        ], path_field="logs")

        footer = ttk.Frame(body)
        footer.pack(side="bottom", fill="x")
        panes = ttk.Panedwindow(body, orient="horizontal")
        panes.pack(fill="both", expand=True, pady=(10, 8))
        left = ttk.LabelFrame(panes, text="Объекты XML-выгрузки", padding=6)
        right = ttk.LabelFrame(panes, text="Состав пакета", padding=6)
        panes.add(left, weight=3)
        panes.add(right, weight=2)

        search = ttk.Frame(left, style="Panel.TFrame")
        search.pack(fill="x")
        ttk.Label(search, text="Поиск", style="Panel.TLabel").pack(side="left", padx=(0, 7))
        ttk.Entry(search, textvariable=self.search).pack(side="left", fill="x", expand=True)
        ttk.Label(left, textvariable=self.count, style="PanelMuted.TLabel").pack(anchor="w", pady=3)
        ttk.Label(left, text="Отметьте флажком или пробелом.", style="PanelMuted.TLabel").pack(
            side="bottom", anchor="w", pady=(3, 0))
        self.objects = self.table(left, [("mark", "Выбор", 64, False)], show="tree headings")
        self.objects.heading("#0", text="Тип и объект")
        self.objects.column("#0", width=300)
        self.objects.column("mark", anchor="center")
        self.objects.tag_configure("group", font=("Segoe UI", 10, "bold"))
        self.objects.bind("<<TreeviewOpen>>", lambda _: self.populate_group(self.objects.focus()))
        self.objects.bind("<Button-1>", self.click_object)
        self.objects.bind("<space>", self.toggle_focused)

        tabs = ttk.Notebook(right)
        tabs.pack(fill="both", expand=True)
        whole = ttk.Frame(tabs, padding=2, style="Panel.TFrame")
        stubs = ttk.Frame(tabs, padding=2, style="Panel.TFrame")
        tabs.add(whole, text="Перенос целиком")
        tabs.add(stubs, text="Заглушки")
        self.whole = self.table(whole, [("name", "Объект", 340, True)])
        self.stubs = self.table(stubs, [("name", "Объект", 185, True), ("reason", "Причина", 155, True)])
        self.stub_tab, self.tabs = stubs, tabs

        actions = ttk.Frame(footer)
        actions.pack(fill="x")
        self.preview_button = ttk.Button(actions, text="Предпросмотр", command=self.prepare)
        self.build_button = ttk.Button(actions, text="Собрать CF", command=self.build, style="Accent.TButton")
        self.open_button = ttk.Button(actions, text="Открыть комплект", command=self.open_package)
        self.log_button = ttk.Button(actions, text="Открыть журнал", command=self.open_last_log)
        for button, hint in (
            (self.preview_button, "Рассчитает зависимости и покажет заглушки."),
            (self.build_button, "Подготовит XML-комплект, соберёт CF и сверит её обратной выгрузкой."),
            (self.open_button, "Откроет папку готового комплекта."),
            (self.log_button, "Откроет журнал последнего предпросмотра или сборки."),
        ):
            button.pack(side="left", padx=(0, 8))
            bind_tooltip(button, hint)
        self.progress = ttk.Progressbar(actions, mode="indeterminate", length=100)
        self.progress.pack(side="right")
        status = ttk.Label(footer, textvariable=self.status, style="PanelMuted.TLabel", padding=(10, 4))
        status.pack(fill="x", pady=(6, 0))
        status.bind("<Configure>", lambda event: status.configure(wraplength=max(1, event.width - 20)))

        self.export.trace_add("write", self.reset_source)
        self.destination.trace_add("write", self.update_buttons)
        self.logs.trace_add("write", self.reset_preview)
        self.platform.trace_add("write", self.update_buttons)
        self.search.trace_add("write", self.render_objects)
        self.update_buttons()

    def field(self, parent, row, label, variable, buttons=(), *, choices=(), path_field=None):
        ttk.Label(parent, text=label, style="Panel.TLabel").grid(row=row, column=0, sticky="w", padx=(0, 10), pady=2)
        if choices:
            entry = ttk.Combobox(parent, textvariable=variable, values=choices)
        else:
            entry = ttk.Entry(parent, textvariable=variable)
        entry.grid(row=row, column=1, sticky="ew", pady=2)
        self.inputs.append(entry)
        if path_field:
            entry.bind("<FocusOut>", lambda _: self.normalize_field(path_field), add="+")
            if choices:
                entry.bind("<<ComboboxSelected>>", lambda _: self.choose_path(path_field, variable.get(), file=True))
        if row == 0:
            entry.bind("<Return>", lambda _: self.open_export())
        if buttons:
            commands = ttk.Frame(parent, style="Panel.TFrame")
            commands.grid(row=row, column=2, sticky="w", padx=(6, 0), pady=2)
            for label, command in buttons:
                button = ttk.Button(commands, text=label, command=command)
                button.pack(side="left", padx=(0, 5))
                self.inputs.append(button)

    @staticmethod
    def table(parent, columns, *, show="headings"):
        frame = ttk.Frame(parent)
        frame.pack(fill="both", expand=True)
        tree = ttk.Treeview(frame, columns=[c[0] for c in columns], show=show, selectmode="browse")
        for key, title, width, stretch in columns:
            tree.heading(key, text=title)
            tree.column(key, width=width, stretch=stretch)
        scroll = ttk.Scrollbar(frame, orient="vertical", command=tree.yview)
        horizontal = ttk.Scrollbar(frame, orient="horizontal", command=tree.xview)
        tree.configure(yscrollcommand=scroll.set, xscrollcommand=horizontal.set)
        tree.grid(row=0, column=0, sticky="nsew")
        scroll.grid(row=0, column=1, sticky="ns")
        horizontal.grid(row=1, column=0, sticky="ew")
        frame.columnconfigure(0, weight=1)
        frame.rowconfigure(0, weight=1)
        return tree

    @staticmethod
    def clear(tree):
        rows = tree.get_children()
        if rows:
            tree.delete(*rows)

    def normalize_field(self, field):
        variable = self.path_fields[field]
        path = normalize_path(variable.get())
        if variable.get() != path:
            variable.set(path)
        return path

    def location(self, field, *, filename=None):
        return dialog_location(self.normalize_field(field), self.last_dirs.get(field, ""), filename=filename)

    def choose_path(self, field, value, *, file=False):
        path = Path(value)
        self.path_fields[field].set(str(path))
        self.last_dirs[field] = str(path.parent if file else path)

    def browse_folder(self, field, after=None):
        path = filedialog.askdirectory(parent=self.root, mustexist=True, **self.location(field))
        if path:
            self.choose_path(field, path)
            if after:
                after()

    def browse_configuration(self):
        path = filedialog.askopenfilename(parent=self.root, title="Выберите Configuration.xml",
                                          filetypes=[("Configuration.xml", "Configuration.xml"), ("XML", "*.xml")],
                                          **self.location("export", filename="Configuration.xml"))
        if path:
            self.choose_path("export", path, file=True)
            self.open_export()

    def browse_platform(self):
        path = filedialog.askopenfilename(parent=self.root, title="Выберите ibcmd.exe из папки bin установленной 1С",
                                          filetypes=[("ibcmd.exe", "ibcmd.exe")],
                                          **self.location("platform", filename="ibcmd.exe"))
        if path:
            self.choose_path("platform", path, file=True)

    def reset_source(self, *_):
        self.catalog = None
        self.selected.clear()
        self.reset_preview()
        self.render_objects()

    def reset_preview(self, *_):
        if self.preview is not None:
            self.wizard.discard(self.preview)
        self.preview = self.package = None
        self.clear(self.stubs)
        self.tabs.tab(self.stub_tab, text="Заглушки")
        self.clear(self.whole)
        for name in sorted(self.selected):
            self.whole.insert("", "end", values=(object_label(name),))
        if self.catalog and not self.busy:
            self.status.set("Проверьте выбранные объекты и сформируйте предпросмотр.")
        self.update_buttons()

    def render_objects(self, *_):
        self.clear(self.objects)
        self.groups.clear()
        self.match_count = 0
        if self.catalog is None:
            self.count.set("Откройте выгрузку, чтобы увидеть объекты.")
            return
        query = self.search.get().strip()
        self.groups = object_groups(self.catalog.objects, query)
        self.match_count = sum(len(names) for names in self.groups.values())
        expand = bool(query) and self.match_count <= SEARCH_EXPAND_LIMIT
        for kind, names in self.groups.items():
            self.objects.insert("", "end", iid=kind, text=f"{group_label(kind)} ({len(names)})",
                                open=expand, tags=("group",))
            self.objects.insert(kind, "end", iid=f"@{kind}")
            if expand:
                self.populate_group(kind)
        self.update_count()

    def populate_group(self, kind):
        placeholder = f"@{kind}"
        if not self.objects.exists(placeholder):
            return
        self.objects.delete(placeholder)
        for key in self.groups[kind]:
            name = key.partition(".")[2]
            self.objects.insert(kind, "end", iid=key, text=name, values=("☑" if key in self.selected else "☐",))

    def update_count(self):
        self.count.set(f"Найдено {self.match_count}. Отмечено {len(self.selected)}.")

    def click_object(self, event):
        if self.objects.identify_region(event.x, event.y) != "cell":
            return
        name = self.objects.identify_row(event.y)
        if bounds := self.objects.bbox(name, "mark"):
            left, _, width, height = bounds
            if abs(event.x - (left + width // 2)) <= height // 2:
                self.toggle(name)

    def toggle_focused(self, _):
        key = self.objects.focus()
        if key in self.groups:
            self.populate_group(key)
            opened = self.root.tk.getboolean(self.objects.item(key, "open"))
            self.objects.item(key, open=not opened)
        else:
            self.toggle(key)
        return "break"

    def toggle(self, name):
        if self.busy or "." not in name or not self.objects.exists(name):
            return
        if name in self.selected:
            self.selected.remove(name)
        else:
            self.selected.add(name)
        self.objects.set(name, "mark", "☑" if name in self.selected else "☐")
        self.update_count()
        self.reset_preview()

    def update_buttons(self, *_):
        self.preview_button.state(["!disabled"] if self.selected and not self.busy else ["disabled"])
        self.build_button.state(["!disabled"] if self.preview and self.platform.get().strip() and self.destination.get().strip()
                                and not self.busy else ["disabled"])
        self.open_button.state(["!disabled"] if self.package and not self.busy else ["disabled"])
        self.log_button.state(["!disabled"] if self.last_log and not self.busy else ["disabled"])


    def run(self, label, work, done):
        if self.busy:
            return
        self.busy = True
        self.status.set(label)
        self.update_buttons()
        for widget in self.inputs:
            widget.state(["disabled"])
        self.progress.start(12)

        def completed(value, error):
            self.progress.stop()
            self.busy = False
            for widget in self.inputs:
                widget.state(["!disabled"])
            if error is None:
                done(value)
            else:
                if isinstance(error, PreviewError):
                    self.last_log = error.log
                self.status.set("Операция остановлена. Проверьте сообщение.")
                messagebox.showerror("Мастер технической CF", str(error), parent=self.root)
            self.update_buttons()

        run_in_background(self.root, work, completed)

    def open_export(self):
        if self.busy:
            return
        path = self.normalize_field("export")
        self.reset_source()

        def opened(catalog):
            self.catalog = catalog
            self.last_dirs["export"] = str(catalog.root)
            self.render_objects()
            self.status.set(f"Открыта конфигурация {catalog.configuration}. Отметьте нужные объекты.")

        self.run("Читаю состав XML-выгрузки…", lambda: self.wizard.open_export(path), opened)

    def prepare(self):
        source, selected = self.catalog, self.selected.copy()
        task, destination = self.task.get(), self.normalize_field("destination")
        logs = self.normalize_field("logs")
        self.reset_preview()

        def prepared(preview):
            self.preview = preview
            self.last_log = preview.log
            self.last_dirs["logs"] = str(preview.log.parent)
            for dependency in preview.dependencies:
                self.stubs.insert("", "end", values=(object_label(dependency.name), object_label(dependency.referred_by)))
            self.tabs.tab(self.stub_tab, text=f"Заглушки ({len(preview.dependencies)})")
            if preview.dependencies:
                self.tabs.select(self.stub_tab)
            self.status.set(f"Предпросмотр готов. Объектов {len(preview.selected)}, заглушек {len(preview.dependencies)}. "
                            f"Журнал {preview.log}")
            if preview.warnings:
                messagebox.showwarning("Предпросмотр", "\n".join(preview.warnings), parent=self.root)

        self.run("Рассчитываю зависимости и заглушки…",
                 lambda: self.wizard.preview(source, task, selected, destination, logs), prepared)

    def build(self):
        preview, platform = self.preview, self.normalize_field("platform")
        destination, task = self.normalize_field("destination"), self.task.get()
        self.preview = None
        self.last_log = preview.log

        def built(package):
            self.package = package
            self.last_dirs["destination"] = str(package.parent)
            self.last_dirs["platform"] = str(Path(platform).parent)
            self.status.set(f"CF собрана и проверена обратной выгрузкой. Комплект: {package}")

        def work():
            try:
                return self.wizard.build(preview, platform, destination=destination, task=task)
            except Exception as exc:
                raise ValueError(f"{exc}\nЖурнал {preview.log}\nДля повтора сформируйте новый предпросмотр.") from exc

        self.run("Собираю CF и сверяю результат обратной выгрузкой…", work, built)

    def open_package(self):
        try:
            os.startfile(str(self.package))
        except OSError as exc:
            messagebox.showerror("Открытие комплекта", str(exc), parent=self.root)

    def open_logs_folder(self):
        folder = self.normalize_field("logs") or self.default_logs
        if folder:
            self.open_log_path(folder)

    def open_last_log(self):
        if self.last_log:
            self.open_log_path(self.last_log)

    def open_log_path(self, path):
        try:
            os.startfile(str(path))
        except OSError as exc:
            messagebox.showerror("Открытие журнала", str(exc), parent=self.root)

    def close(self):
        if self.busy:
            messagebox.showinfo("Мастер технической CF", "Дождитесь завершения текущего шага.", parent=self.root)
            return
        if self.preview is not None:
            self.wizard.discard(self.preview)
            self.preview = None
        if self.save_paths:
            values = {field: normalize_path(variable.get()) for field, variable in self.path_fields.items()}
            try:
                self.save_paths(replace(self.preferences, values=self.preferences.values | values,
                                        last_dirs=self.last_dirs))
            except OSError as exc:
                messagebox.showwarning("Сохранение путей", str(exc), parent=self.root)
        self.root.destroy()
