"""Regenerate the controlled CSS geometry reference using installed Edge.

Uses an isolated headless profile and local synthetic HTML only. The ordinary
Rust regression reads the saved JSON and does not require Edge or Python.
Run: py -3 tests/update_layout_reference.py --browser <msedge.exe>
"""

import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import subprocess
import tempfile


class Output(HTMLParser):
    def __init__(self):
        super().__init__()
        self.active = False
        self.parts = []

    def handle_starttag(self, tag, attrs):
        if tag == "pre" and dict(attrs).get("id") == "lapui-reference-output":
            self.active = True

    def handle_endtag(self, tag):
        if tag == "pre":
            self.active = False

    def handle_data(self, data):
        if self.active:
            self.parts.append(data)


def main(browser: Path, output: Path, scale: float) -> None:
    fixture = Path(__file__).parent / "fixtures/layout-reference.html"
    measure = fixture.with_suffix(".js").read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="lapui-layout-reference-") as temporary:
        workspace = Path(temporary)
        page = workspace / "reference.html"
        page.write_text(fixture.read_text(encoding="utf-8").replace(
            "</body>", '<pre id="lapui-reference-output"></pre><script>' + measure +
            "document.getElementById('lapui-reference-output').textContent = "
            "JSON.stringify({userAgent:navigator.userAgent,scaleFactor:devicePixelRatio,cases});</script></body>",
        ), encoding="utf-8")
        result = subprocess.run([
            str(browser), "--headless", "--no-first-run", "--disable-gpu",
            f"--force-device-scale-factor={scale}", "--window-size=900,800",
            f"--user-data-dir={workspace / 'profile'}", "--dump-dom", page.as_uri(),
        ], capture_output=True, text=True, encoding="utf-8", timeout=45, check=True)
        parser = Output()
        parser.feed(result.stdout)
        measured = json.loads("".join(parser.parts))
        assert len(measured["cases"]) == 2
        assert abs(measured["scaleFactor"] - scale) < 0.001
        measured["fixture"] = "tests/fixtures/layout-reference.html"
        measured["fixtureSha256"] = hashlib.sha256(fixture.read_bytes()).hexdigest()
        measured["measurementSha256"] = hashlib.sha256(measure.encode("utf-8")).hexdigest()
        measured["boundary"] = "browser CSS geometry; no native controls or text metrics"
        output.write_text(json.dumps(measured, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"output": str(output), "userAgent": measured["userAgent"],
                          "scaleFactor": measured["scaleFactor"],
                          "cases": len(measured["cases"])}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser", type=Path,
                        default=Path("C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"))
    parser.add_argument("--output", type=Path,
                        default=Path(__file__).parent / "fixtures/layout-reference.json")
    parser.add_argument("--scale", type=float, default=1.0)
    args = parser.parse_args()
    main(args.browser.resolve(), args.output.resolve(), args.scale)
