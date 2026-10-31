# Local Windows preview packages

The package tools assemble an unfinished **Windows 11 x64 local preview**, not a
public release. They do not change Git remotes, upload artifacts, install a
runtime, or mark the unresolved compatibility/platform gates complete.

## Build

Use Python 3.11+, the native Rust MSVC toolchain and a licensed Visual Studio
installation's **release x64 Redist** runtime. The current binary imports
`VCRUNTIME140.dll`. Do not take a debug DLL or copy one from System32.

Save the applicable Microsoft runtime terms from its
[license directory](https://visualstudio.microsoft.com/license-terms/).
Then, from a clean reviewed source checkout:

```powershell
python tools/package_preview.py `
  --vc-runtime "C:/path/to/VC/Redist/MSVC/version/x64/Microsoft.VC145.CRT/vcruntime140.dll" `
  --vc-license "C:/path/to/saved-runtime-terms.docx"
```

The tool builds `lapui` with locked default features and emits, under
`target/packages/`:

- A portable folder and runtime ZIP with the exe, selected DLL, prebuilt examples,
  launchers, guide sources, original project licenses and dependency notices.
- A matching `*-sources.zip` with public project files and the exact Windows
  normal/build dependency Cargo archives. Build-only packages are included
  conservatively; the inventory is not an exact linker contribution report.
- SHA-256 sums for both archives. The runtime manifest also records each file
  hash except itself, the source revision, toolchain, imported DLLs, source
  companion hash and uncompressed runtime footprint, including its manifest.

The asset policy is `tools/preview-assets.json`. Plans under `docs/`, caches,
node_modules, credential paths and generated output are excluded. The default
rejects a dirty checkout and existing output names. During tool development only,
`--allow-dirty --skip-build` can assemble an existing binary; both facts are
recorded and must not be described as a clean freshly built candidate.

Cargo archives must match `Cargo.lock`; their regular files are compared with
the local unpacked registry sources to detect unrecorded patches. Notice files
are copied without rewriting their terms. Source companions retain all selected
upstream archives, including MPL-covered sources and embedded notices. They are
source availability artifacts, not a ready-made offline Cargo/npm vendor tree.

The app-local VC runtime keeps its Microsoft signature and supplied terms. Its
version/hash and source Redist family are recorded separately from open-source
components. Microsoft documents app-local deployment but recommends centrally
serviced deployment for most applications; local copies must be updated by the
application's distributor. See [Microsoft deployment methods](https://learn.microsoft.com/en-us/cpp/windows/choosing-a-deployment-method?view=msvc-170).
The runtime terms and the builder's Visual Studio redistribution rights are
separate from Lapui's license. The package inventory is not a completed public
release/license approval. Source-form availability and notices also matter for
[MPL components](https://www.mozilla.org/en-US/MPL/2.0/).

System Windows DLLs/UCRT, fonts and graphics drivers remain platform dependencies.
The portable examples start without Rust, Node, Python or a WebView; the optional
network fixture server separately requires Python. Framework bundles are already
built. Their npm lockfiles and licenses accompany the examples.

## Verify the artifact

```powershell
python tools/verify_preview.py target/packages/lapui-version-dev-revision-windows-x64.zip
```

Keep the matching source ZIP next to the runtime ZIP. The verifier checks archive
and source hashes, extracts into a temporary path with spaces, changes to an
unrelated working directory, and gives child apps only Windows directories on
PATH. It exports the file-tool/React/Floating examples, tests the Vue `.cmd`
launcher, initializes a small stdio MCP probe, obtains real controls and a current
window PNG, and inspects **its own process** to confirm the packaged VC runtime
was loaded. The probe is not a replacement for the official SDK interoperability
workflow. Its child process is terminated after testing; this is not graceful
window-close evidence.

The JSON report names the exact ZIP hashes and records `cleanMachineTested:false`
and `physicalInputTested:false`. A restricted PATH on the development machine
does not remove installed fonts, OS components, drivers or user state. A fresh
machine/user acceptance run remains required. Browser geometry failures and
other [current limits](limitations.md) remain unresolved regardless of this smoke.

Run the archive/privacy failure-path checks with `python tools/test_preview.py`.
