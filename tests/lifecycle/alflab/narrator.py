"""Narrator strategy (D8): ONE stage-execution path, identical assertions in
both modes. NullNarrator renders the compact automated view (stage headers +
check lines — CI logs need those); RichNarrator adds the walkthrough idioms
(explain/flow/diffs/⊙ rendering) and pauses that read /dev/tty ('q' aborts,
keeping the container up and printing attach/cleanup commands)."""

from __future__ import annotations

from . import ui


class InteractiveAbort(Exception):
    """Raised when the operator answers 'q' at a pause (exit 130)."""


class NullNarrator:
    interactive = False

    def stage_start(self, stage_id: str, title: str):
        ui.section(stage_id.upper(), title)

    def explain(self, text: str):        # rendered only by RichNarrator
        pass

    def flow(self, arrows: str):
        pass

    def show_diff(self, label: str, text: str):
        pass

    def show_data(self, label: str, data):
        pass

    def inspect(self, run_dir, items):
        pass

    def inspect_online(self, bucket, items):
        pass

    def attach_hint(self, container_name: str):
        pass

    def pause(self, prompt: str = "ENTER to continue · q to abort"):
        pass

    # check-level rendering is shared — every mode shows verdict lines
    def check(self, status: str, name: str, detail: str = ""):
        msg = f"{name}" + (f" — {detail}" if detail else "")
        if status == "PASS":
            ui.ok(msg)
        elif status == "FAIL":
            ui.fail(msg)
        elif status == "SKIP":
            ui.skip(msg)
        elif status == "XFAIL":
            ui.xfail(msg)
        elif status == "XPASS":
            ui.xpass(msg)


class RichNarrator(NullNarrator):
    def __init__(self, interactive: bool, container_name: str = ""):
        self.interactive = interactive
        self.container_name = container_name

    def explain(self, text: str):
        ui.explain(text)

    def flow(self, arrows: str):
        ui.flow(arrows)

    def show_diff(self, label: str, text: str):
        if not text.strip():
            return
        ui.emit(f"  {ui.c('yellow', label)}:")
        for line in text.splitlines()[:60]:
            color = "green" if line.startswith("+") else (
                "red" if line.startswith("-") else "dim")
            ui.emit(f"    {ui.c(color, line)}")
        if len(text.splitlines()) > 60:
            ui.emit(f"    {ui.c('dim', '… (truncated)')}")
        ui.emit()

    def show_data(self, label: str, data):
        ui.show_data(label, data)

    def inspect(self, run_dir, items):
        ui.inspect(run_dir, items)

    def inspect_online(self, bucket, items):
        ui.inspect_online(bucket, items)

    def attach_hint(self, container_name: str = ""):
        name = container_name or self.container_name
        if name:
            ui.emit(f"  {ui.c('dim', 'attach: docker exec -it -u agent ' + name + ' bash')}")

    def pause(self, prompt: str = "ENTER to continue · q to abort"):
        if not self.interactive:
            return
        self.attach_hint()
        try:
            with open("/dev/tty", "r") as tty:
                ui.emit(f"  {ui.c('blue', '▸')} {prompt} ", )
                answer = tty.readline().strip().lower()
        except OSError:
            return  # no tty — behave like --no-pause
        if answer == "q":
            raise InteractiveAbort()
