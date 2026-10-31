"""Build a local Windows x64 preview, notices and a matching source companion.

Requires Python 3.11+, the native Rust MSVC toolchain and an explicitly selected
Microsoft Redist DLL/terms file. This never publishes artifacts or pushes Git.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
OS_DLLS = set("bcrypt.dll bcryptprimitives.dll kernel32.dll kernelbase.dll ws2_32.dll crypt32.dll secur32.dll gdi32.dll user32.dll ntdll.dll oleaut32.dll shell32.dll dwrite.dll opengl32.dll setupapi.dll combase.dll dwmapi.dll imm32.dll ole32.dll uxtheme.dll advapi32.dll shlwapi.dll version.dll winmm.dll hid.dll d3d12.dll dxgi.dll d3dcompiler_47.dll usp10.dll".split())
VC_DLLS = {"vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll", "concrt140.dll"}
TERMS_URL = "https://visualstudio.microsoft.com/license-terms/vs2026-ga-visualcpp-v14-redist-runtime/"
DEPLOYMENT_URL = "https://learn.microsoft.com/en-us/cpp/windows/choosing-a-deployment-method?view=msvc-170"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def json_file(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def command(arguments):
    return subprocess.run(arguments, cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8").stdout.strip()


def pe_info(path):
    """Read PE32+ architecture and static/delay imports, without loading code."""
    data = path.read_bytes()
    def u32(offset):
        return struct.unpack_from("<I", data, offset)[0]
    pe = u32(0x3C)
    if data[:2] != b"MZ" or data[pe:pe + 4] != b"PE\0\0":
        raise ValueError(f"not a PE image: {path.name}")
    machine, section_count = struct.unpack_from("<HH", data, pe + 4)
    optional, optional_size = pe + 24, struct.unpack_from("<H", data, pe + 20)[0]
    if machine != 0x8664 or struct.unpack_from("<H", data, optional)[0] != 0x20B:
        raise ValueError(f"expected Windows x64 PE32+: {path.name}")
    sections = []
    for index in range(section_count):
        offset = optional + optional_size + index * 40
        virtual_size, address, raw_size, raw = struct.unpack_from("<IIII", data, offset + 8)
        sections.append((address, max(virtual_size, raw_size), raw))
    def position(rva):
        if rva < u32(optional + 60):
            return rva
        for address, size, raw in sections:
            if address <= rva < address + size and raw + rva - address < len(data):
                return raw + rva - address
        raise ValueError(f"unmapped PE address in {path.name}")
    def text(rva):
        offset = position(rva)
        end = data.find(b"\0", offset, offset + 512)
        if end < 0:
            raise ValueError("unterminated import name")
        return data[offset:end].decode("ascii").lower()
    imports, delayed = set(), set()
    directory = optional + 112
    for directory_index, record_size, names in ((1, 20, imports), (13, 32, delayed)):
        rva, size = struct.unpack_from("<II", data, directory + directory_index * 8)
        if not rva:
            continue
        start = position(rva)
        for index in range(min(size // record_size + 1, 4096)):
            offset = start + index * record_size
            record = data[offset:offset + record_size]
            if not any(record):
                break
            name_rva = u32(offset + (12 if directory_index == 1 else 4))
            if directory_index == 13 and not u32(offset) & 1:
                name_rva -= struct.unpack_from("<Q", data, optional + 24)[0]
            names.add(text(name_rva))
        else:
            raise ValueError("unterminated import directory")
    return {"machine":"AMD64", "imports":sorted(imports), "delayImports":sorted(delayed)}


def registry_packages(metadata):
    packages = {package["id"]:package for package in metadata["packages"]}
    nodes = {node["id"]:node for node in metadata["resolve"]["nodes"]}
    pending, seen = [metadata["resolve"]["root"]], set()
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        pending.extend(edge["pkg"] for edge in nodes[current]["deps"]
                       if any(kind["kind"] in (None, "build") for kind in edge["dep_kinds"]))
    result = [packages[item] for item in seen if item != metadata["resolve"]["root"]]
    if any(not str(package["source"]).startswith("registry+") for package in result):
        raise ValueError("source collector currently requires registry dependencies")
    return sorted(result, key=lambda package:(package["name"], package["version"]))


def asset_files(policy):
    files = {ROOT / relative for relative in policy["rootFiles"]}
    files.update(ROOT.glob(policy["guides"]))
    for example in policy["examples"]:
        directory = ROOT / "examples" / example
        for pattern in policy["examplePatterns"]:
            files.update(directory.glob(pattern))
    return sorted(path for path in files if path.is_file())


def safe_source(path):
    relative = path.relative_to(ROOT)
    if any(part.casefold() in {"docs", "target", "book", "node_modules", ".git", ".codex", ".agents", ".ssh", "__pycache__"} for part in relative.parts):
        raise ValueError(f"excluded source path: {relative}")
    if path.is_symlink() or not path.resolve().is_relative_to(ROOT):
        raise ValueError(f"source escapes project: {relative}")
    if relative.name.startswith(".env") or path.suffix.lower() in {".pem", ".p12", ".pfx", ".key", ".log", ".pyc"}:
        raise ValueError(f"private/generated source path: {relative}")
    return relative.as_posix()


def registry_archive(package, checksum):
    directory = Path(package["manifest_path"]).parent
    archive = directory.parents[2] / "cache" / directory.parent.name / f"{package['name']}-{package['version']}.crate"
    if not archive.is_file() or digest(archive) != checksum:
        raise ValueError(f"missing or mismatched locked archive: {archive.name}")
    # Check the files actually compiled, not just the cached download checksum.
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            if not member.isfile():
                continue
            relative = PurePosixPath(member.name)
            if relative.is_absolute() or ".." in relative.parts or relative.parts[0] != directory.name:
                raise ValueError(f"unsafe crate member: {member.name}")
            local = directory.joinpath(*relative.parts[1:])
            if not local.resolve().is_relative_to(directory.resolve()) or not local.is_file():
                raise ValueError(f"missing/untrusted unpacked source: {package['name']}/{relative}")
            with source.extractfile(member) as stream:
                expected = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest(local) != expected:
                raise ValueError(f"modified registry source: {package['name']}/{relative}")
    return archive, directory


def runtime_information(path):
    if path.name.lower() not in VC_DLLS or "redist" not in [part.lower() for part in path.parts] or "debug" in str(path).lower():
        raise ValueError("select a release VC runtime DLL from the Visual Studio Redist directory")
    environment = dict(os.environ, LAPUI_PREVIEW_RUNTIME=str(path))
    powershell_root = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0"
    environment["PSMODULEPATH"] = str(powershell_root / "Modules")
    script = "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $p=Get-Item -LiteralPath $env:LAPUI_PREVIEW_RUNTIME; $s=Get-AuthenticodeSignature -LiteralPath $p.FullName; [ordered]@{version=$p.VersionInfo.FileVersion;copyright=$p.VersionInfo.LegalCopyright;signature=$s.Status.ToString()} | ConvertTo-Json -Compress"
    result = subprocess.run([str(powershell_root / "powershell.exe"), "-NoProfile", "-NonInteractive", "-Command", script], capture_output=True, text=True, encoding="utf-8", env=environment, timeout=30)
    if result.returncode:
        raise RuntimeError("runtime metadata query failed: " + result.stderr)
    information = json.loads(result.stdout)
    if information["signature"] != "Valid":
        raise ValueError("runtime signature is not valid")
    return {**information, "sha256":digest(path), "origin":"Visual Studio VC/Redist/x64/" + path.parent.name, **pe_info(path)}


def build_executable():
    print("Building the native release binary with locked default features...", flush=True)
    process = subprocess.Popen(["cargo", "build", "--locked", "--release", "--bin", "lapui", "--message-format=json"], cwd=ROOT, stdout=subprocess.PIPE, text=True, encoding="utf-8")
    executable = None
    for line in process.stdout:
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item["target"]["name"] == "lapui" and item.get("executable"):
            executable = Path(item["executable"])
        if item.get("reason") == "compiler-message" and item["message"]["level"] == "error":
            print(item["message"].get("rendered", "build error"), flush=True)
    if process.wait() or executable is None:
        raise RuntimeError("release build did not produce lapui")
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default=str(ROOT / "target/packages"))
    parser.add_argument("--vc-runtime", action="append", required=True)
    parser.add_argument("--vc-license", required=True, help="Saved upstream Microsoft runtime terms file")
    parser.add_argument("--allow-dirty", action="store_true", help="Development snapshots only; records dirty state")
    parser.add_argument("--skip-build", action="store_true", help="Development assembly only; not fresh-build evidence")
    parser.add_argument("--exe", help="Existing binary, only with --skip-build")
    arguments = parser.parse_args()
    if os.name != "nt" or f"host: {TARGET}" not in command(["rustc", "-vV"]):
        parser.error("the packager currently requires the native Windows x64 MSVC toolchain")
    if arguments.exe and not arguments.skip_build:
        parser.error("--exe requires --skip-build")
    state = command(["git", "status", "--porcelain", "--untracked-files=normal"])
    if state and not arguments.allow_dirty:
        parser.error("source tree is dirty; commit reviewed files or explicitly use --allow-dirty for development")
    revision = command(["git", "rev-parse", "HEAD"])
    policy = json.loads((ROOT / "tools/preview-assets.json").read_text(encoding="utf-8"))
    assets = asset_files(policy)
    templates = sorted(path for path in (ROOT / policy["templates"]).iterdir() if path.is_file())
    tracked = [ROOT / relative for relative in command(["git", "ls-files"]).splitlines()]
    sources = sorted(set(tracked + assets + templates + [ROOT / relative for relative in policy["developmentSourceFiles"]]))
    for source in sources:
        safe_source(source)
        if not source.is_file():
            raise ValueError(f"source file is missing: {source.relative_to(ROOT)}")
    before = {safe_source(path):digest(path) for path in sources}
    metadata = json.loads(command(["cargo", "metadata", "--locked", "--offline", "--filter-platform", TARGET, "--format-version", "1"]))
    package = next(item for item in metadata["packages"] if item["id"] == metadata["resolve"]["root"])
    name = f"lapui-{package['version']}-dev-{revision[:7]}-windows-x64"
    output = Path(arguments.out).resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any((output / item).exists() for item in (name, name + ".zip", name + "-sources.zip", name + "-SHA256SUMS.txt")):
        raise FileExistsError("preview already exists; select a new --out directory")
    if arguments.skip_build:
        executable = Path(arguments.exe).resolve() if arguments.exe else Path(metadata["target_directory"]) / "release/lapui.exe"
    else:
        executable = build_executable()
    if before != {safe_source(path):digest(path) for path in sources}:
        raise RuntimeError("project sources changed during the build")
    terms = Path(arguments.vc_license).resolve()
    if not terms.is_file():
        raise ValueError("runtime terms file is missing")
    runtimes = {Path(item).name.lower():Path(item).resolve() for item in arguments.vc_runtime}
    runtime_info = {name:runtime_information(path) for name, path in runtimes.items()}
    images = {"lapui.exe":pe_info(executable), **{name:pe_info(path) for name, path in runtimes.items()}}
    for image in images.values():
        unresolved = [dll for dll in image["imports"] + image["delayImports"] if dll not in runtimes and dll not in OS_DLLS and not dll.startswith(("api-ms-win-", "ext-ms-win-"))]
        if unresolved:
            raise ValueError(f"unpackaged non-system DLL imports: {unresolved}")
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    checksums = {(item["name"],item["version"],item.get("source")):item.get("checksum") for item in lock["package"]}
    dependencies = registry_packages(metadata)
    print(f"Verifying archives, unpacked sources and notices for {len(dependencies)} registry packages...", flush=True)
    with tempfile.TemporaryDirectory(prefix="lapui-assembly-", dir=output) as temporary:
        work = Path(temporary)
        directory = work / name
        directory.mkdir()
        for source in assets:
            destination = directory / safe_source(source)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        for source in templates:
            shutil.copyfile(source, directory / source.name)
        shutil.copyfile(executable, directory / "lapui.exe")
        for dll, source in runtimes.items():
            shutil.copyfile(source, directory / dll)
        third_party = directory / "third-party"
        microsoft = third_party / "microsoft"
        microsoft.mkdir(parents=True)
        shutil.copyfile(terms, microsoft / ("runtime-terms" + terms.suffix))
        json_file(microsoft / "provenance.json", {"dlls":runtime_info, "termsUrl":TERMS_URL, "termsSha256":digest(terms), "deploymentGuidance":DEPLOYMENT_URL})
        source_name = name + "-sources.zip"
        inventory = []
        with zipfile.ZipFile(work / source_name, "w", compression=zipfile.ZIP_DEFLATED) as source_zip:
            for source in sources:
                source_zip.write(source, "project/" + safe_source(source))
            for index, dependency in enumerate(dependencies, 1):
                checksum = checksums[(dependency["name"],dependency["version"],dependency["source"])]
                archive, local = registry_archive(dependency, checksum)
                source_zip.write(archive, "crates/" + archive.name, compress_type=zipfile.ZIP_STORED)
                notices = []
                for notice in local.rglob("*"):
                    if not notice.is_file() or not re.match(r"^(license|licence|copying|copyright|notice|authors|ofl)([._-]|$)", notice.name, re.IGNORECASE):
                        continue
                    relative = notice.relative_to(local)
                    if notice.is_symlink() or not notice.resolve().is_relative_to(local.resolve()):
                        raise ValueError("notice source escapes its package")
                    destination = third_party / "licenses" / archive.stem / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(notice, destination)
                    notices.append(destination.relative_to(directory).as_posix())
                inventory.append({"name":dependency["name"], "version":dependency["version"], "licenseExpression":dependency["license"], "repository":dependency.get("repository"), "sourceArchive":"crates/" + archive.name, "sourceArchiveSha256":checksum, "noticeFiles":sorted(notices)})
                if index % 50 == 0:
                    print(f"Verified {index}/{len(dependencies)} packages", flush=True)
            source_zip.writestr("source-inventory.json", json.dumps({"target":TARGET, "revision":revision, "projectFileSha256":before, "packages":inventory}, ensure_ascii=False, indent=2))
        json_file(third_party / "inventory.json", {"scope":"selected target reachable normal/build graph; not only linked code", "target":TARGET, "packages":inventory})
        (third_party / "SOURCE-AVAILABILITY.md").write_text(
            "# Source availability\n\nThe matching source companion is `" + source_name + "`. It contains the public project and exact upstream Cargo package archives, including MPL-covered sources and their original notices. Unpack .crate files as tar.gz archives to inspect or modify them. Original component licenses apply; Lapui's MIT/Apache choice does not relicense these sources. Registry archive hashes match Cargo.lock and their files were compared with the selected local registry sources. Build dependencies are included conservatively. npm rebuilds still use their lockfiles; this is not a turnkey offline Cargo/npm vendor configuration.\n\nMicrosoft runtime binaries have separate supplied terms under microsoft/; they are not open-source Cargo components. This is a local preview assembly, not a claim of final public-release license review.\n",
            encoding="utf-8",
        )
        files = {path.relative_to(directory).as_posix():{"bytes":path.stat().st_size,"sha256":digest(path)} for path in sorted(directory.rglob("*")) if path.is_file()}
        if before != {safe_source(path):digest(path) for path in sources}:
            raise RuntimeError("project sources changed during assembly")
        manifest = {"schemaVersion":1, "status":"unfinished_local_preview", "recordedUtc":datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC"), "target":TARGET, "version":package["version"], "sourceRevision":revision, "dirtySource":bool(state), "buildPerformed":not arguments.skip_build, "rustc":command(["rustc", "--version"]), "cargo":command(["cargo", "--version"]), "cargoLockSha256":digest(ROOT / "Cargo.lock"), "features":"default", "dependencyImages":images, "sourceBundle":{"file":source_name,"bytes":(work / source_name).stat().st_size,"sha256":digest(work / source_name)}, "runtimeUncompressedBytes":sum(item["bytes"] for item in files.values()), "registryPackageCount":len(dependencies), "registryArchivesAndLocalSourcesVerified":True, "cleanMachineTested":False, "publicReleaseApproved":False, "files":files}
        json_file(directory / "preview-manifest.json", manifest)
        while manifest["runtimeUncompressedBytes"] != sum(item["bytes"] for item in files.values()) + (directory / "preview-manifest.json").stat().st_size:
            manifest["runtimeUncompressedBytes"] = sum(item["bytes"] for item in files.values()) + (directory / "preview-manifest.json").stat().st_size
            json_file(directory / "preview-manifest.json", manifest)
        runtime_zip = work / (name + ".zip")
        with zipfile.ZipFile(runtime_zip, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for source in sorted(directory.rglob("*")):
                if source.is_file():
                    archive.write(source, source.relative_to(work).as_posix())
        for archive in (runtime_zip, work / source_name):
            with zipfile.ZipFile(archive) as check:
                if check.testzip() is not None:
                    raise RuntimeError("archive integrity failed")
        sums = "".join(digest(path) + "  " + path.name + "\n" for path in (runtime_zip, work / source_name))
        (work / (name + "-SHA256SUMS.txt")).write_text(sums, encoding="ascii")
        shutil.move(str(directory), str(output / name))
        for source in (runtime_zip, work / source_name, work / (name + "-SHA256SUMS.txt")):
            shutil.move(str(source), str(output / source.name))
    print(json.dumps({"directory":str(output / name), "runtimeZip":str(output / (name + ".zip")), "sourceZip":str(output / source_name), "registryPackages":len(dependencies), "runtimeUncompressedBytes":manifest["runtimeUncompressedBytes"]}))


if __name__ == "__main__":
    main()
