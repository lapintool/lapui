# Windows developer preview package

This is a staging checklist for the x64 Windows developer preview. It does not describe a finished installer or a production release. The current runtime candidate is built with Rust 1.99.0 from commit `523571d2a6549f410937ec33f55a94ef17267f89`, including built-in screenshot capture and corrected scroll geometry. The source archive and license audit are separately verified against the unchanged `Cargo.lock`; they are not executable build inputs. Older preview packages and long-soak binaries retain their own identities and do not certify this candidate.

Run `py -3 scripts/check_windows_preview_package.py` to verify that the reviewed executable, source archive, license report, notices, and documented package inputs are present. It writes a JSON report under ignored `target/` and exits nonzero while any resource or acceptance gate remains open. Once there are no missing resources or consistency problems, `py -3 scripts/stage_windows_preview_package.py` creates an ignored local review copy at `target/windows-preview-package-523571d/`, with a file-hash manifest and a status file that preserves every open gate. The destination derives from the executable source commit; an older directory is preserved. Use `--refresh` to update an existing copy only after verifying its manifest and every recorded file. Staging does not install prerequisites, launch the app, or make an unreviewed package distributable.

`sourceCommit` identifies the executable build input. `packageCheckoutCommit`
and `packageTrackedChanges` separately identify the checkout used for the guide,
notices and other package files. Each staged file has its own hash. Final local
staging should use a committed, clean tracked checkout; packaging-only changes
do not require rebuilding an identical Rust runtime.

## Smallest useful package

```text
Lapui-preview/
├── lapui.exe
├── README.md
├── README.zh-CN.md
├── THIRD_PARTY.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── PACKAGE-STATUS.json
├── PACKAGE-MANIFEST.json
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

The current 33,582,592-byte x64 executable has SHA-256 `6791A373C7F23AB747FF94B7D3693693E4481E08755DE4548CCC1A2A9997591C` and is pinned by `scripts/check_windows_preview_package.py` to source commit `523571d2a6549f410937ec33f55a94ef17267f89`. Its PE import table includes Windows system DLLs and `VCRUNTIME140.dll`; therefore this executable is not proven to run from a bare unpacked directory on a clean Windows image. The package documents installation of Microsoft's [latest supported VC++ x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170) by the user. It does not ship or modify Microsoft's runtime files because redistribution is limited to licensed Visual Studio users under the applicable license terms. Individual CRT DLL copying is not the package plan. See [Microsoft redistribution guidance](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files), [C++ deployment](https://learn.microsoft.com/en-us/cpp/windows/deployment-in-visual-cpp?view=msvc-170), and [CRT library options](https://learn.microsoft.com/en-us/cpp/c-runtime-library/crt-library-features?view=msvc-170).

The PE imports show no Node, browser engine, WebView, or OpenSSL DLL. The build uses Windows system APIs and the Windows security stack. This is a static import-table result, not a clean-machine launch test. Lapui uses system fonts and the repository contains no bundled TTF/OTF/WOFF fonts; CJK glyph availability and layout therefore still need a clean Windows check.

## Release gates

- **Notice evidence recorded: `void 1.0.2`; human review pending.** Cargo.lock checksum and cached `.crate` SHA-256 both equal `6a02e4885ed3bc0f2de90ea6dd45ebcbb66dacffe03547fadbb0eeae2770887d`. The crate archive contains only `.gitignore`, `.travis.yml`, `Cargo.toml`, `README.md`, and `src/lib.rs`; no LICENSE or NOTICE file. The pinned upstream source commit is `ab2f4dbb7c95c144ccba2fa8afd11e155aa91133`. Debian's official `rust-void 1.0.2-1` source package identifies the same upstream project and records `Copyright: 2015-2018 Jonathan Reem` for `Files: *` in its [copyright metadata](https://metadata.ftp-master.debian.org/changelogs//main/r/rust-void/rust-void_1.0.2-1_copyright). The exact source file and a complete MIT notice are retained with separate SHA-256 values in `licenses/third_party/distribution/`. The audit verifies both files and reports their exact package mapping, but deliberately remains nonzero until a human reviews this downstream evidence and approves the staged notice wording. Preserve the Debian provenance in the package; this evidence is not an upstream rights-holder confirmation.
- **Documented prerequisite:** users install the official Microsoft x64 VC++ Redistributable when it is not already present. Clean-machine validation must confirm the prerequisite instruction is sufficient and the executable starts; no Microsoft runtime binary is bundled.
- **Local candidate validation:** the runtime passed live screenshot CLI capture at 150% DPI, SDK fixture and local-files workflow checks, plus targeted browser-reference/scroll/Floating UI/IntersectionObserver regressions. The separate ignored staging copy preserves open gates and hashes its files; verify that copy before clean-machine acceptance. Prior package-local tests for `6d8147c` remain historical evidence rather than proof for a new executable. This is local validation, not a clean-machine test.
- **Missing:** run the staged package on a clean Windows 11 user environment without Rust, Node, or a system WebView; verify startup, local-files, CJK font fallback, and MCP stdio workflow.
- **Missing:** obtain a successful license audit, review all notices/source obligations, and create the final archive with its hash and size after the acceptance gates close. The current local package is a developer preview candidate, not a distributable release.
