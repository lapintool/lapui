"""Inventory license metadata and bundled notices for the Windows release graph.

This is an input audit, not a legal-compliance certification. It reports source
license files and checksum-pinned distribution notices separately; downstream
copyright evidence does not clear the review gate until explicitly approved.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_LINE = re.compile(r"([^ ]+) v([^ ]+)(?: .*)?")
LICENSE_FILE = re.compile(r"^(LICENSE|LICENCE|COPYING)(?:[-_.].*)?$", re.IGNORECASE)
NOTICE_FILE = re.compile(r"^(NOTICE|COPYRIGHT)(?:[-_.].*)?$", re.IGNORECASE)
LICENSE_ID = re.compile(r"\b[A-Za-z][A-Za-z0-9.+-]*\b")


def bundled_license_sources() -> tuple[dict[tuple[str, str], list[dict]], dict[str, dict]]:
    root = ROOT / "licenses" / "third_party" / "upstream"
    package_sources: dict[tuple[str, str], list[dict]] = {}
    manifest_path = root / "SOURCES.json"
    if manifest_path.is_file():
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for group in manifest.get("groups", []):
            files = group.get("licenseAndNoticeFiles", [])
            license_files = []
            for entry in files:
                relative = entry.get("bundlePath")
                if not relative or not LICENSE_FILE.match(Path(relative).name):
                    continue
                path = root / relative
                if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == entry.get("sha256"):
                    license_files.append({"path": str(path.relative_to(ROOT)), "sha256": entry["sha256"]})
            for package in group.get("packages", []):
                package_sources[(package["name"], package["version"])] = license_files

    standard_sources: dict[str, dict] = {}
    spdx_manifest_path = root / "SPDX-SOURCES.json"
    if spdx_manifest_path.is_file():
        manifest = json.loads(spdx_manifest_path.read_text(encoding="utf-8"))
        for entry in manifest.get("licenses", []):
            relative = entry.get("path")
            identifier = entry.get("licenseId")
            if not relative or not identifier:
                continue
            path = root / relative
            if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == entry.get("sha256"):
                standard_sources[identifier] = {
                    "path": str(path.relative_to(ROOT)),
                    "sha256": entry["sha256"],
                    "url": entry.get("url"),
                }
    return package_sources, standard_sources


def distribution_notice_sources() -> dict[tuple[str, str], list[dict]]:
    """Load checksum-pinned notice evidence from downstream source packages."""
    manifest_path = ROOT / "licenses" / "third_party" / "distribution" / "SOURCES.json"
    if not manifest_path.is_file():
        return {}

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    package_sources: dict[tuple[str, str], list[dict]] = {}
    for entry in manifest.get("notices", []):
        relative = entry.get("noticePath")
        evidence_relative = entry.get("evidencePath")
        name = entry.get("package")
        version = entry.get("version")
        notice_hash = entry.get("noticeSha256")
        evidence_hash = entry.get("evidenceSha256")
        if not relative or not evidence_relative or not name or not version or not notice_hash or not evidence_hash:
            continue
        distribution_root = ROOT / "licenses" / "third_party" / "distribution"
        notice_path = distribution_root / relative
        evidence_path = distribution_root / evidence_relative
        if (
            not notice_path.is_file()
            or hashlib.sha256(notice_path.read_bytes()).hexdigest() != notice_hash
            or not evidence_path.is_file()
            or hashlib.sha256(evidence_path.read_bytes()).hexdigest() != evidence_hash
        ):
            continue
        package_sources.setdefault((name, version), []).append(
            {
                "path": str(notice_path.relative_to(ROOT)),
                "sha256": notice_hash,
                "source": entry.get("source"),
                "sourcePackage": entry.get("sourcePackage"),
                "evidenceScope": entry.get("evidenceScope"),
                "reviewStatus": entry.get("reviewStatus", "pending-human-notice-review"),
                "evidencePath": str(evidence_path.relative_to(ROOT)),
                "evidenceSha256": evidence_hash,
            }
        )
    return package_sources


def registry_roots() -> list[Path]:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    return list((cargo_home / "registry" / "src").glob("*"))


def license_graph(target: str, roots: list[Path]) -> dict:
    result = subprocess.run(
        [
            "cargo",
            "tree",
            "--locked",
            "--offline",
            "--target",
            target,
            "-e",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    packages: set[tuple[str, str]] = set()
    for line in result.stdout.splitlines():
        match = PACKAGE_LINE.fullmatch(line)
        if match and match.group(1) != "lapui":
            packages.add(match.groups())

    upstream_license_sources, standard_license_sources = bundled_license_sources()
    distribution_sources = distribution_notice_sources()
    entries = []
    missing_sources = []
    for name, version in sorted(packages):
        package_roots = [base / f"{name}-{version}" for base in roots if (base / f"{name}-{version}").is_dir()]
        if not package_roots:
            missing_sources.append({"name": name, "version": version})
            continue
        package_root = package_roots[0]
        manifest = tomllib.loads((package_root / "Cargo.toml").read_text(encoding="utf-8"))
        metadata = manifest.get("package", {})
        license_file = metadata.get("license-file")
        license_files = sorted(
            path.name for path in package_root.iterdir() if path.is_file() and LICENSE_FILE.match(path.name)
        )
        notices = sorted(
            path.name for path in package_root.iterdir() if path.is_file() and NOTICE_FILE.match(path.name)
        )
        if license_file and (package_root / license_file).is_file() and license_file not in license_files:
            license_files.append(license_file)
            license_files.sort()
        upstream_license_files = upstream_license_sources.get((name, version), [])
        distribution_notice_files = distribution_sources.get((name, version), [])
        expression_ids = {
            identifier
            for identifier in LICENSE_ID.findall(metadata.get("license") or "")
            if identifier not in {"AND", "OR", "WITH"}
        }
        standard_license_texts = [
            {"licenseId": identifier, **standard_license_sources[identifier]}
            for identifier in sorted(expression_ids)
            if identifier in standard_license_sources
        ]
        has_complete_standard_text = bool(expression_ids) and expression_ids <= set(standard_license_sources)
        reviewed_distribution_notice = any(
            source["reviewStatus"] == "approved" for source in distribution_notice_files
        )
        entries.append(
            {
                "name": name,
                "version": version,
                "license": metadata.get("license"),
                "authors": metadata.get("authors", []),
                "repository": metadata.get("repository"),
                "licenseFiles": license_files,
                "noticeFiles": notices,
                "upstreamLicenseFiles": upstream_license_files,
                "distributionNoticeFiles": distribution_notice_files,
                "standardLicenseTexts": standard_license_texts,
                "licenseTextMissing": not (
                    license_files or upstream_license_files or distribution_notice_files or has_complete_standard_text
                ),
                "copyrightTemplateReviewRequired": "MIT" in expression_ids
                and not (license_files or upstream_license_files or reviewed_distribution_notice),
            }
        )

    return {
        "target": target,
        "featureSet": "default",
        "dependencyCount": len(packages),
        "missingSourceArchives": missing_sources,
        "missingLicenseMetadata": [entry for entry in entries if not entry["license"]],
        "missingLicenseText": [entry for entry in entries if entry["licenseTextMissing"]],
        "missingExactUpstreamLicenseFile": [
            entry for entry in entries if not entry["licenseFiles"] and not entry["upstreamLicenseFiles"]
        ],
        "packagesUsingCanonicalLicenseText": [
            entry
            for entry in entries
            if entry["standardLicenseTexts"]
            and not entry["licenseFiles"]
            and not entry["upstreamLicenseFiles"]
            and not entry["distributionNoticeFiles"]
        ],
        "copyrightTemplateReviewRequired": [entry for entry in entries if entry["copyrightTemplateReviewRequired"]],
        "packagesWithDistributionNoticeEvidence": [
            entry for entry in entries if entry["distributionNoticeFiles"]
        ],
        "packages": entries,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "target" / "windows-release-license-audit.json",
    )
    args = parser.parse_args()
    report = license_graph(args.target, registry_roots())
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "output": str(args.output.resolve()),
                "target": report["target"],
                "dependencyCount": report["dependencyCount"],
                "missingSourceArchives": len(report["missingSourceArchives"]),
                "missingLicenseMetadata": len(report["missingLicenseMetadata"]),
                "missingLicenseText": len(report["missingLicenseText"]),
                "missingExactUpstreamLicenseFile": len(report["missingExactUpstreamLicenseFile"]),
                "packagesUsingCanonicalLicenseText": len(report["packagesUsingCanonicalLicenseText"]),
                "packagesWithDistributionNoticeEvidence": len(report["packagesWithDistributionNoticeEvidence"]),
                "copyrightTemplateReviewRequired": len(report["copyrightTemplateReviewRequired"]),
                "mplPackages": sum("MPL-2.0" in (entry["license"] or "") for entry in report["packages"]),
            },
            ensure_ascii=False,
        )
    )
    return (
        1
        if report["missingSourceArchives"]
        or report["missingLicenseMetadata"]
        or report["missingLicenseText"]
        or report["copyrightTemplateReviewRequired"]
        else 0
    )


if __name__ == "__main__":
    raise SystemExit(main())
