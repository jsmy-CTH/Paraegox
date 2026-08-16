#!/usr/bin/env python3
"""Optional EAGOS-inspired Textual chat client for Paraegox."""

from __future__ import annotations

import argparse
import asyncio
import shutil
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Optional

from rich.text import Text
from textual import events, on
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.widgets import Input, RichLog, Static

from backend import (
    MAX_ERROR_BYTES,
    MAX_INPUT_BYTES,
    BackendError,
    ChatBackend,
    InputValidationError,
    JsonlBackend,
    bounded_text,
    truncate_utf8,
)


MAX_UI_MESSAGES = 64
MAX_MESSAGE_BYTES = 16 * 1024


@dataclass(frozen=True)
class ChatMessage:
    role: str
    content: str


class ParaegoxTextual(App[None]):
    """Small alternative view over the authoritative Rust Agent client."""

    TITLE = "Paraegox Textual"
    BINDINGS = [
        Binding("escape", "cancel_or_clear", "Cancel / clear", priority=True),
        Binding("ctrl+c", "quit_orderly", "Quit", priority=True),
    ]
    CSS = """
    $background: #0a0e13;
    $cyan: #37e8ff;
    $green: #74ff9c;
    $yellow: #f4d35e;
    $red: #ff6b6b;
    $dim: #4a6068;
    $foreground: #d7e3e7;

    Screen {
        background: $background;
        color: $foreground;
        layout: vertical;
    }

    #header {
        height: 3;
        padding: 1 2 0 2;
        color: $cyan;
        text-style: bold;
        border-bottom: solid $dim;
    }

    #body {
        height: 1fr;
        padding: 0 1;
    }

    .panel {
        height: 1fr;
        margin: 0 1 0 0;
        padding: 0 1;
        border: round $dim;
        background: $background;
    }

    .panel-title {
        height: 2;
        color: $cyan;
        text-style: bold;
    }

    #session-panel { width: 25; }
    #chat-panel { width: 1fr; border: round $cyan; }
    #target-panel { width: 29; margin-right: 0; }

    #chat-log {
        height: 1fr;
        scrollbar-color: $cyan;
        scrollbar-background: $background;
    }

    #message-input {
        height: 3;
        margin: 0 2;
        padding: 0 1;
        border: round $green;
        background: $background;
        color: $foreground;
    }

    #message-input:focus { border: round $cyan; }

    #footer {
        height: 1;
        padding: 0 2;
        color: $dim;
    }
    """

    def __init__(self, backend: ChatBackend, target: str, endpoint: str) -> None:
        super().__init__()
        self.backend = backend
        self.target = bounded_text(target, 64)
        self.endpoint = bounded_text(endpoint, 256)
        self.messages: list[ChatMessage] = []
        self.active_turn_id: Optional[str] = None
        self._quit_requested = False
        self._connected = False
        self._backend_failed = False
        self._notice = "Opening the local Fabric client…"

    def compose(self) -> ComposeResult:
        yield Static("PARAEGOX   DISTRIBUTED AGENT OS", id="header")
        with Horizontal(id="body"):
            with Vertical(id="session-panel", classes="panel"):
                yield Static("SESSION", classes="panel-title")
                yield Static("ID\n—\n\nLIFETIME\nEPHEMERAL\n\nMESSAGES\n0", id="session")
            with Vertical(id="chat-panel", classes="panel"):
                yield Static("CHAT", classes="panel-title")
                yield RichLog(
                    id="chat-log",
                    max_lines=512,
                    min_width=1,
                    wrap=True,
                    highlight=False,
                    markup=False,
                )
            with Vertical(id="target-panel", classes="panel"):
                yield Static("TARGET + AGENT", classes="panel-title")
                yield Static(id="target-status")
        yield Input(
            placeholder="Connecting to Fabric…",
            id="message-input",
            disabled=True,
        )
        yield Static(id="footer")

    async def on_mount(self) -> None:
        self._apply_layout(self.size.width)
        self._update_status("STARTING", "waiting")
        self._update_footer()
        self.run_worker(self._backend_loop(), group="backend", exclusive=True)

    def on_resize(self, event: events.Resize) -> None:
        self._apply_layout(event.size.width)

    @on(Input.Changed, "#message-input")
    def bound_input(self, event: Input.Changed) -> None:
        bounded = truncate_utf8(event.value, MAX_INPUT_BYTES)
        if bounded != event.value:
            event.input.value = bounded
            event.input.cursor_position = len(bounded)
            self._set_notice("Input is limited to 4 KiB")

    @on(Input.Submitted, "#message-input")
    async def submit_message(self, event: Input.Submitted) -> None:
        text = event.value
        if text == "/quit":
            self.action_quit_orderly()
            return
        if self.active_turn_id is not None or not text.strip():
            return
        try:
            turn_id = await self.backend.submit(text)
        except BackendError as error:
            if isinstance(error, InputValidationError):
                self._set_notice(str(error))
                return
            self._mark_backend_failed()
            self._show_error(error)
            return
        self.active_turn_id = turn_id
        event.input.value = ""
        event.input.disabled = True
        self._append_message("YOU", text)
        self._update_status("WAITING", "waiting")
        self._set_notice("Waiting for the authoritative final response")

    async def action_cancel_or_clear(self) -> None:
        if self.active_turn_id is None:
            editor = self.query_one("#message-input", Input)
            editor.value = ""
            self._set_notice("Input cleared")
            return
        try:
            requested = await self.backend.cancel()
        except BackendError as error:
            self._mark_backend_failed()
            self._show_error(error)
            return
        if requested:
            self._append_message("SYSTEM", "Cancellation requested")
            self._set_notice("Waiting for the Turn terminal")

    def action_quit_orderly(self) -> None:
        if self._quit_requested:
            return
        self._quit_requested = True
        self._set_notice("Closing the Agent conversation…")
        self.run_worker(
            self._shutdown_and_exit(),
            group="shutdown",
            exclusive=True,
            exit_on_error=False,
        )

    async def _shutdown_and_exit(self) -> None:
        return_code = 1 if self._backend_failed else 0
        try:
            await self.backend.shutdown()
        except Exception as error:
            return_code = 1
            self._show_error(error)
        if self._backend_failed:
            return_code = 1
        self.exit(return_code=return_code)

    async def on_unmount(self) -> None:
        await self.backend.shutdown()

    async def _backend_loop(self) -> None:
        try:
            await self.backend.start()
            self._connected = True
            editor = self.query_one("#message-input", Input)
            editor.disabled = False
            editor.placeholder = "Write a message…"
            editor.focus()
            self._refresh_session()
            self._update_status("IDLE", "ok")
            self._set_notice(
                "Fabric client open; Agent availability requires a Turn response"
            )
            async for frame in self.backend.events():
                self._handle_backend_event(frame)
        except asyncio.CancelledError:
            raise
        except BackendError as error:
            self._mark_backend_failed()
            if not self._quit_requested:
                self._show_error(error)
        finally:
            if self._connected:
                self._mark_backend_failed()
                if not self._quit_requested:
                    self._update_status("ERROR", "error")
                    self._set_notice("Backend event stream closed unexpectedly")

    def _handle_backend_event(self, frame: dict[str, object]) -> None:
        frame_type = frame["type"]
        if frame_type == "cancel_result":
            result = frame["result"]
            if result == "cancellation_requested":
                self._set_notice("Agent cancellation accepted; waiting for terminal")
            elif result == "already_terminal":
                self._set_notice("Turn was already terminal")
            else:
                self._set_notice("Agent reported that the Turn is not active")
        elif frame_type == "turn_terminal":
            result = frame["result"]
            assert isinstance(result, dict)
            terminal = result["terminal"]
            assert isinstance(terminal, dict)
            kind = terminal["terminal"]
            if kind == "final":
                content = terminal["content"]
                assert isinstance(content, str)
                self._append_message("AGENT", content)
                self._update_status("IDLE", "ok")
                self._set_notice("Agent reply received")
            elif kind == "cancelled":
                self._append_message("SYSTEM", "Turn cancelled")
                self._update_status("IDLE", "ok")
                self._set_notice("The active Turn was cancelled")
            elif kind == "timed_out":
                self._append_message("SYSTEM", "Turn timed out")
                self._update_status("ERROR", "error")
                self._set_notice("The last Turn timed out")
            else:
                reason = terminal["reason"]
                assert isinstance(reason, str)
                self._append_message("SYSTEM", f"Turn failed: {reason.replace('_', ' ')}")
                self._update_status("ERROR", "error")
                self._set_notice("The last Turn failed")
            self._finish_active_turn()
        elif frame_type == "turn_error":
            self._mark_backend_failed()
            message = frame["message"]
            assert isinstance(message, str)
            self._append_message(
                "SYSTEM",
                "Turn outcome is unknown: " + bounded_text(message, MAX_ERROR_BYTES),
            )
            self._update_status("ERROR", "error")
            self._set_notice("Backend could not confirm an Agent terminal")
            self._finish_active_turn(enable_input=False)
        elif frame_type == "stopped":
            self._connected = False
            self._update_status("CLOSED", "dim")
            self._set_notice("Agent conversation closed")

    def _append_message(self, role: str, content: str) -> None:
        content = bounded_text(content, MAX_MESSAGE_BYTES)
        if len(self.messages) == MAX_UI_MESSAGES:
            self.messages.pop(0)
        self.messages.append(ChatMessage(role, content))
        colors = {"YOU": "#37e8ff", "AGENT": "#74ff9c", "SYSTEM": "#4a6068"}
        line = Text()
        line.append(f"{role:<7}", style=f"bold {colors[role]}")
        line.append(content, style="#d7e3e7")
        self.query_one("#chat-log", RichLog).write(line)
        self._refresh_session()

    def _finish_active_turn(self, enable_input: bool = True) -> None:
        self.active_turn_id = None
        editor = self.query_one("#message-input", Input)
        editor.disabled = not enable_input or self._quit_requested
        if not editor.disabled:
            editor.focus()
        self._update_footer()

    def _refresh_session(self) -> None:
        session_id = self.backend.session_id or "—"
        self.query_one("#session", Static).update(
            Text(
                f"ID\n{session_id}\n\nLIFETIME\nEPHEMERAL\n\nMESSAGES\n{len(self.messages)}"
            )
        )

    def _update_status(self, state: str, state_class: str) -> None:
        status = Text()
        status.append("NODE\n", style="#4a6068")
        status.append(f"{self.target}\n\n", style="#d7e3e7")
        status.append("ENDPOINT\n", style="#4a6068")
        status.append(f"{self.endpoint}\n\n", style="#d7e3e7")
        status.append("FABRIC CLIENT\n", style="#4a6068")
        status.append(
            "OPEN\n\n" if self._connected else "NOT OPEN\n\n",
            style="#74ff9c" if self._connected else "#4a6068",
        )
        status.append("LOCAL TURN\n", style="#4a6068")
        colors = {
            "ok": "#74ff9c",
            "waiting": "#f4d35e",
            "error": "#ff6b6b",
            "dim": "#4a6068",
        }
        status.append(state, style=colors[state_class])
        self.query_one("#target-status", Static).update(status)

    def _show_error(self, error: BaseException) -> None:
        message = bounded_text(str(error), MAX_ERROR_BYTES)
        self._append_message("SYSTEM", message)
        self._update_status("ERROR", "error")
        self._set_notice(message)
        self.query_one("#message-input", Input).disabled = True

    def _mark_backend_failed(self) -> None:
        self._backend_failed = True
        self._connected = False

    def _set_notice(self, notice: str) -> None:
        self._notice = bounded_text(notice, MAX_ERROR_BYTES)
        self._update_footer()

    def _update_footer(self) -> None:
        keys = (
            "[Esc] cancel  [Ctrl-C] cancel + quit"
            if self.active_turn_id is not None
            else "[Enter] send  [Esc] clear  [Ctrl-C] quit  [/quit] quit"
        )
        footer = Text()
        footer.append(f" {keys} │ ", style="#4a6068")
        footer.append(self._notice, style="#d7e3e7")
        self.query_one("#footer", Static).update(footer)

    def _apply_layout(self, width: int) -> None:
        self.query_one("#session-panel", Vertical).display = width >= 110
        self.query_one("#target-panel", Vertical).display = width >= 80


def _timeout_ms(value: str) -> int:
    try:
        timeout = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if not 100 <= timeout <= 60_000:
        raise argparse.ArgumentTypeError("must be between 100 and 60000")
    return timeout


def _resolve_binary(override: Optional[str]) -> str:
    candidate = override or "paraegox"
    resolved = shutil.which(candidate)
    if resolved is None:
        raise BackendError(
            f"could not find `{candidate}`; install Paraegox or pass --paraegox-bin"
        )
    return resolved


def parse_args(arguments: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Optional Textual chat client for Paraegox"
    )
    parser.add_argument("--target", required=True, help="target Paraegox NodeId")
    parser.add_argument(
        "--connect",
        default="tcp/127.0.0.1:7447",
        help="loopback Fabric endpoint",
    )
    parser.add_argument(
        "--timeout-ms",
        type=_timeout_ms,
        default=30_000,
        help="Agent Turn deadline, from 100 to 60000 ms",
    )
    parser.add_argument(
        "--paraegox-bin",
        help="path or executable name for the Paraegox Rust binary",
    )
    return parser.parse_args(arguments)


def main(arguments: Optional[Sequence[str]] = None) -> int:
    options = parse_args(arguments)
    try:
        binary = _resolve_binary(options.paraegox_bin)
    except BackendError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    backend = JsonlBackend(
        binary,
        options.target,
        options.connect,
        options.timeout_ms,
    )
    app = ParaegoxTextual(backend, options.target, options.connect)
    app.run()
    return app.return_code or 0


if __name__ == "__main__":
    raise SystemExit(main())
