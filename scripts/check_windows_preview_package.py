"""Check local inputs and open gates for the Windows developer preview package.

This is a read-only staging check. It does not copy files, build Lapui, install
prerequisites, or certify the remaining clean-machine/manual acceptance items.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import tomllib
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RELEASE_SOURCE = "2216726ce5ff3416b660d6c069f6aec66aba71f8"
RELEASE_SHA256 = "083B516CD5278A6583DD125A29F55FAAA6637BC1B8AF475C2D82C062808D1926"
RELEASE_BYTES = 33_589_760
EXPECTED_DEPENDENCIES = 341


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
        "Run the staged package on a clean Windows 11 user environment after following its documented VC++ prerequisite.",
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
        "prerequisites/README.md",
    ]
    for relative in required_files:
        if not (ROOT / relative).is_file():
            missing_resources.append(relative)

    guide_files = sorted((ROOT / "guide").rglob("*.md"))
    if not guide_files:
        problems.append("No tracked guide Markdown files were found.")

    executable = ROOT / "target" / "release" / "lapui.exe"
    executable_info = {
        "path": str(executable),
        "exists": executable.is_file(),
        "expectedSource": RELEASE_SOURCE,
        "expectedBytes": RELEASE_BYTES,
        "expectedSha256": RELEASE_SHA256,
    }
    if executable.is_file():
        executable_info.update({"bytes": executable.stat().st_size, "sha256": sha256(executable)})
        if executable_info["bytes"] != RELEASE_BYTES or executable_info["sha256"] != RELEASE_SHA256:
            problems.append(f"The release executable differs from the reviewed {RELEASE_SOURCE[:7]} candidate.")
    else:
        missing_resources.append("target/release/lapui.exe")

    archive = ROOT / "target" / "windows-release-sources.zip"
    source_info = {"path": str(archive), "exists": archive.is_file()}
    if archive.is_file():
        lock_packages = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8")).get("package", [])
        registry_checksums = {
            (package["name"], package["version"]): package["checksum"]
            for package in lock_packages
            if package.get("source", "").startswith("registry+") and package.get("checksum")
        }
        audit_path = ROOT / "target" / "windows-release-license-audit.json"
        try:
            audit_packages = json.loads(audit_path.read_text(encoding="utf-8")).get("packages", [])
        except (OSError, json.JSONDecodeError) as error:
            audit_packages = []
            problems.append(f"The license audit report could not be used to verify source packages: {error}")
        expected_packages = {
            (package["name"], package["version"])
            for package in audit_packages
            if package.get("name") and package.get("version")
        }
        locked_checksums = {package: registry_checksums.get(package) for package in expected_packages}
        try:
            with zipfile.ZipFile(archive) as bundle:
                bad_member = bundle.testzip()
                manifest = json.loads(bundle.read("SOURCES.json"))
                members = set(bundle.namelist())
                crate_count = sum(name.startswith("crates/") and name.endswith(".crate") for name in members)
                bad_checksums = []
                source_packages = {
                    (package.get("name"), package.get("version")): package
                    for package in manifest.get("packages", [])
                }
                missing_locked = sorted(expected_packages - set(source_packages))
                unexpected_packages = sorted(set(source_packages) - expected_packages)
                lockfile_checksum_mismatches = [
                    f"{name}-{version}"
                    for (name, version), checksum in locked_checksums.items()
                    if (name, version) in source_packages
                    and (
                        checksum is None
                        or source_packages[(name, version)].get("crateSha256") != checksum
                    )
                ]
                for package in manifest.get("packages", []):
                    member = f"crates/{package['name']}-{package['version']}.crate"
                    if member not in members:
                        bad_checksums.append({"package": member, "error": "missing archive"})
                        continue
                    digest = hashlib.sha256()
                    with bundle.open(member) as stream:
                        for block in iter(lambda: stream.read(1024 * 1024), b""):
                            digest.update(block)
                    if digest.hexdigest() != package.get("crateSha256"):
                        bad_checksums.append({"package": member, "error": "Cargo.lock checksum mismatch"})
            source_info.update(
                {
                    "bytes": archive.stat().st_size,
                    "dependencyCount": manifest.get("dependencyCount"),
                    "crateArchiveCount": crate_count,
                    "checksumVerifiedCount": len(manifest.get("packages", [])) - len(bad_checksums),
                    "lockfilePackageCount": len(expected_packages),
                    "lockfileMatchedPackageCount": len(expected_packages)
                    - len(missing_locked)
                    - len(lockfile_checksum_mismatches),
                    "unexpectedPackageCount": len(unexpected_packages),
                    "zipIntegrity": "passed" if bad_member is None else f"failed:{bad_member}",
                }
            )
            if bad_member is not None:
                problems.append(f"The source archive has a corrupt ZIP member: {bad_member}")
            if manifest.get("dependencyCount") != EXPECTED_DEPENDENCIES or crate_count != EXPECTED_DEPENDENCIES:
                problems.append(f"The source archive does not contain all {EXPECTED_DEPENDENCIES} reviewed dependency archives.")
            if bad_checksums:
                problems.append(f"The source archive has missing or checksum-mismatched crate entries: {bad_checksums[:5]}")
            if missing_locked or unexpected_packages or lockfile_checksum_mismatches:
                problems.append(
                    "The source archive manifest differs from Cargo.lock: "
                    f"missing={missing_locked[:5]}, unexpected={unexpected_packages[:5]}, "
                    f"checksumMismatches={lockfile_checksum_mismatches[:5]}"
                )
        except (OSError, KeyError, json.JSONDecodeError, zipfile.BadZipFile) as error:
            problems.append(f"The source archive could not be verified: {error}")
    else:
        missing_resources.append("target/windows-release-sources.zip")

    prerequisite_readme = ROOT / "prerequisites" / "README.md"
    prerequisite_info = {
        "delivery": "user-installed",
        "instructionPath": str(prerequisite_readme),
        "exists": prerequisite_readme.is_file(),
    }
    if prerequisite_readme.is_file():
        prerequisite_text = prerequisite_readme.read_text(encoding="utf-8")
        prerequisite_info["officialMicrosoftLinkDocumented"] = (
            "https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist" in prerequisite_text
        )
        prerequisite_info["x64RuntimeDocumented"] = "x64" in prerequisite_text and "Redistributable" in prerequisite_text
        if not prerequisite_info["officialMicrosoftLinkDocumented"] or not prerequisite_info["x64RuntimeDocumented"]:
            problems.append("The documented VC++ prerequisite does not identify Microsoft's official x64 download.")

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
                problems.append(f"The license audit does not cover all {EXPECTED_DEPENDENCIES} dependencies.")
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
    open_gates.extend(manual_gates)

    return {
        "packageStatus": "not-ready",
        "releaseCandidate": executable_info,
        "runtimePrerequisite": prerequisite_info,
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
