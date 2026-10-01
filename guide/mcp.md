# Model Context Protocol

Lapui can expose the current local UI over stdio. For sessions that need to
survive a client disconnect, run the UI separately and attach a restartable
stdio adapter to it.

```sh
lapui --html ./index.html --debug-trace --mcp-stdio
```

Configure the host to launch that command using its stdio MCP transport. Keep
all diagnostics on stderr: stdout is reserved for MCP protocol messages. In
MCP mode Lapui does not start the separate loopback control socket.

To keep the UI alive when the MCP host restarts its child process, start the
window separately and assign it a bridge id:

```sh
lapui --demo files --mcp-bridge-id files-demo
```

Configure the MCP host to launch a new adapter process with:

```sh
lapui mcp-stdio files-demo
```

The UI binds an ephemeral `127.0.0.1` port and stores a 256-bit random
capability in the current user's private Lapui state directory. The adapter
authenticates before forwarding MCP bytes. At most four adapters may connect at
once. The descriptor is removed when the UI exits. Bridge ids contain only
ASCII letters, digits, hyphens and underscores. A stale descriptor left by a
forced process kill prevents reuse of that id; confirm the old app has exited
before removing the descriptor. `--mcp-stdio` remains useful for single-session
launches; use bridge mode when a client must reconnect to the same window.

## Trust and data

The MCP connection inherits the trust of the local host that launches the
process; the stdio transport does not add authentication or per-tool user
consent. That host can read visible page text and ordinary control values and
can invoke every action the application registered for the running window.
Password values and values marked with sensitive `autocomplete` tokens
(`current-password`, `new-password`, `one-time-code`, and common payment-card
fields) are omitted from semantic control snapshots. Other field values and
page text are not secret-filtered. Avoid rendering credentials or other secrets
in AI-visible UI content, and only configure a trusted local MCP host.

The initial tool set is deliberately small:

| Tool | Purpose |
| --- | --- |
| `app_describe` | Describe Lapui and the enabled semantic capabilities. |
| `page_controls` | Read the current visible semantic controls and their state. |
| `page_observe` | Read a bounded, paged hierarchy of rendered elements, names, text, bounds, and control state. |
| `page_changes` | Read a bounded cursor-based journal of bridge DOM and control-event changes. |
| `page_wait_for_changes` | Wait from a journal cursor for changes or a resynchronization signal. |
| `page_screenshot` | Return the current viewport as a bounded PNG image block. |
| `page_control` | Activate, fill, check, focus a control, or scroll a rendered element using its current reference and document epoch. |
| `page_reload` | Reload the trusted local document source; prior page references become stale. |
| `page_wait_for_control` | Wait for a visible semantic control to match a bounded value or state condition. |
| `page_wait_for_render` | Wait for the frame causally linked to a control mutation to return from the renderer. |
| `page_cancel_wait` | Cancel an active semantic, render, or page-change wait by its wait ID. |
| `page_diagnostics` | Read script/runtime and network diagnostics. |
| `runtime_memory_usage` | Read QuickJS allocator and heap counters; optionally request cycle collection. |
| `actions_list` | Discover registered business actions, with bounded pagination. |
| `actions_describe` | Read the selected action’s exact input and output schemas. |
| `action_invoke` | Invoke one registered action with retry deduplication and optional stale-version protection. |
| `operation` | Read, wait for, or request cancellation of a registered asynchronous operation. |
| `changes` | Read a baseline or wait for bounded, resumable application changes. |

The tools reuse the existing action catalog and document controller. Page
observation is a pre-order projection of rendered elements with a hard scan,
page, text, and 24 KiB structured-result budget; it filters `hidden`,
`aria-hidden`, `display:none`, and `visibility:hidden` subtrees and never copies
arbitrary attributes. `nextAfter` cursors are only valid against the current
tree, so clients should restart after page mutations. `page_changes` returns a
separate 256-record journal for JavaScript bridge attribute, text, child-list,
input/change events, and programmatic form `value`/`checked` property writes. It
returns canonical document-scoped node references, attribute/property names and
bounded added/removed node references, but never attribute values, control
values, or text. If its cursor has fallen behind retained
history, `resyncRequired` asks the client to take a fresh `page_observe` snapshot
and continue from the returned cursor. This journal does not include DOM edits
performed directly by native Rust code and is not the application-state
`changes` feed. Other IDL property assignments without one of these tracked
form properties or an `input`/`change` event are not journal entries.
With `--debug-trace`, each record also carries a `debugTraceSequence` that can
be passed to `page_wait_for_render` to wait for a causally linked renderer frame.
`page_wait_for_changes` accepts a cursor returned by `page_changes` and waits
for a batch, resynchronization requirement, document reload, or bounded timeout.
It shares the four-active-wait limit and four-second maximum with the other
page waits. A returned batch is still only a semantic change journal; it does
not imply that layout or rendering has completed. A document-local
notification wakes the waiter when the bridge appends a journal record; this is
not a native DOM mutation stream or renderer notification.
`page_wait_for_control` requires exactly one `id` or current `ref`, and exactly
one `equals` or `contains` condition. Supported fields are `value`, `checked`,
`focused`, `enabled`, `name`, and `role`; values are checked against their
semantic type. The wait is bounded to four seconds and shares a four-wait
concurrency limit with render waits. It observes control state only, without
claiming a frame was painted.
`page_wait_for_render` requires `--debug-trace` and the `debugTraceSequence`
returned by a successful `page_control` mutation or a `page_changes` record. It waits for the causally
linked frame and resolved layout to return from the renderer, with a maximum
four-second timeout and at most four concurrent waits. The result does not
confirm physical presentation by the native window or operating system.
`page_cancel_wait` sets a cancellation flag for any active page wait. The wait
returns `wait_cancelled`; cancellation wakes a blocked page-change wait
immediately. Unknown or already-completed IDs return `found=false`.
`runtime_memory_usage` reads QuickJS-ng's `JSMemoryUsage` counters on the UI
thread. It separates QuickJS allocator bytes and heap-estimated bytes from
object, string, array, function, and property counts. These counters do not
include Rust/DOM/renderer allocations, GPU memory, or memory retained by the
process allocator. By default the tool does not trigger collection. Setting
`collectGarbage` explicitly runs QuickJS cycle collection and returns snapshots
before and after; it may pause the UI and does not promise that the OS returns
freed pages to the system.
Screenshot requests paint one rendering opportunity with the software
renderer at the current physical viewport size and scale, without resizing the
window. PNG payloads are capped at 4 MiB; the result identifies CPU rendering
but does not acknowledge presentation by the native window or operating system.
The tools do not expose arbitrary JavaScript evaluation, arbitrary DOM access,
or process discovery. The bridge is the only listener and is authenticated and
loopback-only. The official Python MCP SDK 1.30.0 smoke covers one combined
workflow: Chinese page observation, semantic and causal waits, conflict/draft
recovery, verified rename, scan, reload with stale-ref rejection, adapter
disconnect/reconnect, and recovery of the same operation and changes. This is
one Windows release scenario, not broad platform or long-running reliability
evidence; the adapter remains an early integration.

## Current limitations

The adapter is an early implementation. It uses the MCP Rust SDK and stdio
transport; the SDK smoke covers a representative workflow and reconnecting to
the same running application after a client disconnect. Every tool publishes an
`outputSchema`. `app_describe`, `page_observe`, and `page_changes` have
field-level schemas; other tools currently declare an open JSON object because
page, action, and operation fields vary. Their individual properties are not
yet validated by the advertised schema.
Tools return JSON in both
`structuredContent` and a readable text block; tool errors set `isError` and
include the runtime error code. Action calls require a stable `requestId` and return the
committed version plus action result without copying the full application
state. Writes based on an observed state should pass `expectedVersion`. If a result exceeds the response budget, Lapui returns
`outcome_unknown` with the request ID; inspect state or operation status and
never retry the side effect under a new ID.
