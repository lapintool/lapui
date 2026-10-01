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

The active timer limit is 1,024 per document, and native expiry delivery is bounded to 1,024 entries. Delays are truncated to integer milliseconds and clamped to 1–2,147,483,647 ms; non-finite delays use 1 ms. Clear functions share the timer-ID space. Intervals reschedule after their callback returns, so they can drift; missed ticks are not replayed in a burst. A callback can clear its own interval. String callbacks are unsupported and throw `TypeError`; capacity exhaustion throws `RangeError`. Browser nesting clamps, background throttling and exact browser timer ordering remain unsupported.

Timer callback exceptions are logged and retained in `lapui.diagnostics()` with a `timer:<id>` source. An interval continues after an exception until cleared. `queueMicrotask` uses the QuickJS Promise job queue and accepts a function only; this is not a complete global error/rejection reporting API.

Promise checkpoints have a 1,024-job budget and a soft 20 ms slice checked between jobs. Pending jobs resume on a subsequent UI poll and wake the document again when more work remains. This prevents a finite chain from being stranded at the budget boundary; one running job may exceed the slice up to its cooperative execution deadline. An idle document with no queued work does not reschedule itself.

## Animation callbacks

`requestAnimationFrame()` and `cancelAnimationFrame()` use a separate callback-ID space:

```js
let started;
function move(timestamp) {
  started ??= timestamp;
  const progress = Math.min(1, (timestamp - started) / 1000);
  document.getElementById('marker').style.left = (80 * progress) + '%';
  if (progress < 1) requestAnimationFrame(move);
}
const handle = requestAnimationFrame(move);
// cancelAnimationFrame(handle);
```

The CLI uses `lapui::shell::LapuiApplication`, wrapping Blitz's window application. On an active, visible window's actual RedrawRequested event, callbacks run before Blitz resolves layout and paints. Window, renderer, OS-event and lifecycle handling remain delegated to Blitz. No animation timer or worker starts. A soft 60 Hz maximum spaces JS callback batches at least 16.67 ms apart per window. The event loop waits for the next deadline while callbacks remain; it does not replay missed batches. Callback requests made during a frame wait for that deadline instead of requesting immediate redraw. Input/resize/resource events still paint immediately. With no pending callbacks or rendering notifications, the driver's animation deadline is removed and the prior event-loop policy is restored. Embedders' Poll policy and earlier independent wake deadlines are preserved. Occluded/inactive windows retain callbacks until their next eligible redraw. This is not monitor VSync or a guaranteed 60 frames/second; OS/renderer delays and expensive work lower the delivered rate. CSS animations remain paced by Blitz. Broader platform visibility/pacing and frame-latency acceptance remain outstanding.

Existing requests run in registration order; cancellation can remove a later callback in the batch. New requests, including those made from Promise checkpoints, wait for a subsequent opportunity. Delivered callbacks share the batch timestamp; `performance.now()` can advance between them. A Promise checkpoint follows each callback, and exceptions enter diagnostics while other callbacks continue. Requests are removed before invocation so a one-shot callback can schedule its successor.

At most 1,024 callbacks may be pending. Non-functions throw TypeError; capacity/ID exhaustion throws RangeError. A soft 20 ms slice is checked between callbacks, deferring remaining requests. This differs from unrestricted browser processing. Individual callbacks have the 250 ms cooperative deadline; native work can exceed it, so there is no hard whole-frame guarantee. Cancellation, drop, reload and script suspension retire native requests. Suspended JS callback objects remain subject to the managed-heap limit until reload/drop.

`performance.now()` is a monotonic millisecond clock relative to document creation; `performance.timeOrigin` supplies its initial Unix-millisecond origin. Reload creates a new origin. Other Performance APIs are unsupported. JS timing, native layout/render time and screen presentation are separate: callback completion is not a presentation acknowledgement.

Rust window embedders should use LapuiApplication instead of bare BlitzApplication. A custom/offscreen loop establishes the viewport, calls `LapuiDocument::animation_frame()` followed by `rendering_update()`, then resolves/paints using `layout_animation_time()` (seconds). Alternatively `animation_frame_at(css_animation_seconds)` accepts the renderer's CSS animation clock while keeping JS timestamps on the document clock. The driver also delivers [size observations and window events](observers.md) within its paced opportunities. CPU snapshots supply one opportunity before painting; a recursively scheduled animation is not completed by one export. See [layout measurements](geometry.md).

## Execution limits and recovery

The preview limits QuickJS-managed heap allocations to 128 MiB. Each startup script/module entry has a two-second cooperative deadline; UI event/control calls, timer/animation callbacks, host completion dispatch and individual Promise jobs have 250 ms deadlines. Each synchronous window/observer rendering update shares a 250 ms deadline. Nested bridge calls share the outer deadline. `describe` exposes these values in `scriptExecutionLimits`. These are per-entry limits, not a whole-frame latency budget, process-memory cap, or sandbox. Native Rust calls, filesystem reads, parsing and other operations that do not poll QuickJS's interrupt handler cannot be preempted by this mechanism.

On a time-limit interrupt, scripting is suspended for the whole document. QuickJS's uncatchable interrupt skips JavaScript `catch` and `finally`, so continuing the partially unwound bridge would be unsafe. The runtime stops its timers and document-owned network work, rejects new host work and skips queued host calls (already running Rust handlers must finish), skips queued JS completions and jobs, and retains native control observations and diagnostics. Effects already applied to the DOM or Rust application state are not rolled back. Application-scoped background operations retain their separate lifecycle. Heap exhaustion is an allocation error and can prevent diagnostic allocations; it is not the same as a time-limit suspension.

The triggering structured control request returns `script_error`; later control mutations return `script_suspended`. TCP `diagnostics` reports `scriptStatus: "suspended"` with a bounded error record. Fix the source and use manual or watched [full reload](development.md). The replacement creates a new QuickJS runtime/epoch while preserving Rust application state. Reloading the same runaway source can suspend it again. Closing/resizing the OS window and the independent reload/control server remain available, subject to native host work returning.

Regression tests interrupt startup scripts, click handlers, native event dispatch, timers, Promise jobs and host-render notifications, then recover a local document by reload. Another test verifies oversized ArrayBuffer allocation is rejected. Long-duration application responsiveness, compilation limits, per-frame fairness and physical desktop input remain separate acceptance work.
