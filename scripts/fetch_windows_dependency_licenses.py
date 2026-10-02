"""Recover license and notice files omitted from locked Cargo crate archives.

For packages without license text in the registry archive, this fetches only
license/notice files from the exact upstream commit recorded in
``.cargo_vcs_info.json``. It uses the public GitHub API and writes a source
manifest alongside the retrieved texts. It does not decide legal sufficiency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import tomllib
from pathlib import Path, PurePosixPath
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlparse
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
LICENSE_NAME = re.compile(r"^(LICENSE|LICENCE|COPYING|COPYRIGHT|NOTICE)(?:[-_.].*)?$", re.IGNORECASE)
API_BASE = "https://api.github.com"
RAW_BASE = "https://raw.githubusercontent.com"
SPDX_TEXTS = {
    "CC0-1.0": "https://spdx.org/licenses/CC0-1.0.txt",
    "MIT": "https://spdx.org/licenses/MIT.txt",
    "MPL-2.0": "https://spdx.org/licenses/MPL-2.0.txt",
}
TAG_FALLBACK_COMMITS = {
    ("reem/rust-void", "1.0.2"): "ab2f4dbb7c95c144ccba2fa8afd11e155aa91133",
}


def registry_roots() -> list[Path]:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    return list((cargo_home / "registry" / "src").glob("*"))


def github_repository(url: str | None) -> str | None:
    if not url:
        return None
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.netloc.lower() != "github.com":
        return None
    path = parsed.path.strip("/")
    if path.endswith(".git"):
        path = path[:-4]
    parts = path.split("/")
    return "/".join(parts[:2]) if len(parts) >= 2 else None


def get_json(url: str) -> dict:
    request = Request(
        url,
        headers={"Accept": "application/vnd.github+json", "User-Agent": "lapui-license-audit"},
    )
    with urlopen(request, timeout=30) as response:
        return json.load(response)


def get_bytes(url: str) -> bytes:
    request = Request(url, headers={"User-Agent": "lapui-license-audit"})
    with urlopen(request, timeout=30) as response:
        return response.read()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--audit",
        type=Path,
        default=ROOT / "target" / "windows-release-license-audit.json",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "licenses" / "third_party" / "upstream",
    )
    parser.add_argument("--refresh-upstream", action="store_true", help="ignore cached upstream license files and query GitHub again")
    args = parser.parse_args()
    audit = json.loads(args.audit.read_text(encoding="utf-8"))
    roots = registry_roots()
    output = args.output.resolve()
    previous_manifest_path = output / "SOURCES.json"
    previous_manifest = (
        json.loads(previous_manifest_path.read_text(encoding="utf-8"))
        if previous_manifest_path.is_file()
        else {}
    )
    cached_source_errors = {
        (error.get("repository", "").removeprefix("https://github.com/"), error.get("commit"))
        for error in previous_manifest.get("sourceErrors", [])
        if error.get("repository") and error.get("commit")
        and "no exact license file" not in error.get("reason", "")
    }
    no_upstream_file_packages = {
        (package["name"], package["version"])
        for package in audit.get("missingExactUpstreamLicenseFile", [])
    }
    packages_needing_sources = [
        package for package in audit.get("packages", []) if not package.get("licenseFiles")
    ]

    groups: dict[tuple[str, str], list[dict]] = {}
    errors = []
    for package in packages_needing_sources:
        repo = github_repository(package.get("repository"))
        source_roots = [root / f"{package['name']}-{package['version']}" for root in roots]
        source_root = next((path for path in source_roots if path.is_dir()), None)
        vcs_path = source_root / ".cargo_vcs_info.json" if source_root else None
        if repo is None:
            errors.append({"package": f"{package['name']} {package['version']}", "reason": "no public GitHub repository"})
            continue
        source_reference = ""
        if vcs_path is not None and vcs_path.is_file():
            vcs = json.loads(vcs_path.read_text(encoding="utf-8"))
            commit = vcs.get("git", {}).get("sha1")
            package_path = vcs.get("path_in_vcs", "")
            source_reference = f"commit:{commit}"
        else:
            # Older crates may omit Cargo's VCS metadata. In that case, use
            # only a tag whose name exactly matches the locked crate version.
            if (repo, package["version"]) in TAG_FALLBACK_COMMITS:
                commit = TAG_FALLBACK_COMMITS[(repo, package["version"])]
                package_path = ""
                source_reference = f"tag:{package['version']}"
            else:
                try:
                    tag = get_json(f"{API_BASE}/repos/{repo}/git/ref/tags/{quote(package['version'], safe='')}")
                    tag_object = tag["object"]
                    if tag_object["type"] == "tag":
                        tag_object = get_json(tag_object["url"])
                        tag_object = tag_object["object"]
                    commit = tag_object["sha"] if tag_object["type"] == "commit" else None
                    package_path = ""
                    source_reference = f"tag:{package['version']}"
                except (HTTPError, URLError, TimeoutError, KeyError) as error:
                    errors.append({"package": f"{package['name']} {package['version']}", "reason": f"no exact source commit or release tag: {error}"})
                    continue
        if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
            errors.append({"package": f"{package['name']} {package['version']}", "reason": "invalid source commit"})
            continue
        groups.setdefault((repo, commit), []).append(
            {
                "name": package["name"],
                "version": package["version"],
                "license": package.get("license"),
                "pathInRepository": package_path,
                "sourceReference": source_reference,
            }
        )
    manifests = []
    upstream_files_unavailable = []
    for (repo, commit), packages in sorted(groups.items()):
        cached_directory = output / repo / commit
        candidates = []
        if not args.refresh_upstream and cached_directory.is_dir():
            candidates = [
                path.relative_to(cached_directory).as_posix()
                for path in cached_directory.rglob("*")
                if path.is_file() and LICENSE_NAME.fullmatch(path.name)
            ]
        if not candidates and (repo, commit) not in cached_source_errors:
            if any((package["name"], package["version"]) in no_upstream_file_packages for package in packages):
                upstream_files_unavailable.append(
                    {
                        "repository": repo,
                        "commit": commit,
                        "packages": [f"{package['name']} {package['version']}" for package in packages],
                        "reason": "no exact upstream license/notice file; canonical SPDX text is included",
                    }
                )
                continue
            try:
                tree = get_json(f"{API_BASE}/repos/{repo}/git/trees/{commit}?recursive=1")
            except (HTTPError, URLError, TimeoutError) as error:
                errors.append({"repository": repo, "commit": commit, "reason": str(error)})
                continue
            if tree.get("truncated"):
                errors.append({"repository": repo, "commit": commit, "reason": "Git tree response was truncated"})
                continue

            files = [item["path"] for item in tree.get("tree", []) if item.get("type") == "blob"]
            allowed_parents = {PurePosixPath(".")}
            for package in packages:
                path = PurePosixPath(package["pathInRepository"] or ".")
                allowed_parents.update(path.parents)
                allowed_parents.add(path)
            candidates = [
                path
                for path in files
                if PurePosixPath(path).parent in allowed_parents
                and LICENSE_NAME.fullmatch(PurePosixPath(path).name)
            ]
        if not candidates:
            upstream_files_unavailable.append(
                {
                    "repository": repo,
                    "commit": commit,
                    "packages": [f"{package['name']} {package['version']}" for package in packages],
                    "reason": "no exact upstream license/notice file; canonical SPDX text is included",
                }
            )
            continue

        downloaded = []
        for relative in sorted(candidates):
            target = output / repo / commit / Path(*PurePosixPath(relative).parts)
            url = f"{RAW_BASE}/{repo}/{commit}/{quote(relative, safe='/')}"
            if not args.refresh_upstream and target.is_file():
                content = target.read_bytes()
            else:
                try:
                    content = get_bytes(url)
                except (HTTPError, URLError, TimeoutError) as error:
                    errors.append({"repository": repo, "commit": commit, "path": relative, "reason": str(error)})
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
            downloaded.append(
                {
                    "repositoryPath": relative,
                    "bundlePath": target.relative_to(output).as_posix(),
                    "url": url,
                    "sha256": hashlib.sha256(content).hexdigest(),
                }
            )
        manifests.append(
            {
                "repository": f"https://github.com/{repo}",
                "commit": commit,
                "packages": packages,
                "licenseAndNoticeFiles": downloaded,
            }
        )

    canonical_spdx = []
    for identifier, url in SPDX_TEXTS.items():
        target = output / "SPDX" / f"{identifier}.txt"
        if not args.refresh_upstream and target.is_file():
            content = target.read_bytes()
        else:
            try:
                content = get_bytes(url)
            except (HTTPError, URLError, TimeoutError) as error:
                errors.append({"licenseId": identifier, "reason": f"canonical text unavailable: {error}"})
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        canonical_spdx.append(
            {
                "licenseId": identifier,
                "path": target.relative_to(output).as_posix(),
                "url": url,
                "sha256": hashlib.sha256(content).hexdigest(),
            }
        )
    spdx_manifest_path = output / "SPDX-SOURCES.json"
    spdx_manifest_path.write_text(
        json.dumps({"source": "SPDX License List", "licenses": canonical_spdx}, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )

    manifest = {
        "target": audit["target"],
        "featureSet": audit["featureSet"],
        "sourceSelection": "Cargo VCS commit, or exact version tag when VCS metadata is absent",
        "canonicalSpdxTexts": spdx_manifest_path.relative_to(output).as_posix(),
        "groups": manifests,
        "upstreamFilesUnavailable": upstream_files_unavailable,
        "sourceErrors": errors,
    }
    output.mkdir(parents=True, exist_ok=True)
    manifest_path = output / "SOURCES.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(
        json.dumps(
            {
                "output": str(output),
                "repositoryCommits": len(manifests),
                "packages": sum(len(group["packages"]) for group in manifests),
                "licenseAndNoticeFiles": sum(len(group["licenseAndNoticeFiles"]) for group in manifests),
                "manifest": str(manifest_path),
                "upstreamFilesUnavailable": len(upstream_files_unavailable),
                "sourceErrors": errors,
            },
            ensure_ascii=False,
        )
    )
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
