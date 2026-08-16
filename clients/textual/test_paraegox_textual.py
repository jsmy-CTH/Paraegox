import asyncio
import unittest
from typing import Any, Optional
from uuid import uuid4

from textual.widgets import Input, RichLog

from backend import PROTOCOL_VERSION
from paraegox_textual import ParaegoxTextual


class FakeBackend:
    def __init__(self) -> None:
        self.session_id: Optional[str] = None
        self.active_turn_id: Optional[str] = None
        self.operations: list[tuple[Any, ...]] = []
        self._events: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._cancel_sent = False
        self._shutdown = False
        self._stopped_seen = asyncio.Event()
        self._reader_done = asyncio.Event()

    async def start(self) -> None:
        self.session_id = str(uuid4())
        self.operations.append(("start",))

    async def events(self):
        try:
            while True:
                frame = await self._events.get()
                yield frame
                if frame["type"] == "stopped":
                    self._stopped_seen.set()
                    return
        finally:
            self._reader_done.set()

    async def submit(self, text: str) -> str:
        if self.active_turn_id is not None:
            raise AssertionError("test backend received a queued Turn")
        turn_id = str(uuid4())
        self.active_turn_id = turn_id
        self._cancel_sent = False
        self.operations.append(("submit", self.session_id, turn_id, text))
        return turn_id

    async def cancel(self) -> bool:
        if self.active_turn_id is None or self._cancel_sent:
            return False
        self._cancel_sent = True
        self.operations.append(
            ("cancel", self.session_id, self.active_turn_id)
        )
        return True

    async def shutdown(self) -> None:
        if self._shutdown:
            return
        self._shutdown = True
        if self.active_turn_id is not None:
            await self.cancel()
            await self._events.put(
                {
                    "v": PROTOCOL_VERSION,
                    "type": "cancel_result",
                    "session_id": self.session_id,
                    "turn_id": self.active_turn_id,
                    "result": "cancellation_requested",
                }
            )
            await self.emit_terminal(self.active_turn_id, {"terminal": "cancelled"})
        self.operations.append(("shutdown", self.session_id))
        await self._events.put(
            {
                "v": PROTOCOL_VERSION,
                "type": "stopped",
                "session_id": self.session_id,
            }
        )
        stopped = asyncio.create_task(self._stopped_seen.wait())
        reader_done = asyncio.create_task(self._reader_done.wait())
        done, pending = await asyncio.wait(
            {stopped, reader_done}, timeout=1, return_when=asyncio.FIRST_COMPLETED
        )
        for task in pending:
            task.cancel()
        if not done:
            raise TimeoutError("fake backend reader did not close")

    async def emit_terminal(
        self, turn_id: str, terminal: dict[str, Any]
    ) -> None:
        await self._events.put(
            {
                "v": PROTOCOL_VERSION,
                "type": "turn_terminal",
                "result": {
                    "session_id": self.session_id,
                    "turn_id": turn_id,
                    "terminal": terminal,
                },
            }
        )
        self.active_turn_id = None
        self._cancel_sent = False


class HeadlessParaegoxTextual(ParaegoxTextual):
    """Capture the production quit result while retaining Textual exit semantics."""

    def __init__(self, backend: FakeBackend, target: str, endpoint: str) -> None:
        super().__init__(backend, target, endpoint)
        self.exit_seen = asyncio.Event()
        self.shutdown_worker_done = asyncio.Event()
        self.exit_code: Optional[int] = None

    async def _shutdown_and_exit(self) -> None:
        try:
            await super()._shutdown_and_exit()
        finally:
            self.shutdown_worker_done.set()

    def exit(
        self,
        result: None = None,
        return_code: int = 0,
        message: Any = None,
    ) -> None:
        self.exit_code = return_code
        self.exit_seen.set()
        super().exit(result=result, return_code=return_code, message=message)


class TextualChatTest(unittest.IsolatedAsyncioTestCase):
    async def test_submit_literal_final_cancel_and_orderly_close(self) -> None:
        backend = FakeBackend()
        app = HeadlessParaegoxTextual(
            backend, "agent-chat-node", "tcp/127.0.0.1:7447"
        )

        async def run_scenario() -> str:
            async with app.run_test(size=(120, 30)) as pilot:
                await pilot.pause(0.01)
                editor = app.query_one("#message-input", Input)
                self.assertFalse(editor.disabled)

                editor.value = "first"
                editor.focus()
                await pilot.press("enter")
                await pilot.pause(0.01)
                first_turn = backend.operations[-1][2]
                literal = "[bold]literal model text[/bold]"
                await backend.emit_terminal(
                    first_turn, {"terminal": "final", "content": literal}
                )
                await pilot.pause(0.01)

                chat = app.query_one("#chat-log", RichLog)
                rendered = "\n".join(strip.text for strip in chat.lines)
                self.assertIn(literal, rendered)
                self.assertEqual(app.messages[-1].content, literal)

                editor.value = "cancel this"
                editor.focus()
                await pilot.press("enter")
                await pilot.pause(0.01)
                second_turn = backend.operations[-1][2]
                await pilot.press("escape")
                await pilot.pause(0.01)
                self.assertIn(
                    ("cancel", backend.session_id, second_turn), backend.operations
                )
                await backend.emit_terminal(second_turn, {"terminal": "cancelled"})
                await pilot.pause(0.01)
                self.assertIsNone(app.active_turn_id)

                editor.value = "close while active"
                editor.focus()
                await pilot.press("enter")
                await pilot.pause(0.01)
                closing_turn = backend.operations[-1][2]
                app.action_quit_orderly()
                await asyncio.wait_for(app.exit_seen.wait(), timeout=1)
                await asyncio.wait_for(app.shutdown_worker_done.wait(), timeout=1)
                await asyncio.wait_for(asyncio.shield(app._task), timeout=1)
                self.assertEqual(app.exit_code, 0)
                return closing_turn

        closing_turn = await asyncio.wait_for(run_scenario(), timeout=10)

        closing_cancel = ("cancel", backend.session_id, closing_turn)
        shutdown = ("shutdown", backend.session_id)
        self.assertIn(closing_cancel, backend.operations)
        self.assertIn(shutdown, backend.operations)
        self.assertLess(
            backend.operations.index(closing_cancel), backend.operations.index(shutdown)
        )
        submit_turn_ids = [
            operation[2]
            for operation in backend.operations
            if operation[0] == "submit"
        ]
        self.assertEqual(len(submit_turn_ids), len(set(submit_turn_ids)))


if __name__ == "__main__":
    unittest.main()
