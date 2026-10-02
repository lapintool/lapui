# Windows developer preview package

This is a static staging checklist for the x64 Windows developer preview. It does not describe a finished installer or a production release. The current published source baseline remains `b098e0e`; the locally validated release candidate is built from commit `6d8147cf6b6520f9b18bc1ae79170b9dc989b171`. The source archive and license audit are separately verified against the unchanged `Cargo.lock`; they are not executable build inputs.

Run `py -3 scripts/check_windows_preview_package.py` to verify that the reviewed executable, source archive, license report, notices, and documented package inputs are present. It writes a JSON report under ignored `target/` and exits nonzero while any resource or acceptance gate remains open. The checker is read-only apart from that report; it does not stage files or run the executable.

## Smallest useful package

```text
Lapui-preview/
├── lapui.exe
├── README.md
├── README.zh-CN.md
├── THIRD_PARTY.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── guide/                         # tracked Markdown guides
├── licenses/third_party/upstream/ # exact upstream notices + SPDX sources
├── licenses/third_party/distribution/ # assembled notice + Debian evidence/source hashes
├── sources/windows-release-sources.zip
└── prerequisites/
    └── README.md                  # user installs Microsoft's x64 redistributable if needed
```

The `--demo local-files --directory <path>` entry point embeds its HTML and JavaScript with `include_str!`; it does not need a separate UI asset directory. Lapui's own UI bridges are embedded the same way. The local-files demo reads only the selected directory's top-level names and sizes. Example pages are optional: if they are shipped, copy each example's complete referenced subtree, including `dist/` bundles, local modules, styles, and other assets. Node is needed to rebuild some example bundles, not to run the checked-in bundles. The runtime does not require a system WebView.

The source archive currently contains 338 lockfile-verified registry crate archives. It is 53,175,819 bytes and passed ZIP integrity checking. Its `SOURCES.json` records package versions and Cargo.lock checksums. The archive is currently ignored build output under `target/`, so it must be deliberately copied into the staging directory or made available from a durable source location. Source availability does not replace license notices.

## Runtime prerequisites and resources

The current 34,113,536-byte x64 executable has SHA-256 `EB1F48098FC827A3C1DC7D9937E1DF9DD30A89F8678E22E68A6DA3DBE438B156` and is pinned by `scripts/check_windows_preview_package.py` to source commit `6d8147cf6b6520f9b18bc1ae79170b9dc989b171`. Its PE import table includes Windows system DLLs and `VCRUNTIME140.dll`; therefore this executable is not proven to run from a bare unpacked directory on a clean Windows image. The package documents installation of Microsoft's [latest supported VC++ x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170) by the user. It does not ship or modify Microsoft's runtime files because redistribution is limited to licensed Visual Studio users under the applicable license terms. Individual CRT DLL copying is not the package plan. See [Microsoft redistribution guidance](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files), [C++ deployment](https://learn.microsoft.com/en-us/cpp/windows/deployment-in-visual-cpp?view=msvc-170), and [CRT library options](https://learn.microsoft.com/en-us/cpp/c-runtime-library/crt-library-features?view=msvc-170).

The PE imports show no Node, browser engine, WebView, or OpenSSL DLL. The build uses Windows system APIs and the Windows security stack. This is a static import-table result, not a clean-machine launch test. Lapui uses system fonts and the repository contains no bundled TTF/OTF/WOFF fonts; CJK glyph availability and layout therefore still need a clean Windows check.

## Release gates

- **Notice evidence recorded: `void 1.0.2`; human review pending.** Cargo.lock checksum and cached `.crate` SHA-256 both equal `6a02e4885ed3bc0f2de90ea6dd45ebcbb66dacffe03547fadbb0eeae2770887d`. The crate archive contains only `.gitignore`, `.travis.yml`, `Cargo.toml`, `README.md`, and `src/lib.rs`; no LICENSE or NOTICE file. The pinned upstream source commit is `ab2f4dbb7c95c144ccba2fa8afd11e155aa91133`. Debian's official `rust-void 1.0.2-1` source package identifies the same upstream project and records `Copyright: 2015-2018 Jonathan Reem` for `Files: *` in its [copyright metadata](https://metadata.ftp-master.debian.org/changelogs//main/r/rust-void/rust-void_1.0.2-1_copyright). The exact source file and a complete MIT notice are retained with separate SHA-256 values in `licenses/third_party/distribution/`. The audit verifies both files and reports their exact package mapping, but deliberately remains nonzero until a human reviews this downstream evidence and approves the staged notice wording. Preserve the Debian provenance in the package; this evidence is not an upstream rights-holder confirmation.
- **Documented prerequisite:** users install the official Microsoft x64 VC++ Redistributable when it is not already present. Clean-machine validation must confirm the prerequisite instruction is sufficient and the executable starts; no Microsoft runtime binary is bundled.
- **Missing:** create the staging directory from the explicit contents list and verify every referenced guide, license, notice, and source file is present.
- **Missing:** run the staged package on a clean Windows 11 user environment without Rust, Node, or a system WebView; verify startup, local-files, CJK font fallback, and MCP stdio workflow.
- **Missing:** obtain a successful license audit, review all notices/source obligations, and record the final staged package hash and size. The current package is a developer preview candidate, not a distributable release.
