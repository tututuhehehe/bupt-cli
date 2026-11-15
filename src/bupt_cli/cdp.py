"""Minimal Chrome DevTools Protocol client (standard library only).

No Playwright / Selenium / agent-browser: the CLI speaks CDP over a raw
WebSocket to a Chrome it launches itself, so there is one dependency fewer and
no npm toolchain in the loop.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import socket
import struct
from typing import Any, Optional


_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class CDPError(Exception):
    """A CDP command returned an error."""


class CDP:
    """A single page target on one Chrome instance."""

    def __init__(self, ws_url: str, *, timeout: float = 30.0) -> None:
        # ws://127.0.0.1:<port>/devtools/page/<id>
        host_port, _, path = ws_url.partition("/devtools/")
        host, _, port = host_port.split("//", 1)[1].rpartition(":")
        self._sock = socket.create_connection((host, int(port)), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /devtools/{path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self._next_id = 0
        self._pending = b""
        try:
            self._sock.sendall(request.encode())
            buffer = b""
            while b"\r\n\r\n" not in buffer:
                chunk = self._sock.recv(4096)
                if not chunk:
                    raise CDPError("WebSocket handshake failed")
                buffer += chunk
            header_blob, _, self._pending = buffer.partition(b"\r\n\r\n")
            if not header_blob.startswith(b"HTTP/1.1 101"):
                raise CDPError(f"WebSocket handshake rejected: {header_blob[:60]!r}")
            expected = base64.b64encode(
                hashlib.sha1((key + _WS_GUID).encode()).digest()
            ).decode()
            if expected.encode() not in header_blob:
                raise CDPError("WebSocket handshake: Sec-WebSocket-Accept mismatch")
        except BaseException:
            # Never leak the socket when the handshake does not complete.
            try:
                self._sock.close()
            except OSError:
                pass
            raise

    # -- framing -----------------------------------------------------------
    def _send_text(self, payload: str) -> None:
        data = payload.encode()
        frame = bytearray([0x81])  # FIN + text
        length = len(data)
        if length < 126:
            frame.append(0x80 | length)
        elif length < 65536:
            frame.append(0x80 | 126)
            frame += struct.pack(">H", length)
        else:
            frame.append(0x80 | 127)
            frame += struct.pack(">Q", length)
        mask = os.urandom(4)
        frame += mask
        frame += bytes(byte ^ mask[i % 4] for i, byte in enumerate(data))
        self._sock.sendall(bytes(frame))

    def _read_exact(self, count: int) -> bytes:
        buffer = b""
        # Frames that arrived in the same packet as the handshake response.
        if self._pending:
            take = min(count, len(self._pending))
            buffer, self._pending = self._pending[:take], self._pending[take:]
        while len(buffer) < count:
            chunk = self._sock.recv(count - len(buffer))
            if not chunk:
                raise CDPError("connection closed")
            buffer += chunk
        return buffer

    def _recv_text(self) -> str:
        """Return the next text frame, skipping control frames.

        Chrome may interleave ping/pong frames; treating one as JSON aborts the
        command loop, so they are consumed silently. Continuation frames are
        concatenated until FIN is set.
        """
        parts: list[bytes] = []
        while True:
            header = self._read_exact(2)
            opcode = header[0] & 0x0F
            length = header[1] & 0x7F
            if length == 126:
                length = struct.unpack(">H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self._read_exact(8))[0]
            payload = self._read_exact(length) if length else b""

            if opcode == 0x8:  # close
                raise CDPError("WebSocket closed by peer")
            if opcode in (0x9, 0xA):  # ping / pong
                continue
            if opcode == 0x2:  # binary: CDP is JSON-only, ignore the payload
                if header[0] & 0x80:
                    continue
                continue
            if opcode in (0x1, 0x0):  # text / continuation
                parts.append(payload)
                if header[0] & 0x80:  # FIN
                    return b"".join(parts).decode("utf-8", "ignore")
                continue
            # Unknown opcode: ignore rather than desynchronise the stream.
            if header[0] & 0x80:
                continue

    # -- commands ----------------------------------------------------------
    def call(
        self, method: str, params: Optional[dict[str, Any]] = None, timeout: float = 30.0
    ) -> dict[str, Any]:
        self._next_id += 1
        message_id = self._next_id
        self._send_text(
            json.dumps({"id": message_id, "method": method, "params": params or {}})
        )
        self._sock.settimeout(timeout)
        while True:
            try:
                raw = self._recv_text()
            except (TimeoutError, socket.timeout) as exc:
                raise CDPError(f"CDP command timed out: {method}") from exc
            except OSError as exc:
                raise CDPError(f"CDP connection error: {exc}") from exc
            try:
                message = json.loads(raw)
            except ValueError as exc:
                raise CDPError(f"CDP sent non-JSON frame: {raw[:60]!r}") from exc
            if message.get("id") == message_id:
                if "error" in message:
                    raise CDPError(str(message["error"]))
                return message.get("result", {})

    def eval(self, expression: str, *, timeout: float = 30.0) -> Any:
        """Evaluate JS in the page and return its JSON-serialisable value."""
        result = self.call(
            "Runtime.evaluate",
            {
                "expression": expression,
                "returnByValue": True,
                "awaitPromise": True,
            },
            timeout=timeout,
        )
        return result.get("result", {}).get("value")

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass
