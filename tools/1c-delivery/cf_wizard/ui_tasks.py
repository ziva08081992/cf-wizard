"""Run blocking view actions without touching Tk from a worker."""

from queue import Empty, Queue
from threading import Thread


def run_in_background(window, work, completed):
    results = Queue(maxsize=1)

    def worker():
        try:
            results.put((work(), None))
        except Exception as exc:
            results.put((None, exc))

    def poll():
        try:
            value, error = results.get_nowait()
        except Empty:
            window.after(70, poll)
            return
        completed(value, error)

    Thread(target=worker, daemon=True).start()
    window.after(70, poll)
