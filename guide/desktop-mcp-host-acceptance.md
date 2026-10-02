# Desktop MCP host acceptance (Windows / Cursor)

**Status: prepared, not run.** The current machine has Cursor 3.21.16 installed at
`C:\Program Files\cursor\Cursor.exe`; an older per-user Cursor 0.45.14 is also
registered. The newer executable resolves from `cursor` on PATH. It was not
running during preparation, and no project MCP configuration was found. This
document does not claim a desktop-host integration pass or assume an account is
signed in.

The goal is to verify Lapui through the installed desktop host, using a disposable
directory and the authenticated bridge. The bridge keeps the same Lapui window
alive while Cursor's stdio adapter disconnects and reconnects. Cursor's current
MCP guide documents project configuration at `.cursor/mcp.json`, local `stdio`
transport, and per-tool approval; see [Cursor MCP configuration](https://docs.cursor.com/context/model-context-protocol).

## Candidate and isolation

Use the staged developer-preview candidate only for this local acceptance:

- Executable: `target/windows-preview-package-6d8147c/lapui.exe`
- SHA-256: `EB1F48098FC827A3C1DC7D9937E1DF9DD30A89F8678E22E68A6DA3DBE438B156`
- Source commit: `6d8147cf6b6520f9b18bc1ae79170b9dc989b171`

The package is explicitly `not-ready`: the downstream `void` notice still needs
human review, and clean-Windows acceptance remains open. Do not use real personal
directories or files. Create two harmless fixture files in
`C:\LapuiAcceptance\sample`, for example `alpha.txt` and `中文-样例.txt`, with
synthetic contents. Record their names, sizes, SHA-256 values, and the directory
entry list before the run.

## Run

In PowerShell, launch the UI as a separate bridge host. Keep its window open and
do not display or share the bridge descriptor, which contains a local capability
token.

```powershell
$exe = 'C:\Dev\Projects\Rust\lapui\target\windows-preview-package-6d8147c\lapui.exe'
$sample = 'C:\LapuiAcceptance\sample'
$bridgeId = 'cursor-acceptance-20261003'
$descriptor = Join-Path $env:LOCALAPPDATA "lapui\mcp-bridge\$bridgeId.json"
if (Test-Path -LiteralPath $descriptor) {
  throw 'Choose a fresh bridge id; do not reuse or manually edit a stale descriptor.'
}
Start-Process -FilePath $exe -ArgumentList @(
  '--demo', 'local-files', '--directory', $sample,
  '--debug-trace', '--mcp-bridge-id', $bridgeId
)
```

Create a throwaway project at `C:\LapuiAcceptance\cursor-workspace` and put
this file at `C:\LapuiAcceptance\cursor-workspace\.cursor\mcp.json` (create
the `.cursor` directory first):

```json
{
  "mcpServers": {
    "lapui-preview": {
      "command": "C:\\Dev\\Projects\\Rust\\lapui\\target\\windows-preview-package-6d8147c\\lapui.exe",
      "args": ["mcp-stdio", "cursor-acceptance-20261003"]
    }
  }
}
```

Open that throwaway project with the explicit current executable:

```powershell
& 'C:\Program Files\cursor\Cursor.exe' 'C:\LapuiAcceptance\cursor-workspace'
```

Use Cursor's MCP settings to confirm the server connects. If the host requests
project trust, MCP connection, or account sign-in, handle those prompts locally;
do not copy credentials or the bridge descriptor into chat. Leave tool approval
enabled and approve each acceptance call manually.

## Acceptance checks and evidence

1. Confirm `app_describe` and `page_controls` work and the host discovers the
   expected 18 tools. Record the Cursor version, Lapui SHA-256, negotiated
   protocol, and tool count.
2. Ask Cursor to use `local_files.query` on the synthetic fixture. Verify both
   names and sizes appear; no file contents or absolute paths should be returned.
3. Use `local_files.metadata.update` to set a harmless note on one entry. Query
   again and verify the note appears while the file's bytes, hash, and directory
   entries remain unchanged. A retry with the old `expectedFileVersion` should
   fail with `stale_entity`.
4. Disable/re-enable the MCP server in Cursor, or close and reopen Cursor. Verify
   the same Lapui window/process remains open, reconnects through the adapter,
   and returns the same app-owned note and document epoch. Record whether the
   host exposes a reliable server restart action; do not infer reconnection from
   the Python SDK test.
5. Save a redacted screenshot of Cursor's connected MCP panel and the tool
   result, plus before/after fixture hashes and a short pass/fail record. Exclude
   descriptor/token contents and unrelated user data.
6. Close Cursor, then close Lapui and verify its bridge descriptor is removed.
   Delete only the disposable fixture and workspace created for this test.

**Pass requires all checks and evidence above.** Until performed in the desktop
host, M3 desktop-host integration remains unverified; the published Python SDK
smokes do not substitute for this run.
