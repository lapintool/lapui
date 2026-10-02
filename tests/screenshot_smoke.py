"""Exercise built-in live PNG capture through the CLI, without an MCP SDK.

Run: py -3 tests/screenshot_smoke.py --binary target/release/lapui.exe
Requires a working native window session and a software-renderer build.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import queue
import struct
import subprocess
import threading
import time


def main(binary: Path, artifacts: Path) -> None:
    project = Path(__file__).resolve().parents[1]
    artifacts.mkdir(parents=True, exist_ok=True)
    with (artifacts / "stderr.log").open("w", encoding="utf-8") as errors:
        app = subprocess.Popen(
            [str(binary), "--html", str(project / "examples/forms-demo/index.html")],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=errors,
            text=True, encoding="utf-8", cwd=project,
        )
        lines: queue.Queue[str] = queue.Queue()

        def read_output() -> None:
            assert app.stdout is not None
            for line in app.stdout:
                lines.put(line)

        threading.Thread(target=read_output, daemon=True).start()
        try:
            deadline = time.monotonic() + 20
            address = None
            while address is None and time.monotonic() < deadline:
                if app.poll() is not None:
                    raise RuntimeError(f"Lapui exited during startup: {app.returncode}")
                try:
                    line = lines.get(timeout=min(0.2, max(0.01, deadline - time.monotonic())))
                except queue.Empty:
                    continue
                if line.startswith("LAPUI_CONTROL="):
                    address = line.strip().split("=", 1)[1]
            if address is None:
                raise TimeoutError("Lapui did not publish a control address within 20s")

            def client(command: str, *args: str) -> dict:
                result = subprocess.run(
                    [str(binary), "client", address, command, *args],
                    check=True, capture_output=True, text=True, encoding="utf-8", timeout=25,
                )
                return json.loads(result.stdout)

            description = client("describe")["observation"]
            assert description["capabilities"]["screenshot"] is True
            assert "screenshot" in description["methods"]
            controls = client("controls")["observation"]
            assert controls["controls"], "forms fixture has no controls"
            image_path = artifacts / "current.png"
            metadata = client("screenshot", str(image_path))
            assert "pngBase64" not in metadata, "CLI should print compact metadata"
            assert metadata["documentEpoch"] == controls["documentEpoch"]
            assert metadata["boundary"] == "cpu_rendered"
            assert metadata["physicalPresentation"] == "not_confirmed"
            assert metadata["scaleFactor"] > 0
            png = image_path.read_bytes()
            assert png[:8] == b"\x89PNG\r\n\x1a\n" and png[12:16] == b"IHDR"
            assert len(png) <= 4 * 1024 * 1024
            assert struct.unpack(">II", png[16:24]) == (metadata["width"], metadata["height"])
            again = client("screenshot", str(artifacts / "second.png"))
            for field in ("documentEpoch", "width", "height", "scaleFactor"):
                assert again[field] == metadata[field], f"capture changed {field}"
            assert not client("diagnostics")["observation"]["errors"]
            report = {
                "status": "passed", "binary": str(binary),
                "binarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "capture": metadata, "pngBytes": len(png),
                "pngSha256": hashlib.sha256(png).hexdigest(),
                "checks": ["CLI discovery", "live PNG", "epoch and DPI metadata",
                           "repeated viewport preservation", "clean diagnostics"],
            }
            (artifacts / "report.json").write_text(
                json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8",
            )
            print(json.dumps(report, ensure_ascii=False))
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait(timeout=5)
            if app.stdout is not None:
                app.stdout.close()


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=root / "target/release/lapui.exe")
    parser.add_argument("--artifacts", type=Path, default=root / "target/screenshot-smoke")
    args = parser.parse_args()
    main(args.binary.resolve(), args.artifacts.resolve())
