# Task scheduling

The JavaScript bridge provides function-based `setTimeout`, `setInterval`, their clear functions, and `queueMicrotask`:

```js
const timer = setTimeout((message) => {
  document.getElementById('status').textContent = message;
}, 50, 'Ready');
clearTimeout(timer);

const interval = setInterval(() => refreshStatus(), 1000);
clearInterval(interval);
queueMicrotask(() => flushPendingChanges());
```

Timers run JavaScript on the document's UI thread. One sleeping scheduler worker maintains native deadlines; it waits for the next deadline or a scheduling change, without continuously polling. Timer callbacks run through the UI wake/completion path, with a Promise microtask checkpoint after each callback. Closing the document stops its scheduler and releases pending deadlines.

The active timer limit is 1,024 per document, and native expiry delivery is bounded to 1,024 entries. Delays are truncated to integer milliseconds and clamped to 1–2,147,483,647 ms; non-finite delays use 1 ms. Clear functions share the timer-ID space. Intervals reschedule after their callback returns, so they can drift; missed ticks are not replayed in a burst. A callback can clear its own interval. String callbacks are unsupported and throw `TypeError`; capacity exhaustion throws `RangeError`. Animation-frame scheduling, browser nesting clamps, background throttling and exact browser timer ordering remain unsupported.

Timer callback exceptions are logged and retained in `lapui.diagnostics()` with a `timer:<id>` source. An interval continues after an exception until cleared. `queueMicrotask` uses the QuickJS Promise job queue and accepts a function only; this is not a complete global error/rejection reporting API.

Promise checkpoints have a 1,024-job budget and a soft 20 ms slice checked between jobs. Pending jobs resume on a subsequent UI poll and wake the document again when more work remains. This prevents a finite chain from being stranded at the budget boundary; one running job may exceed the slice up to its cooperative execution deadline. An idle document with no queued work does not reschedule itself.

## Execution limits and recovery

The preview limits QuickJS-managed heap allocations to 128 MiB. Each startup script/module entry has a two-second cooperative deadline; UI event/control calls, timer callbacks, host completion dispatch and individual Promise jobs have 250 ms deadlines. Nested bridge calls share the outer deadline. `describe` exposes these values in `scriptExecutionLimits`. These are per-entry limits, not a whole-frame latency budget, process-memory cap, or sandbox. Native Rust calls, filesystem reads, parsing and other operations that do not poll QuickJS's interrupt handler cannot be preempted by this mechanism.

On a time-limit interrupt, scripting is suspended for the whole document. QuickJS's uncatchable interrupt skips JavaScript `catch` and `finally`, so continuing the partially unwound bridge would be unsafe. The runtime stops its timers and document-owned network work, rejects new host work and skips queued host calls (already running Rust handlers must finish), skips queued JS completions and jobs, and retains native control observations and diagnostics. Effects already applied to the DOM or Rust application state are not rolled back. Application-scoped background operations retain their separate lifecycle. Heap exhaustion is an allocation error and can prevent diagnostic allocations; it is not the same as a time-limit suspension.

The triggering structured control request returns `script_error`; later control mutations return `script_suspended`. TCP `diagnostics` reports `scriptStatus: "suspended"` with a bounded error record. Fix the source and use manual or watched [full reload](development.md). The replacement creates a new QuickJS runtime/epoch while preserving Rust application state. Reloading the same runaway source can suspend it again. Closing/resizing the OS window and the independent reload/control server remain available, subject to native host work returning.

Regression tests interrupt startup scripts, click handlers, native event dispatch, timers, Promise jobs and host-render notifications, then recover a local document by reload. Another test verifies oversized ArrayBuffer allocation is rejected. Long-duration application responsiveness, compilation limits, per-frame fairness and physical desktop input remain separate acceptance work.
