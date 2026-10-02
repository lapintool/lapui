"""Open maintained offline UI examples and check semantic readiness without trace waits."""

from __future__ import annotations

import argparse
import asyncio
import base64
import json
from dataclasses import dataclass
from pathlib import Path

from mcp import Client, StdioServerParameters


@dataclass(frozen=True)
class Fixture:
    name: str
    minimum_controls: int
    ready_control: str
    ready_role: str


FIXTURES = (
    Fixture("form-submit-demo", 16, "submit", "button"),
    Fixture("changes-demo", 8, "increment", "button"),
    Fixture("scroll-demo", 20, "end", "button"),
    Fixture("list-stress-demo", 3, "clear", "button"),
    Fixture("forms-demo", 16, "first-contact-email", "option"),
    Fixture("floating-demo", 12, "anchor", "button"),
    Fixture("animation-demo", 9, "start", "button"),
    Fixture("modules-demo", 3, "module-increment", "button"),
    Fixture("vue-demo", 9, "add-task", "button"),
    Fixture("react-demo", 8, "react-add", "button"),
)


async def check_fixture(binary: Path, examples: Path, artifacts: Path, fixture: Fixture) -> dict:
    page = examples / fixture.name / "index.html"
    if not page.is_file():
        raise FileNotFoundError(f"fixture page not found: {page}")
    params = StdioServerParameters(
        command=str(binary),
        args=["--html", str(page), "--mcp-stdio"],
    )

    async with Client(params) as client:
        snapshot = await client.call_tool("page_controls")
        if snapshot.is_error or not isinstance(snapshot.structured_content, dict):
            raise AssertionError(f"{fixture.name}: page_controls failed: {snapshot.structured_content}")
        controls = snapshot.structured_content.get("controls", [])
        epoch = snapshot.structured_content.get("documentEpoch")
        if len(controls) < fixture.minimum_controls:
            raise AssertionError(
                f"{fixture.name}: expected at least {fixture.minimum_controls} controls, got {len(controls)}"
            )
        match = next(
            (
                control
                for control in controls
                if control.get("id") == fixture.ready_control and control.get("role") == fixture.ready_role
            ),
            None,
        )
        if match is None:
            raise AssertionError(f"{fixture.name}: ready control {fixture.ready_control!r} is missing")

        ready = await client.call_tool(
            "page_wait_for_control",
            arguments={
                "documentEpoch": epoch,
                "id": fixture.ready_control,
                "field": "role",
                "equals": fixture.ready_role,
                "waitId": f"fixture-ready-{fixture.name}",
                "timeoutMs": 3000,
            },
        )
        if ready.is_error or ready.structured_content.get("status") != "matched":
            raise AssertionError(f"{fixture.name}: readiness wait failed: {ready.structured_content}")

        if fixture.name == "changes-demo":
            activated = await client.call_tool(
                "page_control",
                arguments={
                    "operation": "activate",
                    "controlRef": match["ref"],
                    "documentEpoch": epoch,
                },
            )
            if activated.is_error:
                raise AssertionError(f"{fixture.name}: activation failed: {activated.structured_content}")
            updated = await client.call_tool(
                "page_wait_for_control",
                arguments={
                    "documentEpoch": epoch,
                    "id": "counter",
                    "field": "name",
                    "equals": "Count: 1",
                    "waitId": "fixture-counter-updated",
                    "timeoutMs": 3000,
                },
            )
            if updated.is_error or updated.structured_content.get("status") != "matched":
                raise AssertionError(f"{fixture.name}: asynchronous state wait failed: {updated.structured_content}")

        if fixture.name == "list-stress-demo":
            initial = await client.call_tool(
                "page_wait_for_control",
                arguments={
                    "documentEpoch": epoch,
                    "id": "result",
                    "field": "name",
                    "contains": "1000 / 1000 entries · 100 rows rendered",
                    "waitId": "fixture-list-ready",
                    "timeoutMs": 3000,
                },
            )
            if initial.is_error or initial.structured_content.get("status") != "matched":
                raise AssertionError(f"list-stress-demo: initial 100-row window did not render: {initial.structured_content}")

            geometry = await client.call_tool("page_observe", arguments={"limit": 64})
            if geometry.is_error or not isinstance(geometry.structured_content, dict):
                raise AssertionError(f"list-stress-demo: geometry observation failed: {geometry.structured_content}")
            geometry_by_id = {
                item.get("id"): item.get("bounds")
                for item in geometry.structured_content.get("items", [])
                if isinstance(item, dict) and isinstance(item.get("id"), str)
            }
            viewport_bounds = geometry_by_id.get("viewport")
            rows_bounds = geometry_by_id.get("rows")
            first_row_bounds = geometry_by_id.get("entry-0001")
            if not all(isinstance(bounds, dict) for bounds in (viewport_bounds, rows_bounds, first_row_bounds)):
                raise AssertionError(f"list-stress-demo: fixed layout boxes are missing: {geometry_by_id}")
            if abs(viewport_bounds["height"] - 364) > 0.1:
                raise AssertionError(f"list-stress-demo: viewport height drifted: {viewport_bounds}")
            if abs(rows_bounds["height"] - 28_000) > 0.1:
                raise AssertionError(f"list-stress-demo: 1,000-row content height drifted: {rows_bounds}")
            if abs(first_row_bounds["height"] - 28) > 0.1:
                raise AssertionError(f"list-stress-demo: row height drifted: {first_row_bounds}")
            if abs(first_row_bounds["y"] - (viewport_bounds["y"] + 2)) > 0.1:
                raise AssertionError(
                    f"list-stress-demo: first row is not aligned to viewport content: {viewport_bounds}, {first_row_bounds}"
                )

            search = next((control for control in controls if control.get("id") == "query"), None)
            if search is None:
                raise AssertionError("list-stress-demo: search control was not discovered")
            filtered = await client.call_tool(
                "page_control",
                arguments={
                    "operation": "fill",
                    "controlRef": search["ref"],
                    "documentEpoch": epoch,
                    "value": "Entry 0999",
                },
            )
            if filtered.is_error:
                raise AssertionError(f"list-stress-demo: search fill failed: {filtered.structured_content}")
            result = await client.call_tool(
                "page_wait_for_control",
                arguments={
                    "documentEpoch": epoch,
                    "id": "result",
                    "field": "name",
                    "contains": "1 / 1000 entries · 1 rows rendered",
                    "waitId": "fixture-list-filtered",
                    "timeoutMs": 3000,
                },
            )
            if result.is_error or result.structured_content.get("status") != "matched":
                raise AssertionError(f"list-stress-demo: 1,000-row filter did not settle: {result.structured_content}")

            filtered_geometry = await client.call_tool("page_observe", arguments={"limit": 64})
            filtered_items = filtered_geometry.structured_content.get("items", [])
            filtered_by_id = {
                item.get("id"): item.get("bounds")
                for item in filtered_items
                if isinstance(item, dict) and isinstance(item.get("id"), str)
            }
            filtered_viewport = filtered_by_id.get("viewport")
            filtered_rows = filtered_by_id.get("rows")
            filtered_row = filtered_by_id.get("entry-0999")
            if not all(isinstance(bounds, dict) for bounds in (filtered_viewport, filtered_rows, filtered_row)):
                raise AssertionError(f"list-stress-demo: filtered layout boxes are missing: {filtered_by_id}")
            if abs(filtered_rows["height"] - 360) > 0.1:
                raise AssertionError(f"list-stress-demo: filtered list did not shrink to its viewport: {filtered_rows}")
            if abs(filtered_row["y"] - (filtered_viewport["y"] + 2)) > 0.1:
                raise AssertionError(
                    f"list-stress-demo: filtered row is not aligned to viewport content: {filtered_viewport}, {filtered_row}"
                )

        if fixture.name == "react-demo":
            effect = await client.call_tool(
                "page_wait_for_control",
                arguments={
                    "documentEpoch": epoch,
                    "id": "react-status",
                    "field": "name",
                    "equals": "React effect completed",
                    "waitId": "fixture-react-effect",
                    "timeoutMs": 3000,
                },
            )
            if effect.is_error or effect.structured_content.get("status") != "matched":
                raise AssertionError(f"{fixture.name}: mount effect did not complete: {effect.structured_content}")

        if fixture.name == "modules-demo":
            loaded = await client.call_tool(
                "page_wait_for_control",
                arguments={
                    "documentEpoch": epoch,
                    "id": "module-status",
                    "field": "name",
                    "equals": "Local modules and top-level await are ready.",
                    "waitId": "fixture-modules-loaded",
                    "timeoutMs": 3000,
                },
            )
            if loaded.is_error or loaded.structured_content.get("status") != "matched":
                raise AssertionError(f"{fixture.name}: module graph did not finish: {loaded.structured_content}")

        diagnostics = await client.call_tool("page_diagnostics")
        if diagnostics.is_error or diagnostics.structured_content.get("errors"):
            raise AssertionError(f"{fixture.name}: runtime diagnostics: {diagnostics.structured_content}")

        screenshot = await client.call_tool("page_screenshot")
        if screenshot.is_error:
            raise AssertionError(f"{fixture.name}: screenshot failed: {screenshot.structured_content}")
        image = next((item for item in screenshot.content if item.type == "image"), None)
        if image is None or image.mime_type != "image/png":
            raise AssertionError(f"{fixture.name}: screenshot did not return a PNG image")
        artifacts.mkdir(parents=True, exist_ok=True)
        screenshot_path = artifacts / f"{fixture.name}.png"
        screenshot_path.write_bytes(base64.b64decode(image.data))

        return {
            "fixture": fixture.name,
            "documentEpoch": epoch,
            "controls": len(controls),
            "ready": ready.structured_content["status"],
            "diagnostics": "clean",
            "physicalPresentation": ready.structured_content.get("physicalPresentation"),
            "screenshot": str(screenshot_path),
        }


async def main(binary: Path, project: Path, artifacts: Path) -> None:
    if not binary.is_file():
        raise FileNotFoundError(f"Lapui binary not found: {binary}")
    examples = project / "examples"
    results = []
    for fixture in FIXTURES:
        results.append(await check_fixture(binary, examples, artifacts, fixture))
    print(json.dumps({"fixtures": results}, ensure_ascii=False))


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=root / "target" / "release" / "lapui.exe")
    parser.add_argument("--project", type=Path, default=root)
    parser.add_argument("--artifacts", type=Path, default=root / "target" / "m2-fixture-screenshots")
    arguments = parser.parse_args()
    asyncio.run(main(arguments.binary.resolve(), arguments.project.resolve(), arguments.artifacts.resolve()))
