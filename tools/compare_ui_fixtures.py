"""Compare local UI fixtures in Lapui and a real headless Chromium browser.

Build: cargo build --locked --example fixture_runner
Run: python tools/compare_ui_fixtures.py --browser <chrome-or-edge-executable>
Requires Python 3.11+ and Node.js 22+, using only their standard libraries.
Generated reports, browser profiles and PNGs stay in target/ by default.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "examples/ui-fixtures"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def png_size(path):
    header = path.read_bytes()[:24]
    if header[:8] != b"\x89PNG\r\n\x1a\n" or len(header) != 24:
        raise ValueError(f"invalid PNG: {path}")
    width, height = struct.unpack(">II", header[16:24])
    return {"width": width, "height": height}


def compare(native, reference, viewport, tolerance):
    failures = []
    for engine, report in (("lapui", native), ("browser", reference)):
        if report.get("status") != "ready" or report.get("viewport") != viewport:
            failures.append({"field": engine + ".readiness", "actual": report})
        checks = report.get("checks")
        if not checks or any(value is not True for value in checks.values()):
            failures.append({"field": engine + ".checks", "actual": checks})
    for field in ("scenario", "states"):
        if native.get(field) != reference.get(field):
            failures.append({"field": field, "lapui": native.get(field), "browser": reference.get(field)})
    native_metrics, reference_metrics = native.get("metrics", {}), reference.get("metrics", {})
    if not native_metrics or native_metrics.keys() != reference_metrics.keys():
        failures.append({"field": "metricIds", "lapui": list(native_metrics), "browser": list(reference_metrics)})
    for node, expected in reference_metrics.items():
        actual = native_metrics.get(node, {})
        for field, value in expected.items():
            observed = actual.get(field)
            if not isinstance(observed, (int, float)) or not math.isfinite(observed) or abs(observed - value) > tolerance:
                failures.append({"field": f"metrics.{node}.{field}", "lapui": observed, "browser": value})
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser", required=True)
    parser.add_argument("--browser-version", help="Optional expected browser version, checked against Browser.getVersion")
    parser.add_argument("--node", default="node")
    parser.add_argument("--suite", choices=("basic", "frameworks", "all"), default="basic")
    parser.add_argument("--lapui-runner", default=str(ROOT / "target/debug/examples" / ("fixture_runner.exe" if os.name == "nt" else "fixture_runner")))
    parser.add_argument("--out")
    arguments = parser.parse_args()
    browser, runner = Path(arguments.browser).resolve(), Path(arguments.lapui_runner).resolve()
    if not browser.is_file() or not runner.is_file():
        parser.error("browser and native fixture runner must exist; build the example first")
    default_output = {"basic":"fixture-comparison", "frameworks":"framework-comparison", "all":"fixture-comparison-all"}[arguments.suite]
    output = Path(arguments.out).resolve() if arguments.out else ROOT / "target" / default_output
    output.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((FIXTURE / "manifest.json").read_text(encoding="utf-8"))
    scenarios = list(manifest["scenarios"]) if arguments.suite != "frameworks" else []
    framework_inputs = {}
    if arguments.suite != "basic":
        scenarios += [item["name"] for item in manifest["frameworkScenarios"]]
        for item in manifest["frameworkScenarios"]:
            page = ROOT / item["page"]
            for source in (page, page.parent / "dist/app.js", page.parent / "style.css", page.parent / "package-lock.json"):
                if source.is_file():
                    framework_inputs[source.relative_to(ROOT).as_posix()] = digest(source)
    for viewport in manifest["viewports"]:
        width, height = viewport["width"], viewport["height"]
        native = output / f"{width}x{height}" / "lapui"
        subprocess.run([str(runner), "--out", str(native), "--width", str(width), "--height", str(height), "--suite", arguments.suite], check=True, timeout=45)
    subprocess.run([
        arguments.node, str(ROOT / "tools/capture_browser_fixtures.mjs"),
        "--browser", str(browser), "--out", str(output), "--suite", arguments.suite,
    ], check=True, timeout=90)
    browser_metadata = json.loads((output / "browser-metadata.json").read_text(encoding="utf-8"))
    if arguments.browser_version and arguments.browser_version not in browser_metadata["product"]:
        raise RuntimeError("browser version differs from the requested version")
    reports = []
    for viewport in manifest["viewports"]:
        width, height = viewport["width"], viewport["height"]
        directory = output / f"{width}x{height}"
        for name in scenarios:
            native = json.loads((directory / "lapui" / f"{name}.json").read_text(encoding="utf-8"))
            reference = json.loads((directory / f"{name}-browser.json").read_text(encoding="utf-8"))
            failures = compare(native, reference, viewport, manifest["geometryToleranceCssPx"])
            for engine, image in (("lapui", directory / "lapui" / f"{name}.png"), ("browser", directory / f"{name}-browser.png")):
                size = png_size(image)
                if size != viewport:
                    failures.append({"field": engine + ".pngDimensions", "actual": size, "expected": viewport})
            behavior_checks = all(report.get("checks") and all(value is True for value in report["checks"].values()) for report in (native, reference))
            reports.append({"scenario": name, "viewport": viewport, "behaviorChecksPassed": behavior_checks,
                            "stateComparisonPassed": native.get("states") == reference.get("states"),
                            "geometryComparisonPassed": not any(item["field"].startswith("metrics.") or item["field"] == "metricIds" for item in failures),
                            "layoutViewports": {"lapui":native.get("layoutViewport"), "browser":reference.get("layoutViewport")},
                            "failures": failures})
            print(f"{width}x{height} {name}: {'FAIL' if failures else 'PASS'} ({len(failures)} differences)")
    summary = {
        "recordedUtc": datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC"),
        "platform": platform.platform(), "pythonVersion": platform.python_version(), "browser": browser_metadata,
        "browserExecutableSha256": digest(browser), "lapuiRunnerSha256": digest(runner),
        "cargoLockSha256": digest(ROOT / "Cargo.lock"),
        "suite": arguments.suite, "frameworkInputsSha256": framework_inputs,
        "fixtureSha256": {name: digest(FIXTURE / name) for name in ("index.html", "app.js", "frameworks.js", "manifest.json")},
        "driverSha256": {name: digest(ROOT / name) for name in ("tools/compare_ui_fixtures.py", "tools/capture_browser_fixtures.mjs", "examples/fixture_runner.rs")},
        "geometryToleranceCssPx": manifest["geometryToleranceCssPx"], "pngDimensionsChecked": True,
        "pixelEqualityChecked": False, "physicalInputChecked": False,
        "cases": reports, "passed": all(not report["failures"] for report in reports),
    }
    (output / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    raise SystemExit(0 if summary["passed"] else 1)


if __name__ == "__main__":
    main()
