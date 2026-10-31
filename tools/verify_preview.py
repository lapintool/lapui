"""Verify a relocated Windows preview with only Windows directories on PATH.

This tests an artifact on the current OS, not a clean machine or physical input.
No Rust, Node, Python SDK, package installation or repository resources are used
by the child application. Python is used only by this development verifier.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import queue
import struct
import subprocess
import tempfile
import threading
import time
import zipfile


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def png_dimensions(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("invalid PNG response")
    return struct.unpack(">II", data[16:24])


def safe_file(root, relative):
    parts = PurePosixPath(relative)
    result = root.joinpath(*parts.parts)
    if parts.is_absolute() or ".." in parts.parts or not result.resolve().is_relative_to(root.resolve()):
        raise ValueError(f"unsafe package path: {relative}")
    return result


def extract(archive, destination):
    with zipfile.ZipFile(archive) as source:
        names = set()
        for entry in source.infolist():
            safe_file(destination, entry.filename)
            if entry.filename.casefold() in names or (entry.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError("duplicate path or symlink in runtime ZIP")
            names.add(entry.filename.casefold())
        if source.testzip() is not None:
            raise ValueError("runtime ZIP CRC failure")
        source.extractall(destination)


class StdioProbe:
    def __init__(self, process):
        self.process = process
        self.messages = queue.Queue()
        self.sequence = 0
        self.errors = []
        def reader():
            try:
                for line in process.stdout:
                    self.messages.put(json.loads(line))
            except Exception as error:
                self.messages.put(error)
            self.messages.put(EOFError("MCP stdout ended"))
        def diagnostics():
            for line in process.stderr:
                self.errors.append(line.strip())
                self.errors = self.errors[-30:]
        self.threads = [threading.Thread(target=reader, daemon=True), threading.Thread(target=diagnostics, daemon=True)]
        for worker in self.threads:
            worker.start()

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method, parameters=None):
        self.sequence += 1
        expected = self.sequence
        self.send({"jsonrpc":"2.0", "id":expected, "method":method, "params":parameters or {}})
        deadline = time.monotonic() + 20
        while True:
            message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(message, Exception):
                raise RuntimeError(f"protocol ended: {message}; stderr={self.errors}")
            if "id" not in message:
                continue
            if message["id"] != expected or "error" in message:
                raise RuntimeError(f"unexpected MCP response: {message}")
            return message["result"]

    def tool(self, name):
        result = self.request("tools/call", {"name":name,"arguments":{}})
        if result.get("isError"):
            raise RuntimeError(f"packaged tool failed: {result}")
        return result


def verify(runtime_zip, report_path):
    if os.name != "nt":
        raise RuntimeError("the portable verification currently requires Windows")
    report_path.parent.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ)
    windows = Path(os.environ["SystemRoot"])
    environment["PATH"] = os.pathsep.join(str(path) for path in (windows / "System32", windows))
    for variable in ("CARGO_HOME", "CARGO_BUILD_TARGET", "RUSTUP_HOME", "RUSTFLAGS", "NODE_PATH", "PYTHONPATH"):
        environment.pop(variable, None)
    powershell = windows / "System32/WindowsPowerShell/v1.0/powershell.exe"
    environment["PSMODULEPATH"] = str(powershell.parent / "Modules")
    with tempfile.TemporaryDirectory(prefix="lapui preview relocation ") as temporary:
        root = Path(temporary)
        extract(runtime_zip, root)
        manifests = list(root.glob("*/preview-manifest.json"))
        if len(manifests) != 1:
            raise ValueError("expected one preview root")
        package = manifests[0].parent
        if list(root.iterdir()) != [package]:
            raise ValueError("unexpected files outside the preview root")
        manifest = json.loads(manifests[0].read_text(encoding="utf-8"))
        listed = set(manifest["files"]) | {"preview-manifest.json"}
        actual = {path.relative_to(package).as_posix() for path in package.rglob("*") if path.is_file()}
        if listed != actual:
            raise ValueError("package contents differ from the file manifest")
        for relative, record in manifest["files"].items():
            path = safe_file(package, relative)
            if path.stat().st_size != record["bytes"] or digest(path) != record["sha256"]:
                raise ValueError(f"artifact hash mismatch: {relative}")
        companion_name = manifest["sourceBundle"]["file"]
        if Path(companion_name).name != companion_name:
            raise ValueError("unsafe source companion name")
        companion = runtime_zip.parent / companion_name
        if digest(companion) != manifest["sourceBundle"]["sha256"]:
            raise ValueError("source companion checksum mismatch")
        with zipfile.ZipFile(companion) as sources:
            if sources.testzip() is not None:
                raise ValueError("source ZIP CRC failure")
            inventory = json.loads(sources.read("source-inventory.json"))
            for dependency in inventory["packages"]:
                if hashlib.sha256(sources.read(dependency["sourceArchive"])).hexdigest() != dependency["sourceArchiveSha256"]:
                    raise ValueError("dependency source checksum mismatch")
            for relative, checksum in inventory["projectFileSha256"].items():
                if hashlib.sha256(sources.read("project/" + relative)).hexdigest() != checksum:
                    raise ValueError("project source checksum mismatch")
        working = root / "unrelated working directory"
        working.mkdir()
        snapshot_results = {}
        for name, arguments in (
            ("files", ["--demo", "files"]),
            ("react", ["--html", str(package / "examples/react-demo/index.html")]),
            ("floating", ["--html", str(package / "examples/floating-demo/index.html")]),
        ):
            image = working / (name + ".png")
            completed = subprocess.run([str(package / "lapui.exe"), *arguments, "--snapshot", str(image), "--width", "960", "--height", "640"], cwd=working, env=environment, capture_output=True, text=True, encoding="utf-8", timeout=30)
            if completed.returncode or "Lapui script" in completed.stderr:
                raise RuntimeError(f"packaged {name} startup failed: {completed.stderr}")
            dimensions = png_dimensions(image.read_bytes())
            if dimensions != (960, 640):
                raise ValueError("snapshot dimensions changed")
            snapshot_results[name] = list(dimensions)
        image = working / "vue launcher.png"
        launcher_environment = dict(environment, LAPUI_PREVIEW_LAUNCHER=str(package / "run-vue.cmd"), LAPUI_PREVIEW_IMAGE=str(image))
        script = "& $env:LAPUI_PREVIEW_LAUNCHER --snapshot $env:LAPUI_PREVIEW_IMAGE --width 960 --height 640; exit $LASTEXITCODE"
        launched = subprocess.run([str(powershell), "-NoProfile", "-NonInteractive", "-Command", script], cwd=working, env=launcher_environment, capture_output=True, timeout=30)
        if launched.returncode or png_dimensions(image.read_bytes()) != (960, 640):
            raise RuntimeError(f"relocated Vue launcher failed: {launched.stderr!r}")
        snapshot_results["vue-launcher"] = [960, 640]
        process = subprocess.Popen([str(package / "lapui.exe"), "--demo", "files", "--mcp-stdio"], cwd=working, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
        try:
            probe = StdioProbe(process)
            initialized = probe.request("initialize", {"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"lapui-portable-probe","version":"1"}})
            probe.send({"jsonrpc":"2.0","method":"notifications/initialized"})
            tools = probe.request("tools/list")["tools"]
            description = probe.tool("app_describe")["structuredContent"]
            controls = probe.tool("page_controls")["structuredContent"]
            if not controls.get("controls"):
                raise RuntimeError("packaged UI exposed no controls")
            screenshot = probe.tool("page_screenshot")
            image_block = next(item for item in screenshot["content"] if item["type"] == "image")
            pixels = base64.b64decode(image_block["data"], validate=True)
            png_dimensions(pixels)
            report_path.with_suffix(".png").write_bytes(pixels)
            module_environment = dict(environment, LAPUI_PREVIEW_TEST_PID=str(process.pid))
            script = "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); @(Get-Process -Id ([int]$env:LAPUI_PREVIEW_TEST_PID) | Select-Object -ExpandProperty Modules | Where-Object {$_.ModuleName -ieq 'vcruntime140.dll'} | Select-Object -ExpandProperty FileName) | ConvertTo-Json -Compress"
            loaded = subprocess.run([str(powershell), "-NoProfile", "-NonInteractive", "-Command", script], check=True, capture_output=True, text=True, encoding="utf-8", env=module_environment, timeout=15)
            modules = json.loads(loaded.stdout)
            modules = [modules] if isinstance(modules, str) else modules
            if not modules or not all(Path(module).resolve() == (package / "vcruntime140.dll").resolve() for module in modules):
                raise RuntimeError("the test process did not load the packaged VC runtime")
            result = {"runtimeZipSha256":digest(runtime_zip), "sourceZipSha256":digest(companion), "sourceRevision":manifest["sourceRevision"], "dirtySource":manifest["dirtySource"], "buildPerformed":manifest["buildPerformed"], "fileHashesVerified":True, "sourceHashesVerified":True, "sourcePackages":len(inventory["packages"]), "relocatedWithSpaces":True, "restrictedPath":True, "appLocalVcRuntimeLoaded":True, "snapshots":snapshot_results, "mcpProtocol":initialized["protocolVersion"], "mcpTools":len(tools), "runtime":description.get("runtime"), "controlCount":len(controls["controls"]), "currentViewportPng":True, "cleanMachineTested":False, "physicalInputTested":False, "cleanup":"owned UI child terminated after probe; not graceful-window-close evidence"}
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            for worker in probe.threads:
                worker.join(timeout=2)
            for stream in (process.stdin, process.stdout, process.stderr):
                stream.close()
    report_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runtime_zip")
    parser.add_argument("--report", default="target/package-verification.json")
    arguments = parser.parse_args()
    verify(Path(arguments.runtime_zip).resolve(), Path(arguments.report).resolve())


if __name__ == "__main__":
    main()
