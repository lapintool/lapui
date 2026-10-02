"""Create a checksum-verified source archive for the audited Windows graph.

This preserves the exact Cargo registry source archives corresponding to the
locked Windows release dependencies. It is a source-availability artifact, not
a substitute for license notices or legal review.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import tomllib
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_registry_checksums(lockfile: Path) -> dict[tuple[str, str], str]:
    lock = tomllib.loads(lockfile.read_text(encoding="utf-8"))
    checksums = {}
    for package in lock.get("package", []):
        source = package.get("source", "")
        checksum = package.get("checksum")
        if source.startswith("registry+") and checksum:
            checksums[(package["name"], package["version"])] = checksum
    return checksums


def find_archive(name: str, version: str, roots: list[Path]) -> Path | None:
    filename = f"{name}-{version}.crate"
    return next((path for root in roots if (path := root / filename).is_file()), None)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--audit",
        type=Path,
        default=ROOT / "target" / "windows-release-license-audit.json",
    )
    parser.add_argument(
        "--lockfile",
        type=Path,
        default=ROOT / "Cargo.lock",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "target" / "windows-release-sources.zip",
    )
    args = parser.parse_args()
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    cache_roots = list((cargo_home / "registry" / "cache").glob("*"))
    audit = json.loads(args.audit.read_text(encoding="utf-8"))
    checksums = load_registry_checksums(args.lockfile)

    entries = []
    missing = []
    for package in audit["packages"]:
        name, version = package["name"], package["version"]
        checksum = checksums.get((name, version))
        archive = find_archive(name, version, cache_roots)
        if checksum is None or archive is None:
            missing.append({"name": name, "version": version, "missingChecksum": checksum is None,
                            "missingArchive": archive is None})
            continue
        actual = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual != checksum:
            missing.append({"name": name, "version": version, "checksumMismatch": True})
            continue
        entries.append(
            {
                "name": name,
                "version": version,
                "license": package.get("license"),
                "repository": package.get("repository"),
                "crateSha256": checksum,
                "crateUrl": f"https://crates.io/api/v1/crates/{name}/{version}/download",
                "archive": archive,
            }
        )

    if missing:
        print(json.dumps({"missingOrInvalidSources": missing}, ensure_ascii=False))
        return 1

    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".partial")
    manifest = {
        "target": audit["target"],
        "featureSet": audit["featureSet"],
        "dependencyCount": len(entries),
        "source": "Cargo.lock registry checksums",
        "packages": [
            {key: value for key, value in entry.items() if key != "archive"}
            for entry in entries
        ],
    }
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
            bundle.writestr("SOURCES.json", json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
            for entry in entries:
                bundle.write(entry["archive"], f"crates/{entry['name']}-{entry['version']}.crate")
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)

    print(
        json.dumps(
            {
                "output": str(output),
                "dependencyCount": len(entries),
                "archiveBytes": output.stat().st_size,
                "licenseNoticeStatus": "separate audit required",
            },
            ensure_ascii=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
