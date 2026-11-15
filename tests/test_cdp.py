"""Protocol tests for the hand-rolled CDP WebSocket client.

A tiny scripted WebSocket server stands in for Chrome, so these run offline and
never launch a browser. The framer and the handshake are the parts most likely
to break silently, hence the attention here.
"""

import base64
import hashlib
import json
import socket
import struct
import threading
import time

import pytest

from bupt_cli.cdp import CDP, CDPError

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def server_frame(payload: bytes, opcode: int = 0x1, fin: bool = True) -> bytes:
    """Build an unmasked server->client frame (RFC 6455 §5.2)."""
    header = bytearray([(0x80 if fin else 0x00) | opcode])
    length = len(payload)
    if length < 126:
        header.append(length)
    elif length < 65536:
        header.append(126)
        header += struct.pack(">H", length)
    else:
        header.append(127)
        header += struct.pack(">Q", length)
    return bytes(header) + payload


def read_client_frame(conn: socket.socket) -> dict:
    """Read one masked client frame and return the decoded JSON payload."""
    def take(n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = conn.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("client closed")
            buf += chunk
        return buf

    head = take(2)
    opcode = head[0] & 0x0F
    masked = bool(head[1] & 0x80)
    length = head[1] & 0x7F
    if length == 126:
        length = struct.unpack(">H", take(2))[0]
    elif length == 127:
        length = struct.unpack(">Q", take(8))[0]
    mask = take(4) if masked else b"\x00\x00\x00\x00"
    raw = take(length) if length else b""
    payload = bytes(b ^ mask[i % 4] for i, b in enumerate(raw))
    return {"opcode": opcode, "masked": masked, "payload": payload}


class FakeChrome:
    """One-shot WebSocket server scripted with a list of (stage, bytes)."""

    def __init__(
        self,
        *,
        frames: list[bytes] | None = None,
        accept_override: str | None = None,
        status: bytes = b"HTTP/1.1 101 Switching Protocols",
        handshake_extra: bytes = b"",
        silence: bool = False,
    ) -> None:
        self.frames = frames or []
        self.accept_override = accept_override
        self.status = status
        self.handshake_extra = handshake_extra
        self.silence = silence
        self.captured: list[dict] = []
        self._sock = socket.socket()
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(1)
        self.port = self._sock.getsockname()[1]
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}/devtools/page/test"

    def _serve(self) -> None:
        try:
            conn, _ = self._sock.accept()
        except OSError:
            return
        with conn:
            conn.settimeout(5)
            request = b""
            while b"\r\n\r\n" not in request:
                chunk = conn.recv(4096)
                if not chunk:
                    return
                request += chunk
            key = ""
            for line in request.split(b"\r\n"):
                if line.lower().startswith(b"sec-websocket-key:"):
                    key = line.split(b":", 1)[1].strip().decode()
            accept = self.accept_override or base64.b64encode(
                hashlib.sha1((key + GUID).encode()).digest()
            ).decode()

            response = (
                self.status + b"\r\n"
                b"Upgrade: websocket\r\n"
                b"Connection: Upgrade\r\n"
                b"Sec-WebSocket-Accept: " + accept.encode() + b"\r\n\r\n"
            )
            try:
                conn.sendall(response + self.handshake_extra)
            except OSError:
                return

            if self.silence:
                time.sleep(1.5)
                return

            try:
                # Capture the command the client sends, then reply.
                self.captured.append(read_client_frame(conn))
                for frame in self.frames:
                    conn.sendall(frame)
            except (OSError, ConnectionError):
                return

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass


def _response(payload: dict, frame_id: int = 1) -> bytes:
    body = json.dumps({"id": frame_id, "result": payload}).encode()
    return server_frame(body)


# ---------------------------------------------------------------------------


def test_handshake_and_simple_command():
    server = FakeChrome(frames=[_response({"ok": 1})])
    try:
        cdp = CDP(server.url)
        assert cdp.call("Runtime.evaluate", {"expression": "1"}) == {"ok": 1}
        cdp.close()
    finally:
        server.close()
    assert server.captured[0]["masked"] is True
    assert json.loads(server.captured[0]["payload"])["method"] == "Runtime.evaluate"


def test_frame_arriving_with_the_handshake_is_not_lost():
    """Chrome may coalesce the first frame with the 101 response."""
    server = FakeChrome(
        handshake_extra=_response({"coalesced": True}), frames=[]
    )
    try:
        cdp = CDP(server.url)
        assert cdp.call("X") == {"coalesced": True}
        cdp.close()
    finally:
        server.close()


def test_fragmented_text_frames_are_joined():
    body = json.dumps({"id": 1, "result": {"frag": True}}).encode()
    head, tail = body[:10], body[10:]
    server = FakeChrome(
        frames=[
            server_frame(head, opcode=0x1, fin=False),
            server_frame(tail, opcode=0x0, fin=True),
        ]
    )
    try:
        cdp = CDP(server.url)
        assert cdp.call("X") == {"frag": True}
        cdp.close()
    finally:
        server.close()


def test_ping_between_fragments_is_skipped():
    """A ping carries a non-JSON payload here: if it were appended to the text
    fragments (or parsed as a message), the reply could not be decoded."""
    body = json.dumps({"id": 1, "result": {"pinged": True}}).encode()
    head, tail = body[:8], body[8:]
    server = FakeChrome(
        frames=[
            server_frame(head, opcode=0x1, fin=False),
            server_frame(b"NOT-JSON-PING", opcode=0x9, fin=True),  # ping
            server_frame(tail, opcode=0x0, fin=True),
        ]
    )
    try:
        cdp = CDP(server.url)
        assert cdp.call("X") == {"pinged": True}
        cdp.close()
    finally:
        server.close()


def test_large_payload_uses_64bit_length():
    big = "x" * 70000
    server = FakeChrome(frames=[_response({"big": big})])
    try:
        cdp = CDP(server.url)
        assert cdp.call("X")["big"] == big
        cdp.close()
    finally:
        server.close()


def test_event_frames_without_id_are_ignored():
    event = server_frame(json.dumps({"method": "Page.loadEventFired"}).encode())
    server = FakeChrome(frames=[event, _response({"after": "event"})])
    try:
        cdp = CDP(server.url)
        assert cdp.call("X") == {"after": "event"}
        cdp.close()
    finally:
        server.close()


def test_close_frame_raises_cdp_error():
    """Assert the *close* path specifically.

    A bare `pytest.raises(CDPError)` also passes if the close frame is ignored
    and the connection later dies with "connection closed", so the message is
    matched too.
    """
    server = FakeChrome(frames=[server_frame(b"", opcode=0x8, fin=True)])
    try:
        cdp = CDP(server.url)
        with pytest.raises(CDPError, match="closed by peer"):
            cdp.call("X")
        cdp.close()
    finally:
        server.close()


def test_binary_frame_is_ignored_not_grafted_onto_json():
    """CDP is JSON-only; a binary frame must not corrupt the next text frame."""
    good = _response({"after": "binary"})
    server = FakeChrome(
        frames=[server_frame(b"\x00\x01\x02", opcode=0x2, fin=True), good]
    )
    try:
        cdp = CDP(server.url)
        assert cdp.call("X") == {"after": "binary"}
        cdp.close()
    finally:
        server.close()


def test_bad_accept_raises_cdp_error():
    server = FakeChrome(accept_override="not-the-right-token")
    try:
        with pytest.raises(CDPError):
            CDP(server.url)
    finally:
        server.close()


def test_non_101_response_raises_cdp_error():
    server = FakeChrome(status=b"HTTP/1.1 403 Forbidden")
    try:
        with pytest.raises(CDPError):
            CDP(server.url)
    finally:
        server.close()


def test_timeout_surfaces_as_cdp_error():
    server = FakeChrome(silence=True)
    try:
        cdp = CDP(server.url)
        with pytest.raises(CDPError):
            cdp.call("X", timeout=0.3)
        cdp.close()
    finally:
        server.close()


def test_command_error_is_raised():
    body = json.dumps({"id": 1, "error": {"code": -1, "message": "boom"}}).encode()
    server = FakeChrome(frames=[server_frame(body)])
    try:
        cdp = CDP(server.url)
        with pytest.raises(CDPError):
            cdp.call("X")
        cdp.close()
    finally:
        server.close()
