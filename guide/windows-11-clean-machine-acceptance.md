# Clean Windows 11 acceptance (developer preview)

**Status: prepared, not run.** The development machine is not clean: it has Rust,
Node, and the VC++ runtime (Windows 11 Pro `10.0.26200` x64). Windows Sandbox's
optional feature was disabled at last inventory; Hyper-V was enabled, but no
disposable VM existed. No feature enablement, reboot, VM creation, package
install, or clean-image claim is made.

The smallest repeatable target is a disposable Windows 11 Sandbox or a fresh
Windows 11 x64 VM/user image without Rust, Node, or a system WebView. Keep the
candidate marked not-ready until this checklist and the separate human license
review are complete.

## Prepare an offline, disposable input folder

1. Run the package checker and inspect its JSON report. A nonzero exit is
   expected while manual gates remain open, but stop if `missingResources` or
   `problems` is nonempty. Use the current staged directory if it exists; only
   create it when absent. Do not use `--refresh` here because the package guide
   file set may have changed since the candidate was first staged.

   ```powershell
   py -3 scripts/check_windows_preview_package.py
   $report = Get-Content target\windows-preview-package-check.json -Raw | ConvertFrom-Json
   if (@($report.missingResources).Count -or @($report.problems).Count) {
     throw 'Package report has missing resources or consistency problems.'
   }
   $stagePath = Join-Path 'target' ('windows-preview-package-' + $report.releaseCandidate.expectedSource.Substring(0,7))
   $stageRoot = (Resolve-Path $stagePath -ErrorAction SilentlyContinue).Path
   if (-not $stageRoot) {
     py -3 scripts/stage_windows_preview_package.py
     if ($LASTEXITCODE -ne 0) { throw 'Could not stage the reviewed candidate.' }
     $stageRoot = (Resolve-Path $stagePath).Path
   }
   $manifest = Get-Content (Join-Path $stageRoot 'PACKAGE-MANIFEST.json') -Raw | ConvertFrom-Json
   $expected = @($manifest.files | ForEach-Object { $_.path } | Sort-Object)
   $actual = @(Get-ChildItem -LiteralPath $stageRoot -File -Recurse |
     Where-Object { $_.Name -ne 'PACKAGE-MANIFEST.json' } |
     ForEach-Object { $_.FullName.Substring($stageRoot.Length + 1).Replace('\', '/') } |
     Sort-Object)
   if (Compare-Object $expected $actual) { throw 'Staged file set differs from its manifest.' }
   foreach ($entry in $manifest.files) {
     $file = Join-Path $stageRoot $entry.path
     if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $entry.sha256) {
       throw "Staged hash mismatch: $($entry.path)"
     }
   }
   ```

2. Create `C:\LapuiAcceptance\host-input\package`,
   `C:\LapuiAcceptance\host-input\prerequisites`, and
   `C:\LapuiAcceptance\host-input\acceptance\tests`. Copy the staged package
   contents into `package`, and copy `tests/mcp_sdk_v2_smoke.py` plus
   `tests/requirements-mcp-sdk-v2-lock.txt` into `acceptance\tests`.

   ```powershell
   $kit = 'C:\LapuiAcceptance\host-input'
   New-Item -ItemType Directory -Force "$kit\package", "$kit\prerequisites", `
     "$kit\acceptance\tests", "$kit\acceptance\wheelhouse" | Out-Null
   Copy-Item (Join-Path $stageRoot '*') "$kit\package" -Recurse
   Copy-Item 'tests\mcp_sdk_v2_smoke.py', 'tests\requirements-mcp-sdk-v2-lock.txt' `
     "$kit\acceptance\tests"
   ```
3. Place the official x64 VC++ Redistributable installer at
   `C:\LapuiAcceptance\host-input\prerequisites\vc_redist.x64.exe`. Download it
   from Microsoft's [latest supported VC++ Redistributable page](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170).
   Also place a signed Python 3.12 x64 installer from the [official Python for
   Windows downloads](https://www.python.org/downloads/windows/) at
   `prerequisites\python-3.12-amd64.exe`. Verify each Authenticode signature is
   `Valid` and its signer matches Microsoft Corporation or Python Software
   Foundation respectively; record the versions and signature status.
4. Prepare an offline wheelhouse for the exact Windows/Python 3.12 lock:

   ```powershell
   New-Item -ItemType Directory -Force C:\LapuiAcceptance\host-input\acceptance\wheelhouse
   py -3.12 -m pip download --dest C:\LapuiAcceptance\host-input\acceptance\wheelhouse `
     -r tests/requirements-mcp-sdk-v2-lock.txt
   ```

   This keeps guest networking disabled while allowing the official MCP smoke
   to run. Record the wheelhouse file list and hashes. Python and the test client
   are guest-only acceptance tools, not Lapui runtime prerequisites.
5. Confirm the staged executable hash is
   `EB1F48098FC827A3C1DC7D9937E1DF9DD30A89F8678E22E68A6DA3DBE438B156` and note
   the package source commit from `PACKAGE-STATUS.json`.
6. If Windows Sandbox is unavailable, an administrator must enable the Windows
   feature and restart before this test. That host change has not been made.

   ```powershell
   Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All
   Restart-Computer
   ```

   Microsoft's [Sandbox configuration guide](https://learn.microsoft.com/en-us/windows/security/threat-protection/windows-sandbox/windows-sandbox-configure-using-wsb-file)
   documents the `.wsb` format, read-only folder mappings, and networking
   isolation.

The repository includes
[`windows-preview-clean-room.wsb`](../tests/acceptance/windows-preview-clean-room.wsb)
as a locked-down template. It expects the host input folder above to exist and
maps it read-only, disables networking, clipboard redirection, and vGPU, and
assigns 4 GiB. Start it only after the input folder and signed installer are
ready. Do not map the repository or a user profile into the guest.

## Guest checks

1. In Sandbox, verify the OS build/architecture and confirm `rustc`, `cargo`,
   and `node` are unavailable. Install the signed VC++ x64 prerequisite from the
   mapped folder, interactively if Windows requests elevation. Record the
   installed runtime version; do not copy individual CRT DLLs.
2. Create a guest-local fixture at `C:\LapuiAcceptance\sample` containing
   synthetic `alpha.txt` and `中文-样例.txt` files. Record file hashes and sizes.
3. Launch `C:\LapuiAcceptance\input\package\lapui.exe --demo local-files
   --directory C:\LapuiAcceptance\sample`. Verify the native window starts,
   both names render with readable CJK glyphs, resizing does not clip content,
   and the index shows names and sizes without reading file contents.
   Record the guest fixture hashes before and after this visual check, then
   close this window before starting the stdio harness (which opens its own
   window).
4. Install the mapped Python 3.12 x64 installer inside the guest. Create a
   virtual environment and install only from the mapped offline wheelhouse:

   ```powershell
   py -3.12 -m venv C:\LapuiAcceptance\venv
   & C:\LapuiAcceptance\venv\Scripts\python.exe -m pip install --no-index `
     --find-links C:\LapuiAcceptance\input\acceptance\wheelhouse `
     -r C:\LapuiAcceptance\input\acceptance\tests\requirements-mcp-sdk-v2-lock.txt
   ```

   Then run the copied official SDK v2 stdio smoke against the staged
   executable:

   ```powershell
   & C:\LapuiAcceptance\venv\Scripts\python.exe `
     C:\LapuiAcceptance\input\acceptance\tests\mcp_sdk_v2_smoke.py `
     C:\LapuiAcceptance\input\package\lapui.exe
   ```

   The smoke must negotiate protocol
   `2026-07-28`, discover all 18 tools, query the synthetic file, update only
   app-owned metadata, reject stale metadata versions, and prove fixture hashes
   and directory entries remain unchanged. Python and test files are acceptance
   tooling, not application prerequisites.
5. Close the application and Sandbox. The guest is disposable; verify the
   host-input package and installer hashes are unchanged. Capture OS build,
   package hash, runtime version, tool output, and redacted screenshots without
   recording file contents or personal paths.

If Python/SDK tooling cannot be staged without weakening the isolated guest,
mark the clean-machine MCP stdio check blocked/unrun rather than substituting a
host-machine smoke. A successful app launch alone does not close M5.

## Result record

Record each result as `pass`, `fail`, `blocked`, or `not run`; attach the exact
package SHA-256 and source commit. Preserve each open license/runtime gate. The
package remains a local developer-preview candidate until the clean-machine,
MCP, CJK, and human notice checks all pass.
