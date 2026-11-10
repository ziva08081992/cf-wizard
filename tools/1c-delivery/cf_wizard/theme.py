"""Dark ttk palette and control styles for the technical CF workspace."""

from tkinter import ttk


BACKGROUND = "#1E1F22"
PANEL = "#25262B"
INPUT = "#1C1D21"
CONTROL = "#33353C"
BORDER = "#42444D"
TEXT = "#E5E7EB"
MUTED = "#A7AAB5"
ACCENT = "#3574E9"
SELECTION = "#294B80"
FONT = ("Segoe UI", 10)


def apply_theme(root):
    root.configure(background=BACKGROUND)
    style = ttk.Style(root)
    style.theme_use("clam")
    style.configure(".", background=BACKGROUND, foreground=TEXT, font=FONT, bordercolor=BORDER,
                    lightcolor=BORDER, darkcolor=BORDER, selectbackground=SELECTION, selectforeground=TEXT)
    style.configure("TFrame", background=BACKGROUND)
    style.configure("Panel.TFrame", background=PANEL)
    style.configure("Panel.TLabel", background=PANEL)
    style.configure("Muted.TLabel", foreground=MUTED)
    style.configure("PanelMuted.TLabel", background=PANEL, foreground=MUTED)
    style.configure("Title.TLabel", font=("Segoe UI", 18, "bold"))
    style.configure("TLabelframe", background=PANEL)
    style.configure("TLabelframe.Label", background=BACKGROUND, font=("Segoe UI", 10, "bold"))

    style.configure("TButton", background=CONTROL, padding=(9, 4), focuscolor=ACCENT)
    style.map("TButton", background=[("disabled", PANEL), ("pressed", SELECTION), ("active", BORDER)],
              foreground=[("disabled", MUTED)])
    style.configure("Accent.TButton", background=ACCENT, foreground="#FFFFFF", bordercolor=ACCENT)
    style.map("Accent.TButton",
              background=[("disabled", PANEL), ("pressed", "#295CBD"), ("active", "#4884F3")],
              bordercolor=[("disabled", BORDER), ("focus", "#82ABF9")])
    for field_style in ("TEntry", "TCombobox"):
        style.configure(field_style, fieldbackground=INPUT, insertcolor=TEXT, padding=(7, 3))
        style.map(field_style, fieldbackground=[("disabled", PANEL)],
                  bordercolor=[("focus", ACCENT)], foreground=[("disabled", MUTED)])
    style.configure("TCombobox", arrowcolor=TEXT, background=CONTROL)
    root.option_add("*TCombobox*Listbox.background", PANEL)
    root.option_add("*TCombobox*Listbox.foreground", TEXT)
    root.option_add("*TCombobox*Listbox.selectBackground", SELECTION)
    root.option_add("*TCombobox*Listbox.selectForeground", TEXT)

    style.configure("Treeview", background=INPUT, fieldbackground=INPUT, rowheight=25)
    style.map("Treeview", background=[("selected", SELECTION)], foreground=[("selected", "#FFFFFF")])
    style.configure("Treeview.Heading", background=CONTROL, padding=(6, 4), font=("Segoe UI", 9, "bold"))
    style.map("Treeview.Heading", background=[("active", CONTROL)])
    style.configure("TPanedwindow", background=BACKGROUND, sashwidth=8)
    style.configure("TNotebook", background=PANEL, borderwidth=0, tabmargins=(0, 0, 0, 4))
    style.configure("TNotebook.Tab", background=CONTROL, padding=(10, 6))
    style.map("TNotebook.Tab", background=[("selected", INPUT), ("active", BORDER)],
              foreground=[("selected", TEXT), ("!selected", MUTED)])
    for orientation in ("Vertical", "Horizontal"):
        style.configure(f"{orientation}.TScrollbar", background=CONTROL, troughcolor=INPUT, arrowcolor=MUTED)
        style.map(f"{orientation}.TScrollbar", background=[("active", BORDER)])
    style.configure("Horizontal.TProgressbar", background=ACCENT, troughcolor=PANEL)
