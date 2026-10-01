"""Dependency-free local HTTP, SSE, and WebSocket fixture for the network demo."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


_sequence = 0
_sequence_lock = threading.Lock()
_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def next_sequence() -> int:
    global _sequence
    with _sequence_lock:
        _sequence += 1
        return _sequence


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: object) -> None:
        pass

    def do_GET(self) -> None:
        if self.headers.get("Upgrade", "").lower() == "websocket":
            self._websocket()
            return

        request = urlsplit(self.path)
        if request.path == "/healthz":
            self._json(200, {"ok": True})
        elif request.path == "/api/items":
            query = parse_qs(request.query).get("q", [""])[0][:256]
            self._json(200, {
                "query": query,
                "items": [
                    {"id": f"{query}-{index}", "label": f"{query or 'item'} - result {index}"}
                    for index in range(1, 9)
                ],
            })
        elif request.path == "/events":
            self._events()
        else:
            self._json(404, {"error": "not found"})

    def _json(self, status: int, payload: object) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def _events(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        try:
            self.wfile.write(b"retry: 500\n\n")
            self.wfile.flush()
            while True:
                sequence = next_sequence()
                payload = json.dumps({
                    "sequence": sequence,
                    "message": f"local event {sequence}",
                }).encode("utf-8")
                self.wfile.write(
                    f"id: {sequence}\nevent: progress\ndata: ".encode("ascii")
                    + payload + b"\n\n"
                )
                self.wfile.flush()
                time.sleep(0.25)
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            self.close_connection = True

    def _websocket(self) -> None:
        key = self.headers.get("Sec-WebSocket-Key")
        if not key:
            self.send_error(400, "missing WebSocket key")
            return
        accept = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode("ascii")).digest()).decode("ascii")
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        try:
            while True:
                header = self.rfile.read(2)
                if len(header) != 2:
                    return
                first, second = header
                opcode = first & 0x0F
                if first & 0x70:
                    return
                masked = bool(second & 0x80)
                length = second & 0x7F
                if length == 126:
                    extended = self.rfile.read(2)
                    if len(extended) != 2:
                        return
                    length = int.from_bytes(extended, "big")
                elif length == 127:
                    extended = self.rfile.read(8)
                    if len(extended) != 8:
                        return
                    length = int.from_bytes(extended, "big")
                if length > 1024 * 1024:
                    self._write_frame(0x8, (1009).to_bytes(2, "big"))
                    return
                mask = self.rfile.read(4) if masked else b""
                payload = self.rfile.read(length)
                if len(payload) != length:
                    return
                if masked:
                    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
                if opcode == 0x8:
                    self._write_frame(0x8, payload[:125])
                    return
                if opcode == 0x9:
                    self._write_frame(0xA, payload)
                elif opcode in (0x1, 0x2):
                    self._write_frame(opcode, payload)
                else:
                    self._write_frame(0x8, (1002).to_bytes(2, "big"))
                    return
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            self.close_connection = True

    def _write_frame(self, opcode: int, payload: bytes) -> None:
        length = len(payload)
        if length < 126:
            header = bytes((0x80 | opcode, length))
        elif length <= 0xFFFF:
            header = bytes((0x80 | opcode, 126)) + length.to_bytes(2, "big")
        else:
            header = bytes((0x80 | opcode, 127)) + length.to_bytes(8, "big")
        self.wfile.write(header + payload)
        self.wfile.flush()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    with FixtureServer((args.host, args.port), Handler) as server:
        print(f"Lapui network fixture listening on http://{args.host}:{args.port}", flush=True)
        try:
            server.serve_forever(poll_interval=0.2)
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
