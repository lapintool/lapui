"""Interop smoke for the latest supported Python MCP SDK v2 client line.

Install with ``python -m pip install -r tests/requirements-mcp-sdk-v2.txt`` and
run with ``python tests/mcp_sdk_v2_smoke.py [path-to-lapui]`` after building.
The test uses a temporary directory and verifies that app-owned metadata never
modifies the indexed file.
"""

from __future__ import annotations

import asyncio
import importlib.metadata
import json
import sys
import tempfile
from pathlib import Path

from mcp import Client, StdioServerParameters


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def action_result(response: object) -> dict:
    structured = getattr(response, "structured_content", None)
    require(isinstance(structured, dict), "expected a structured MCP response")
    require(not getattr(response, "is_error", False), f"MCP tool returned error: {structured}")
    result = structured.get("result")
    require(isinstance(result, dict), f"action did not return a result object: {structured}")
    return result


async def run(binary: Path) -> None:
    sdk_version = importlib.metadata.version("mcp")
    require(sdk_version == "2.2.0", f"expected pinned MCP SDK 2.2.0, got {sdk_version}")
    require(binary.is_file(), f"Lapui binary not found: {binary}")

    with tempfile.TemporaryDirectory(prefix="lapui-mcp-v2-") as temporary:
        directory = Path(temporary)
        source = directory / "interop fixture.txt"
        original_bytes = b"MCP v2 smoke fixture; must remain unchanged.\n"
        source.write_bytes(original_bytes)
        params = StdioServerParameters(
            command=str(binary),
            args=["--demo", "local-files", "--directory", str(directory), "--mcp-stdio"],
        )

        async with Client(params) as client:
            require(client.protocol_version == "2026-07-28", f"unexpected protocol: {client.protocol_version}")
            tools = await client.list_tools()
            require(len(tools.tools) == 18, f"expected 18 stable MCP tools, got {len(tools.tools)}")
            required_tools = {"actions_list", "actions_describe", "action_invoke"}
            require(required_tools <= {tool.name for tool in tools.tools}, "core action tools missing")
            by_name = {tool.name: tool for tool in tools.tools}
            invoke_schema = by_name["action_invoke"].input_schema
            require("requestId" in invoke_schema.get("required", []), "action_invoke lost its retry ID contract")
            for tool_name, stable_fields in {
                "app_describe": {"protocolVersion", "runtime", "capabilities", "scriptEvaluation"},
                "page_observe": {"documentEpoch", "items", "truncated", "cursorConsistency"},
                "page_changes": {"documentEpoch", "cursor", "records", "resyncRequired"},
            }.items():
                output_schema = by_name[tool_name].output_schema
                require(isinstance(output_schema, dict), f"{tool_name} is missing its output schema")
                properties = output_schema.get("properties", {})
                require(stable_fields <= set(properties), f"{tool_name} schema is missing {stable_fields - set(properties)}")

            discovered = await client.call_tool("actions_list", arguments={"prefix": "local_files."})
            require(not discovered.is_error, f"action discovery failed: {discovered.structured_content}")
            actions = discovered.structured_content["items"]
            action_ids = {action["id"] for action in actions}
            require(
                action_ids == {
                    "local_files.query",
                    "local_files.refresh",
                    "local_files.metadata.update",
                },
                f"unexpected local-files actions: {action_ids}",
            )

            queried = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.query",
                    "args": {"query": "interop fixture", "limit": 10, "offset": 0},
                    "requestId": "mcp-v2-query-1",
                },
            )
            files = action_result(queried)["items"]
            require(len(files) == 1, f"expected one matching file, got {files}")
            item = files[0]
            require(item["name"] == source.name, f"wrong indexed name: {item['name']!r}")
            require(item["size"] == len(original_bytes), f"wrong indexed size: {item['size']}")
            initial_version = item["version"]

            saved = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.metadata.update",
                    "args": {
                        "fileId": item["id"],
                        "note": "updated via MCP Python SDK 2",
                        "expectedFileVersion": initial_version,
                    },
                    "requestId": "mcp-v2-save-1",
                },
            )
            saved_item = action_result(saved)
            require(saved_item["metadata"]["note"] == "updated via MCP Python SDK 2", "metadata update not returned")
            require(saved_item["version"] > initial_version, "successful update did not advance object version")

            retry = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.metadata.update",
                    "args": {
                        "fileId": item["id"],
                        "note": "updated via MCP Python SDK 2",
                        "expectedFileVersion": initial_version,
                    },
                    "requestId": "mcp-v2-save-1",
                },
            )
            require(action_result(retry) == saved_item, "same requestId retry did not replay the committed result")

            reused = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.metadata.update",
                    "args": {
                        "fileId": item["id"],
                        "note": "different payload must be rejected",
                        "expectedFileVersion": initial_version,
                    },
                    "requestId": "mcp-v2-save-1",
                },
            )
            require(reused.is_error, "same requestId with a different payload unexpectedly succeeded")
            require(
                reused.structured_content.get("code") == "request_id_conflict",
                f"requestId payload reuse returned the wrong error: {reused.structured_content}",
            )

            stale = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.metadata.update",
                    "args": {
                        "fileId": item["id"],
                        "note": "stale write must fail",
                        "expectedFileVersion": initial_version,
                    },
                    "requestId": "mcp-v2-stale-1",
                },
            )
            require(stale.is_error, "stale object version unexpectedly succeeded")

            verified = await client.call_tool(
                "action_invoke",
                arguments={
                    "action": "local_files.query",
                    "args": {"query": "interop fixture", "limit": 10, "offset": 0},
                    "requestId": "mcp-v2-query-2",
                },
            )
            final_items = action_result(verified)["items"]
            require(final_items[0]["metadata"]["note"] == "updated via MCP Python SDK 2", "stale write changed metadata")
            require(source.read_bytes() == original_bytes, "metadata action modified the source file")
            require(sorted(path.name for path in directory.iterdir()) == [source.name], "smoke created extra filesystem entries")

            print(
                json.dumps(
                    {
                        "sdk": sdk_version,
                        "protocol": client.protocol_version,
                        "server": client.server_info.name if client.server_info else None,
                        "tools": len(tools.tools),
                        "local_files_actions": sorted(action_ids),
                        "checks": [
                            "SDK v2 protocol negotiation",
                            "action discovery",
                            "top-level index query",
                            "metadata update",
                            "same requestId retry deduplicated",
                            "same requestId with a different payload rejected",
                            "stale version rejected",
                            "source file unchanged",
                        ],
                    },
                    ensure_ascii=False,
                )
            )


if __name__ == "__main__":
    default_binary = Path(__file__).resolve().parents[1] / "target" / "release" / "lapui.exe"
    binary_path = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else default_binary
    asyncio.run(run(binary_path))
