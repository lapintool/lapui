"""Inventory license metadata and bundled notices for the Windows release graph.

This is an input audit, not a legal-compliance certification. It deliberately
reports packages that declare an SPDX expression but expose no top-level or
declared license file in the cached crate archive, so packaging cannot silently
treat metadata as a complete third-party notice bundle.
"""

from __future__ import annotations

import argparse
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
        entries.append(
            {
                "name": name,
                "version": version,
                "license": metadata.get("license"),
                "authors": metadata.get("authors", []),
                "repository": metadata.get("repository"),
                "licenseFiles": license_files,
                "noticeFiles": notices,
                "licenseTextMissing": not license_files,
            }
        )

    return {
        "target": target,
        "featureSet": "default",
        "dependencyCount": len(packages),
        "missingSourceArchives": missing_sources,
        "missingLicenseMetadata": [entry for entry in entries if not entry["license"]],
        "missingLicenseText": [entry for entry in entries if entry["licenseTextMissing"]],
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
        else 0
    )


if __name__ == "__main__":
    raise SystemExit(main())
