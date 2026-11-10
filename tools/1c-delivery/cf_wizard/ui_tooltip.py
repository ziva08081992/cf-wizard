"""Display a short hint when the pointer rests on a command."""

import tkinter as tk
from tkinter import ttk


def bind_tooltip(widget, text):
    pending = None
    window = None

    def hide(_event=None):
        nonlocal pending, window
        if pending is not None:
            widget.after_cancel(pending)
            pending = None
        if window is not None:
            if window.winfo_exists():
                window.destroy()
            window = None

    def show():
        nonlocal pending, window
        pending = None
        if not widget.winfo_ismapped():
            return
        window = tk.Toplevel(widget)
        window.withdraw()
        window.overrideredirect(True)
        window.attributes("-topmost", True)
        ttk.Label(window, text=text, wraplength=320, padding=(8, 5), relief="solid").pack()
        window.update_idletasks()
        root = widget.winfo_toplevel()
        width, height = window.winfo_reqwidth(), window.winfo_reqheight()
        x = min(widget.winfo_rootx(), root.winfo_rootx() + root.winfo_width() - width)
        y = widget.winfo_rooty() + widget.winfo_height() + 5
        if y + height > root.winfo_rooty() + root.winfo_height():
            y = widget.winfo_rooty() - height - 5
        window.geometry(f"+{x}+{y}")
        window.deiconify()

    def enter(_event):
        nonlocal pending
        hide()
        pending = widget.after(450, show)

    widget.bind("<Enter>", enter, add="+")
    for event in ("<Leave>", "<ButtonPress>", "<Unmap>", "<Destroy>"):
        widget.bind(event, hide, add="+")
