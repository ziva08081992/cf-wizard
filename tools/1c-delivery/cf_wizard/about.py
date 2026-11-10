"""Credits displayed by the desktop wizard."""

import tkinter as tk
from tkinter import messagebox, ttk
import webbrowser

from . import __version__


REPOSITORY_URL = "https://github.com/alonehobo/1c-partial-cf"


def show_about(parent):
    window = tk.Toplevel(parent)
    window.title("О программе")
    window.transient(parent)
    window.resizable(False, False)
    body = ttk.Frame(window, padding=18)
    body.pack(fill="both", expand=True)
    ttk.Label(body, text="Мастер технической CF", font=("Segoe UI", 15, "bold")).pack(anchor="w")
    ttk.Label(body, text=f"Версия программы {__version__}", style="Muted.TLabel").pack(anchor="w", pady=(2, 0))
    ttk.Label(body, text="Для подготовки технической конфигурации используется скрипт partial_cf.py.",
              wraplength=480).pack(anchor="w", pady=(8, 5))
    ttk.Label(body, text="Большое спасибо alonehobo за этот скрипт.").pack(anchor="w")

    def open_repository(_=None):
        try:
            opened = webbrowser.open(REPOSITORY_URL, new=2)
        except webbrowser.Error:
            opened = False
        if not opened:
            messagebox.showinfo("Репозиторий", REPOSITORY_URL, parent=window)

    link = ttk.Label(body, text="Открыть репозиторий 1c-partial-cf на GitHub", cursor="hand2", takefocus=True,
                     foreground="#86AEFF", font=("Segoe UI", 10, "underline"))
    link.pack(anchor="w", pady=(8, 12))
    link.bind("<Button-1>", open_repository)
    link.bind("<Return>", open_repository)
    link.bind("<space>", open_repository)
    ttk.Button(body, text="Закрыть", command=window.destroy).pack(anchor="e")
    window.bind("<Escape>", lambda _: window.destroy())
