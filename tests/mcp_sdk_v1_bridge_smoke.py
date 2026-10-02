"""Exercise authenticated bridge rejection, wait cancellation, and reconnect recovery.

Install the pinned client dependencies from ``tests/requirements-mcp-sdk-v1.txt``
and run ``python tests/mcp_sdk_v1_bridge_smoke.py --binary target/release/lapui.exe``.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib.metadata
import json
import os
import socket
import time
import uuid
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


ROOT = Path(__file__).resolve().parents[1]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def bridge_descriptor_path(bridge_id: str) -> Path:
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA")
        if not base:
            raise RuntimeError("Lapui bridge requires LOCALAPPDATA on Windows")
        root = Path(base)
    else:
        runtime = os.environ.get("XDG_RUNTIME_DIR")
        root = Path(runtime) if runtime else Path.home() / ".local" / "state"
    return root / "lapui" / "mcp-bridge" / f"{bridge_id}.json"


def result_data(result: Any) -> tuple[dict[str, Any], bool]:
    data = getattr(result, "structuredContent", None)
    if data is None:
        data = getattr(result, "structured_content", None)
    if data is None:
        for block in getattr(result, "content", []):
            if getattr(block, "type", None) == "text":
                try:
                    data = json.loads(block.text)
                    break
                except ValueError:
                    continue
    require(isinstance(data, dict), f"expected structured MCP response, got {data!r}")
    is_error = bool(
        getattr(result, "isError", False) or getattr(result, "is_error", False)
    )
    return data, is_error


async def call(
    session: ClientSession,
    name: str,
    arguments: dict[str, Any] | None = None,
) -> dict[str, Any]:
    result = await session.call_tool(name, arguments=arguments or {})
    data, is_error = result_data(result)
    require(not is_error, f"{name} returned an MCP error: {data}")
    return data


async def call_allow_error(
    session: ClientSession, name: str, arguments: dict[str, Any]
) -> tuple[dict[str, Any], bool]:
    result = await session.call_tool(name, arguments=arguments)
    return result_data(result)


async def run(binary: Path) -> None:
    sdk_version = importlib.metadata.version("mcp")
    require(sdk_version == "1.30.0", f"expected MCP SDK 1.30.0, got {sdk_version}")
    require(binary.is_file(), f"Lapui binary not found: {binary}")

    bridge_id = f"sdk-v1-{uuid.uuid4().hex}"
    descriptor = bridge_descriptor_path(bridge_id)
    protocol_version = "unknown"
    app = await asyncio.create_subprocess_exec(
        str(binary),
        "--demo",
        "files",
        "--debug-trace",
        "--mcp-bridge-id",
        bridge_id,
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.DEVNULL,
    )

    @asynccontextmanager
    async def open_adapter():
        nonlocal protocol_version
        parameters = StdioServerParameters(
            command=str(binary), args=["mcp-stdio", bridge_id]
        )
        async with stdio_client(parameters) as (read, write):
            async with ClientSession(read, write) as session:
                initialized = await session.initialize()
                require(initialized is not None, "MCP adapter initialization returned no result")
                protocol_version = getattr(initialized, "protocolVersion", "unknown")
                yield session

    try:
        deadline = time.monotonic() + 15
        while not descriptor.exists() and app.returncode is None and time.monotonic() < deadline:
            await asyncio.sleep(0.05)
            require(
                descriptor.is_file() and app.returncode is None,
                "bridge window did not publish its descriptor",
            )

        bridge = json.loads(descriptor.read_text(encoding="utf-8"))
        host, port = bridge["address"].rsplit(":", 1)
        with socket.create_connection((host, int(port)), timeout=2) as unauthenticated:
            unauthenticated.sendall(b"LAPUI-MCP/1 invalid-token\n")
            unauthenticated.settimeout(2)
            require(
                unauthenticated.recv(1) == b"",
                "bridge accepted an invalid capability token",
            )

        async with open_adapter() as session:
            controls = await call(session, "page_controls")
            old_epoch = controls["documentEpoch"]
            by_id = {item.get("id"): item for item in controls["controls"]}
            require("search" in by_id, "files demo search control is missing")

            for attempt in range(5):
                page_baseline = await call(
                    session, "page_changes", {"documentEpoch": old_epoch, "limit": 64}
                )
                wait_id = f"{bridge_id}-cancel-{attempt}"
                pending_wait = asyncio.create_task(
                    call_allow_error(
                        session,
                        "page_wait_for_changes",
                        {
                            "documentEpoch": old_epoch,
                            "cursor": page_baseline["cursor"],
                            "limit": 64,
                            "timeoutMs": 4000,
                            "waitId": wait_id,
                        },
                    )
                )
                await asyncio.sleep(0.05)
                cancel_deadline = time.monotonic() + 2
                cancelled = {"found": False}
                while not cancelled.get("found") and time.monotonic() < cancel_deadline:
                    if pending_wait.done():
                        wait_result, wait_is_error = await pending_wait
                        require(
                            not wait_is_error,
                            f"page wait failed before cancellation: {wait_result}",
                        )
                        break
                    cancelled = await call(
                        session, "page_cancel_wait", {"waitId": wait_id}
                    )
                    if not cancelled.get("found"):
                        await asyncio.sleep(0.05)
                if cancelled.get("found"):
                    wait_result, wait_is_error = await asyncio.wait_for(
                        pending_wait, timeout=3
                    )
                    require(
                        wait_is_error and wait_result.get("code") == "wait_cancelled",
                        f"pending page wait did not return wait_cancelled: {wait_result}",
                    )
                    break
            else:
                raise AssertionError("could not cancel an active page wait after five attempts")

            old_search_ref = by_id["search"]["ref"]
            baseline = await call(session, "changes", {"scope": "operations", "limit": 16})
            accepted = await call(
                session,
                "action_invoke",
                {
                    "action": "files.scan",
                    "requestId": f"{bridge_id}-scan",
                    "args": {},
                },
            )
            operation_id = accepted.get("result", {}).get("operationId")
            require(
                isinstance(operation_id, str) and operation_id,
                f"scan was not accepted: {accepted}",
            )

            reloaded = await call(session, "page_reload", {"documentEpoch": old_epoch})
            require(
                reloaded.get("execution") == "completed"
                and reloaded.get("documentEpoch") != old_epoch,
                f"page reload did not create a new document epoch: {reloaded}",
            )
            stale, stale_is_error = await call_allow_error(
                session,
                "page_control",
                {
                    "operation": "fill",
                    "controlRef": old_search_ref,
                    "documentEpoch": old_epoch,
                    "value": "stale",
                },
            )
            require(
                stale_is_error and stale.get("code") == "stale_document",
                f"old page reference was not rejected after reload: {stale}",
            )

        require(app.returncode is None, "bridge window exited when adapter 1 disconnected")
        async with open_adapter() as session:
            operation = await call(
                session,
                "operation",
                {"operation": "status", "operationId": operation_id},
            )
            deadline = time.monotonic() + 20
            while operation.get("execution") not in ("completed", "failed", "cancelled"):
                require(time.monotonic() < deadline, f"operation did not finish: {operation}")
                operation = await call(
                    session,
                    "operation",
                    {
                        "operation": "wait",
                        "operationId": operation_id,
                        "afterRevision": operation["revision"],
                        "timeoutMs": 1000,
                    },
                )
            require(operation["execution"] == "completed", f"scan operation failed: {operation}")

            controls_after_reconnect = await call(session, "page_controls")
            require(
                controls_after_reconnect["documentEpoch"] != old_epoch,
                "reconnected adapter did not recover the reloaded window",
            )
            changes = await call(
                session,
                "changes",
                {"scope": "operations", "cursor": baseline["cursor"], "limit": 32},
            )
            require(
                any(
                    record.get("data", {}).get("operationId") == operation_id
                    for record in changes.get("records", [])
                ),
                f"reconnected adapter did not recover the operation change: {changes}",
            )
        require(app.returncode is None, "window process did not survive adapter reconnection")
        print(
            json.dumps(
                {
                    "sdk": sdk_version,
                    "protocol": protocol_version,
                    "unauthorizedBridgeRejected": True,
                    "waitCancelled": True,
                    "sameWindowReconnect": True,
                    "operationRecovered": True,
                    "changeFeedRecovered": True,
                },
                ensure_ascii=False,
            )
        )
    finally:
        if app.returncode is None:
            app.terminate()
            try:
                await asyncio.wait_for(app.wait(), timeout=5)
            except asyncio.TimeoutError:
                app.kill()
                await app.wait()
        descriptor.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    default_binary = ROOT / "target" / "release" / (
        "lapui.exe" if os.name == "nt" else "lapui"
    )
    parser.add_argument("--binary", type=Path, default=default_binary)
    options = parser.parse_args()
    asyncio.run(run(options.binary.resolve()))
