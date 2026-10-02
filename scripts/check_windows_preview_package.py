"""Check local inputs and open gates for the Windows developer preview package.

This is a read-only staging check. It does not copy files, build Lapui, install
prerequisites, or certify the remaining clean-machine/manual acceptance items.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RELEASE_SHA256 = "C8FF3F17F8F00769CF27EA259E3D9BB4AD874FDCDF557033396DDF48779EBD48"
RELEASE_BYTES = 33_571_328
EXPECTED_DEPENDENCIES = 338


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest().upper()


def check_package() -> dict:
    missing_resources: list[str] = []
    problems: list[str] = []
    manual_gates = [
        "Run the staged package on a clean Windows 11 user environment.",
        "Verify CJK fallback/rendering and the MCP stdio workflow on that machine.",
    ]

    required_files = [
        "README.md",
        "README.zh-CN.md",
        "THIRD_PARTY.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "guide/SUMMARY.md",
        "guide/windows-preview-package.md",
        "licenses/third_party/upstream/SOURCES.json",
        "licenses/third_party/upstream/SPDX-SOURCES.json",
        "licenses/third_party/distribution/SOURCES.json",
        "licenses/third_party/distribution/notices/void-1.0.2-MIT.txt",
    ]
    for relative in required_files:
        if not (ROOT / relative).is_file():
            missing_resources.append(relative)

    guide_files = sorted((ROOT / "guide").rglob("*.md"))
    if not guide_files:
        problems.append("No tracked guide Markdown files were found.")

    executable = ROOT / "target" / "release" / "lapui.exe"
    executable_info = {"path": str(executable), "exists": executable.is_file()}
    if executable.is_file():
        executable_info.update({"bytes": executable.stat().st_size, "sha256": sha256(executable)})
        if executable_info["bytes"] != RELEASE_BYTES or executable_info["sha256"] != RELEASE_SHA256:
            problems.append("The release executable differs from the reviewed b098e0e candidate.")
    else:
        missing_resources.append("target/release/lapui.exe")

    archive = ROOT / "target" / "windows-release-sources.zip"
    source_info = {"path": str(archive), "exists": archive.is_file()}
    if archive.is_file():
        try:
            with zipfile.ZipFile(archive) as bundle:
                bad_member = bundle.testzip()
                manifest = json.loads(bundle.read("SOURCES.json"))
                crate_count = sum(
                    name.startswith("crates/") and name.endswith(".crate") for name in bundle.namelist()
                )
            source_info.update(
                {
                    "bytes": archive.stat().st_size,
                    "dependencyCount": manifest.get("dependencyCount"),
                    "crateArchiveCount": crate_count,
                    "zipIntegrity": "passed" if bad_member is None else f"failed:{bad_member}",
                }
            )
            if bad_member is not None:
                problems.append(f"The source archive has a corrupt ZIP member: {bad_member}")
            if manifest.get("dependencyCount") != EXPECTED_DEPENDENCIES or crate_count != EXPECTED_DEPENDENCIES:
                problems.append("The source archive does not contain all 338 reviewed dependency archives.")
        except (OSError, KeyError, json.JSONDecodeError, zipfile.BadZipFile) as error:
            problems.append(f"The source archive could not be verified: {error}")
    else:
        missing_resources.append("target/windows-release-sources.zip")

    redist = ROOT / "target" / "windows-preview-package" / "prerequisites" / "vc_redist.x64.exe"
    if not redist.is_file():
        missing_resources.append("prerequisites/vc_redist.x64.exe")

    audit_path = ROOT / "target" / "windows-release-license-audit.json"
    audit_info = {"path": str(audit_path), "exists": audit_path.is_file()}
    notice_review_required = True
    if audit_path.is_file():
        try:
            audit = json.loads(audit_path.read_text(encoding="utf-8"))
            pending = audit.get("copyrightTemplateReviewRequired", [])
            notice_review_required = bool(pending)
            audit_info.update(
                {
                    "dependencyCount": audit.get("dependencyCount"),
                    "missingSourceArchives": len(audit.get("missingSourceArchives", [])),
                    "missingLicenseMetadata": len(audit.get("missingLicenseMetadata", [])),
                    "missingLicenseText": len(audit.get("missingLicenseText", [])),
                    "distributionNoticeEvidence": sum(
                        bool(item.get("distributionNoticeFiles")) for item in audit.get("packages", [])
                    ),
                    "copyrightNoticeReviewRequired": notice_review_required,
                }
            )
            if audit_info["dependencyCount"] != EXPECTED_DEPENDENCIES:
                problems.append("The license audit does not cover all 338 dependencies.")
            if any(
                audit_info[key]
                for key in ("missingSourceArchives", "missingLicenseMetadata", "missingLicenseText")
            ):
                problems.append("The license audit reports unresolved source, metadata, or license-text gaps.")
        except (OSError, json.JSONDecodeError) as error:
            problems.append(f"The license audit report could not be read: {error}")
    else:
        missing_resources.append("target/windows-release-license-audit.json")

    open_gates = []
    if notice_review_required:
        open_gates.append("Human review of the downstream void 1.0.2 notice evidence and staged wording.")
    if not redist.is_file():
        open_gates.append("Stage the official x64 Visual C++ Redistributable or revalidate a static-CRT build.")
    open_gates.extend(manual_gates)

    return {
        "packageStatus": "not-ready",
        "releaseCandidate": executable_info,
        "sourceArchive": source_info,
        "licenseAudit": audit_info,
        "missingResources": missing_resources,
        "problems": problems,
        "openGates": open_gates,
        "guideMarkdownFileCount": len(guide_files),
        "stagingPerformed": False,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "target" / "windows-preview-package-check.json",
    )
    args = parser.parse_args()
    report = check_package()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    summary = {
        "output": str(args.output.resolve()),
        "packageStatus": report["packageStatus"],
        "missingResources": len(report["missingResources"]),
        "problems": len(report["problems"]),
        "openGates": len(report["openGates"]),
        "stagingPerformed": report["stagingPerformed"],
    }
    print(json.dumps(summary, ensure_ascii=False))
    return 1 if report["missingResources"] or report["problems"] or report["openGates"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
