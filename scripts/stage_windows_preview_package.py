"""Stage a local Windows preview candidate without claiming it is distributable."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

from check_windows_preview_package import RELEASE_SOURCE, ROOT, check_package


DESTINATION = ROOT / "target" / f"windows-preview-package-{RELEASE_SOURCE[:7]}"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest().upper()


def copy_file(relative: str, destination: Path, package_path: str | None = None) -> None:
    source = ROOT / relative
    target = destination / (package_path or relative)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def stage_package(refresh: bool = False) -> dict:
    report = check_package()
    if report["missingResources"] or report["problems"]:
        raise RuntimeError(
            "Cannot stage an incomplete candidate: "
            f"missing={report['missingResources']}, problems={report['problems']}"
        )
    package_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True,
        check=True, timeout=10,
    ).stdout.strip()
    tracked_changes = bool(subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT,
        capture_output=True, text=True, check=True, timeout=10,
    ).stdout.strip())
    if DESTINATION.exists():
        if not refresh:
            raise FileExistsError(f"Refusing to overwrite existing staging directory: {DESTINATION}")
        manifest_path = DESTINATION / "PACKAGE-MANIFEST.json"
        if not manifest_path.is_file():
            raise RuntimeError("Refusing to refresh a directory without a package manifest")
        previous = json.loads(manifest_path.read_text(encoding="utf-8"))
        previous_files = {
            item["path"]
            for item in previous.get("files", [])
            if item["path"] != "PACKAGE-MANIFEST.json"
        }
        actual_files = {
            item.relative_to(DESTINATION).as_posix()
            for item in DESTINATION.rglob("*")
            if item.is_file() and item.name != "PACKAGE-MANIFEST.json"
        }
        if actual_files != previous_files:
            raise RuntimeError("Refusing to refresh a package with added or missing files")
        for item in previous["files"]:
            if item["path"] == "PACKAGE-MANIFEST.json":
                continue
            path = DESTINATION / item["path"]
            if path.stat().st_size != item["bytes"] or sha256(path) != item["sha256"]:
                raise RuntimeError(f"Refusing to refresh modified package file: {item['path']}")
        if previous.get("sourceCommit") != report["releaseCandidate"]["expectedSource"]:
            raise RuntimeError("Refusing to refresh a package from a different source commit")
    else:
        DESTINATION.mkdir(parents=True)

    for relative in (
        "README.md",
        "README.zh-CN.md",
        "THIRD_PARTY.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "prerequisites/README.md",
    ):
        copy_file(relative, DESTINATION)

    shutil.copytree(
        ROOT / "guide",
        DESTINATION / "guide",
        ignore=shutil.ignore_patterns("*.lock"),
        dirs_exist_ok=refresh,
    )
    shutil.copytree(
        ROOT / "licenses" / "third_party",
        DESTINATION / "licenses" / "third_party",
        dirs_exist_ok=refresh,
    )
    copy_file("target/release/lapui.exe", DESTINATION, "lapui.exe")
    copy_file("target/windows-release-sources.zip", DESTINATION, "sources/windows-release-sources.zip")

    status = {
        "packageStatus": "not-ready",
        "sourceCommit": report["releaseCandidate"]["expectedSource"],
        "packageCheckoutCommit": package_commit,
        "packageTrackedChanges": tracked_changes,
        "executableSha256": report["releaseCandidate"]["sha256"],
        "openGates": report["openGates"],
        "noticeReviewRequired": True,
        "cleanMachineAcceptance": "not-run",
        "runtimePrerequisite": "user-installed from Microsoft's documented x64 download",
    }
    (DESTINATION / "PACKAGE-STATUS.json").write_text(
        json.dumps(status, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    files = []
    for path in sorted(
        item
        for item in DESTINATION.rglob("*")
        if item.is_file() and item.name != "PACKAGE-MANIFEST.json"
    ):
        files.append(
            {
                "path": path.relative_to(DESTINATION).as_posix(),
                "bytes": path.stat().st_size,
                "sha256": sha256(path),
            }
        )
    manifest = {
        "formatVersion": 1,
        "packageStatus": "not-ready",
        "sourceCommit": status["sourceCommit"],
        "packageCheckoutCommit": package_commit,
        "packageTrackedChanges": tracked_changes,
        "executableSha256": status["executableSha256"],
        "fileCountExcludingManifest": len(files),
        "uncompressedBytesExcludingManifest": sum(item["bytes"] for item in files),
        "files": files,
        "openGates": report["openGates"],
    }
    (DESTINATION / "PACKAGE-MANIFEST.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return {
        "destination": str(DESTINATION),
        "packageStatus": "not-ready",
        "fileCountExcludingManifest": len(files),
        "uncompressedBytesExcludingManifest": manifest["uncompressedBytesExcludingManifest"],
        "openGates": len(report["openGates"]),
        "stagingPerformed": True,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--refresh",
        action="store_true",
        help="update a previously staged package after verifying its manifest and files",
    )
    result = stage_package(refresh=parser.parse_args().refresh)
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
