"""Stage a local Windows preview candidate without claiming it is distributable."""

from __future__ import annotations

import hashlib
import json
import shutil
from pathlib import Path

from check_windows_preview_package import ROOT, check_package


DESTINATION = ROOT / "target" / "windows-preview-package-6d8147c"


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


def stage_package() -> dict:
    report = check_package()
    if report["missingResources"] or report["problems"]:
        raise RuntimeError(
            "Cannot stage an incomplete candidate: "
            f"missing={report['missingResources']}, problems={report['problems']}"
        )
    if DESTINATION.exists():
        raise FileExistsError(f"Refusing to overwrite existing staging directory: {DESTINATION}")

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

    shutil.copytree(ROOT / "guide", DESTINATION / "guide", ignore=shutil.ignore_patterns("*.lock"))
    shutil.copytree(ROOT / "licenses" / "third_party", DESTINATION / "licenses" / "third_party")
    copy_file("target/release/lapui.exe", DESTINATION, "lapui.exe")
    copy_file("target/windows-release-sources.zip", DESTINATION, "sources/windows-release-sources.zip")

    status = {
        "packageStatus": "not-ready",
        "sourceCommit": report["releaseCandidate"]["expectedSource"],
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
    for path in sorted(item for item in DESTINATION.rglob("*") if item.is_file()):
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
    result = stage_package()
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
