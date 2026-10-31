# Lapui Windows developer preview

This is a temporary, unfinished local development preview for Windows 11 x64.
It is not a production release. Keep the whole extracted folder together.

Double-click `run-files.cmd` for the in-memory file tool. It does not rename disk
files. Use `run-vue.cmd`, `run-react.cmd`, `run-forms.cmd` or `run-floating.cmd`
for the bundled UI examples. The scripts resolve assets beside themselves and
work when the folder name contains spaces. Additional CLI arguments are passed
through, for example `run-vue.cmd --watch`.

Rust, Node.js, Python and a system WebView are not needed to run these examples.
The optional network-demo fixture server is a separate Python development tool.
System fonts supply glyphs; keep the Windows CJK fonts installed for Chinese
text. This archive does not redistribute system fonts or graphics drivers.

The default renderer is CPU. Use `lapui.exe --renderer gpu` only for the optional
GPU path, which still has unresolved resource and platform acceptance work.
Export the embedded file tool without a window:

```powershell
.\lapui.exe --demo files --snapshot files.png --width 960 --height 640
```

For an MCP client, edit the absolute executable path in
`mcp-config.example.json`. That configuration launches the real embedded file
tool over stdio, with no additional background server. See `guide/mcp.md` for
capability boundaries and reconnectable sessions.

## Current limitations

The basic browser fixture matrix passes its 14 selected cases. The additional
Vue/React/Floating UI matrix completes its behavior checks but still fails
strict reference geometry due to line metrics, default control sizes and
scrollbar space. Physical IME/candidate windows, pointer/wheel behavior, both DPI
scales, a fresh Windows installation and the complete long-run comparison
matrix remain unverified. See `guide/limitations.md` and `guide/compatibility.md`.

## Package records and sources

`preview-manifest.json` records every included file hash except itself, the
source revision, build options, required DLLs and the matching source archive.
The sibling `*-sources.zip` contains the public project sources and exact Cargo
registry archives for the selected Windows normal/build dependency graph.
`third-party/inventory.json` records package licenses, notices and archive hashes.
Retain the source companion and source-availability notice together with this
preview. The inventory includes build dependencies; it is not a list of only
the code that the linker retained.

Microsoft's application-local runtime DLL is proprietary and is not covered by
Lapui's MIT/Apache choice. Its supplied terms and provenance are under
`third-party/microsoft/`. App-local copies must be serviced when the runtime is
updated; see the linked Microsoft deployment guidance there.

A local smoke report tests the exact archive hashes after extraction with a
restricted PATH and records the DLL loaded by its own child process. That is
not evidence of a clean OS, a different user account, physical input or a signed
public release. This package is neither published nor an acceptance claim for
the unfinished gates.
