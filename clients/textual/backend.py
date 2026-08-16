"""Bounded v1 JSONL subprocess owner for the optional Textual client."""

from __future__ import annotations

import asyncio
import json
import unicodedata
from collections.abc import AsyncIterator
from contextlib import suppress
from typing import Any, Optional, Protocol
from uuid import UUID, uuid4


PROTOCOL_VERSION = 1
MAX_INPUT_BYTES = 4 * 1024
MAX_STDIN_FRAME_BYTES = 16 * 1024
MAX_STDOUT_FRAME_BYTES = 64 * 1024
STDOUT_STREAM_LIMIT = MAX_STDOUT_FRAME_BYTES + 1024
MAX_STDERR_BYTES = 32 * 1024
MAX_ERROR_BYTES = 512
SHUTDOWN_TIMEOUT_SECONDS = 5.0
TERMINATE_TIMEOUT_SECONDS = 1.0

READY_FIELDS = {
    "v",
    "type",
    "session_id",
    "turn_timeout_ms",
    "max_input_bytes",
}
CANCEL_RESULTS = {"cancellation_requested", "already_terminal", "not_active"}
TERMINALS = {"final", "cancelled", "timed_out", "failed"}


class BackendError(RuntimeError):
    """The bundled Rust stdio adapter failed or violated the fixed contract."""


class InputValidationError(BackendError):
    """The local input is invalid; the backend connection remains usable."""


class ChatBackend(Protocol):
    session_id: Optional[str]
    active_turn_id: Optional[str]

    async def start(self) -> None: ...

    def events(self) -> AsyncIterator[dict[str, Any]]: ...

    async def submit(self, text: str) -> str: ...

    async def cancel(self) -> bool: ...

    async def shutdown(self) -> None: ...


class JsonlBackend:
    """Own one Rust child, one Agent Session, and at most one active Turn."""

    def __init__(
        self,
        paraegox_bin: str,
        target: str,
        endpoint: str,
        timeout_ms: int,
    ) -> None:
        self._command = (
            paraegox_bin,
            "tui",
            "--stdio-jsonl",
            "--target",
            target,
            "--connect",
            endpoint,
            "--timeout-ms",
            str(timeout_ms),
        )
        self._configured_timeout_ms = timeout_ms
        self._process: Optional[asyncio.subprocess.Process] = None
        self._stderr_task: Optional[asyncio.Task[None]] = None
        self._stderr = bytearray()
        self._write_lock = asyncio.Lock()
        self._reader_done = asyncio.Event()
        self._shutdown_started = False
        self._cancel_sent = False
        self.session_id: Optional[str] = None
        self.active_turn_id: Optional[str] = None
        self.turn_timeout_ms = timeout_ms
        self.max_input_bytes = MAX_INPUT_BYTES

    async def start(self) -> None:
        if self._process is not None:
            raise BackendError("Paraegox backend was already started")
        try:
            self._process = await asyncio.create_subprocess_exec(
                *self._command,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                limit=STDOUT_STREAM_LIMIT,
            )
            self._stderr_task = asyncio.create_task(self._drain_stderr())
            ready = await asyncio.wait_for(
                self._read_frame(), self._configured_timeout_ms / 1000 + 1.0
            )
            self._accept_ready(ready)
        except BaseException:
            await self._force_stop()
            raise

    async def submit(self, text: str) -> str:
        session_id = self._require_session()
        if self.active_turn_id is not None:
            raise BackendError("Only one Agent Turn may be active")
        validate_input(text, self.max_input_bytes)
        turn_id = str(uuid4())
        self.active_turn_id = turn_id
        self._cancel_sent = False
        try:
            await self._write_frame(
                {
                    "v": PROTOCOL_VERSION,
                    "type": "submit",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "input": text,
                }
            )
        except BaseException:
            self.active_turn_id = None
            raise
        return turn_id

    async def cancel(self) -> bool:
        session_id = self._require_session()
        turn_id = self.active_turn_id
        if turn_id is None or self._cancel_sent:
            return False
        await self._write_frame(
            {
                "v": PROTOCOL_VERSION,
                "type": "cancel",
                "session_id": session_id,
                "turn_id": turn_id,
            }
        )
        self._cancel_sent = True
        return True

    async def events(self) -> AsyncIterator[dict[str, Any]]:
        if self.session_id is None:
            raise BackendError("Paraegox backend is not ready")
        try:
            while True:
                frame = await self._read_frame()
                self._validate_event(frame)
                frame_type = frame["type"]
                if frame_type == "turn_terminal":
                    self.active_turn_id = None
                    self._cancel_sent = False
                elif frame_type == "turn_error":
                    self.active_turn_id = None
                    self._cancel_sent = False
                    yield frame
                    return
                elif frame_type == "stopped":
                    yield frame
                    return
                yield frame
        finally:
            self._reader_done.set()

    async def shutdown(self) -> None:
        if self._shutdown_started:
            await self._wait_for_process()
            return
        self._shutdown_started = True
        process = self._process
        if process is None:
            return

        try:
            await asyncio.wait_for(
                self._request_graceful_shutdown(),
                timeout=SHUTDOWN_TIMEOUT_SECONDS,
            )
        except asyncio.TimeoutError:
            await self._force_stop()
            raise BackendError(
                "Paraegox backend did not stop within the shutdown deadline"
            )
        finally:
            await self._finish_stderr()

    def stderr_text(self) -> str:
        return self._stderr.decode("utf-8", errors="replace")

    async def _request_graceful_shutdown(self) -> None:
        process = self._process
        if process is None:
            return
        if process.returncode is None and self.session_id is not None:
            with suppress(BackendError, BrokenPipeError, ConnectionError):
                await self.cancel()
                await self._write_frame(
                    {
                        "v": PROTOCOL_VERSION,
                        "type": "shutdown",
                        "session_id": self.session_id,
                    }
                )
        await self._graceful_exit()

    async def _graceful_exit(self) -> None:
        process = self._process
        if process is None:
            return
        await self._reader_done.wait()
        return_code = await process.wait()
        if return_code != 0:
            detail = bounded_text(self.stderr_text(), MAX_ERROR_BYTES).strip()
            suffix = f": {detail}" if detail else ""
            raise BackendError(
                f"Paraegox backend exited with status {return_code}{suffix}"
            )

    def _accept_ready(self, frame: dict[str, Any]) -> None:
        require_exact_fields(frame, READY_FIELDS, "ready")
        if frame["v"] != PROTOCOL_VERSION or frame["type"] != "ready":
            raise BackendError("Paraegox backend did not send a v1 ready frame")
        session_id = canonical_uuid(frame["session_id"], "ready session_id")
        timeout_ms = frame["turn_timeout_ms"]
        max_input_bytes = frame["max_input_bytes"]
        if timeout_ms != self._configured_timeout_ms:
            raise BackendError("Paraegox backend reported a different Turn deadline")
        if max_input_bytes != MAX_INPUT_BYTES:
            raise BackendError("Paraegox backend reported an unsupported input limit")
        self.session_id = session_id
        self.turn_timeout_ms = timeout_ms
        self.max_input_bytes = max_input_bytes

    def _validate_event(self, frame: dict[str, Any]) -> None:
        if frame.get("v") != PROTOCOL_VERSION:
            raise BackendError("Paraegox backend sent an unsupported protocol version")
        frame_type = frame.get("type")
        if frame_type == "cancel_result":
            require_exact_fields(
                frame, {"v", "type", "session_id", "turn_id", "result"}, frame_type
            )
            self._require_active_identity(frame)
            if frame["result"] not in CANCEL_RESULTS:
                raise BackendError("Paraegox backend sent an invalid cancel result")
        elif frame_type == "turn_terminal":
            require_exact_fields(frame, {"v", "type", "result"}, frame_type)
            result = frame["result"]
            if not isinstance(result, dict):
                raise BackendError("Paraegox terminal result must be an object")
            require_exact_fields(
                result, {"session_id", "turn_id", "terminal"}, "turn result"
            )
            self._require_active_identity(result)
            self._validate_terminal(result["terminal"])
        elif frame_type == "turn_error":
            require_exact_fields(
                frame,
                {"v", "type", "session_id", "turn_id", "message"},
                frame_type,
            )
            self._require_active_identity(frame)
            if not isinstance(frame["message"], str):
                raise BackendError("Paraegox Turn error message must be text")
            if len(frame["message"].encode("utf-8")) > MAX_ERROR_BYTES:
                raise BackendError("Paraegox Turn error message exceeds 512 bytes")
        elif frame_type == "stopped":
            require_exact_fields(frame, {"v", "type", "session_id"}, frame_type)
            self._require_session_identity(frame["session_id"])
        else:
            raise BackendError("Paraegox backend sent an unknown event")

    def _validate_terminal(self, terminal: Any) -> None:
        if not isinstance(terminal, dict):
            raise BackendError("Paraegox Turn terminal must be an object")
        kind = terminal.get("terminal")
        if kind not in TERMINALS:
            raise BackendError("Paraegox backend sent an unknown Turn terminal")
        expected = {"terminal", "content"} if kind == "final" else {"terminal"}
        if kind == "failed":
            expected.add("reason")
        require_exact_fields(terminal, expected, "Turn terminal")
        if kind == "final":
            content = terminal["content"]
            if not isinstance(content, str):
                raise BackendError("Paraegox final content must be text")
        if kind == "failed" and not isinstance(terminal["reason"], str):
            raise BackendError("Paraegox failure reason must be text")

    def _require_active_identity(self, value: dict[str, Any]) -> None:
        self._require_session_identity(value.get("session_id"))
        turn_id = canonical_uuid(value.get("turn_id"), "TurnId")
        if self.active_turn_id is None or turn_id != self.active_turn_id:
            raise BackendError("Paraegox event does not match the active Turn")

    def _require_session_identity(self, session_id: Any) -> None:
        canonical = canonical_uuid(session_id, "SessionId")
        if self.session_id is None or canonical != self.session_id:
            raise BackendError("Paraegox event does not match this Session")

    def _require_session(self) -> str:
        if self.session_id is None:
            raise BackendError("Paraegox backend is not ready")
        return self.session_id

    async def _write_frame(self, frame: dict[str, Any]) -> None:
        encoded = json.dumps(
            frame, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
        if len(encoded) > MAX_STDIN_FRAME_BYTES:
            raise BackendError("Paraegox request exceeds the 16 KiB frame limit")
        process = self._process
        if process is None or process.stdin is None or process.returncode is not None:
            raise BackendError("Paraegox backend is not writable")
        async with self._write_lock:
            process.stdin.write(encoded + b"\n")
            try:
                await process.stdin.drain()
            except (BrokenPipeError, ConnectionResetError) as error:
                raise BackendError("Paraegox backend closed stdin") from error

    async def _read_frame(self) -> dict[str, Any]:
        process = self._process
        if process is None or process.stdout is None:
            raise BackendError("Paraegox backend stdout is unavailable")
        try:
            line = await process.stdout.readline()
        except (ValueError, asyncio.LimitOverrunError) as error:
            raise BackendError("Paraegox backend exceeded the stdout frame limit") from error
        if not line:
            detail = bounded_text(self.stderr_text(), MAX_ERROR_BYTES).strip()
            suffix = f": {detail}" if detail else ""
            raise BackendError(f"Paraegox backend closed stdout{suffix}")
        if len(line) > MAX_STDOUT_FRAME_BYTES + 1 or not line.endswith(b"\n"):
            raise BackendError("Paraegox backend sent an invalid JSONL frame")
        try:
            frame = json.loads(line[:-1].decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise BackendError("Paraegox backend sent invalid UTF-8 JSON") from error
        if not isinstance(frame, dict):
            raise BackendError("Paraegox backend frame must be a JSON object")
        return frame

    async def _drain_stderr(self) -> None:
        process = self._process
        if process is None or process.stderr is None:
            return
        while True:
            chunk = await process.stderr.read(4096)
            if not chunk:
                return
            self._stderr.extend(chunk)
            overflow = len(self._stderr) - MAX_STDERR_BYTES
            if overflow > 0:
                del self._stderr[:overflow]

    async def _wait_for_process(self) -> None:
        process = self._process
        if process is None or process.returncode is not None:
            await self._finish_stderr()
            return
        try:
            await asyncio.wait_for(process.wait(), timeout=SHUTDOWN_TIMEOUT_SECONDS)
        except asyncio.TimeoutError:
            await self._force_stop()
        await self._finish_stderr()

    async def _force_stop(self) -> None:
        process = self._process
        if process is not None and process.returncode is None:
            with suppress(ProcessLookupError):
                process.terminate()
            try:
                await asyncio.wait_for(
                    process.wait(), timeout=TERMINATE_TIMEOUT_SECONDS
                )
            except asyncio.TimeoutError:
                with suppress(ProcessLookupError):
                    process.kill()
                await process.wait()
        self._reader_done.set()
        await self._finish_stderr()

    async def _finish_stderr(self) -> None:
        task = self._stderr_task
        if task is None:
            return
        self._stderr_task = None
        try:
            await asyncio.wait_for(task, timeout=TERMINATE_TIMEOUT_SECONDS)
        except asyncio.TimeoutError:
            task.cancel()
            with suppress(asyncio.CancelledError):
                await task


def require_exact_fields(
    value: dict[str, Any], expected: set[str], description: str
) -> None:
    if set(value) != expected:
        raise BackendError(f"Paraegox {description} fields do not match protocol v1")


def canonical_uuid(value: Any, description: str) -> str:
    if not isinstance(value, str):
        raise BackendError(f"Paraegox {description} must be a UUID string")
    try:
        parsed = UUID(value)
    except (ValueError, AttributeError) as error:
        raise BackendError(f"Paraegox {description} is not a valid UUID") from error
    canonical = str(parsed)
    if canonical != value:
        raise BackendError(f"Paraegox {description} must use canonical UUID form")
    return canonical


def validate_input(text: str, max_bytes: int) -> None:
    if not text.strip():
        raise InputValidationError("Agent input must not be empty")
    if len(text.encode("utf-8")) > max_bytes:
        raise InputValidationError("Agent input exceeds 4 KiB")
    if any(unicodedata.category(character) == "Cc" for character in text):
        raise InputValidationError("Agent input must not contain control characters")


def bounded_text(text: str, max_bytes: int) -> str:
    safe = "".join(
        character
        if character in "\n\t" or unicodedata.category(character) != "Cc"
        else "�"
        for character in text
    )
    return truncate_utf8(safe, max_bytes, ellipsis=True)


def truncate_utf8(text: str, max_bytes: int, ellipsis: bool = False) -> str:
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    suffix = "…" if ellipsis and max_bytes >= 3 else ""
    prefix_bytes = max_bytes - len(suffix.encode("utf-8"))
    prefix = encoded[:prefix_bytes].decode("utf-8", errors="ignore")
    return prefix + suffix
