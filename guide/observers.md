# Size observations and window events

`ResizeObserver` reads the authoritative Blitz layout after animation callbacks and before painting. The CLI supplies this rendering opportunity; `poll()` alone does not. No observation timer, thread or second layout tree is added.

```javascript
const panel = document.getElementById('panel');
const observer = new ResizeObserver((entries, owner) => {
  for (const entry of entries) {
    const {inlineSize, blockSize} = entry.contentBoxSize[0];
    console.log(inlineSize, blockSize);
  }
});
observer.observe(panel); // content-box by default
// observer.unobserve(panel);
// observer.disconnect();
```

The API follows the [Resize Observer processing model](https://drafts.csswg.org/resize-observer/) for selected native HTML boxes. Observe accepts `content-box`, `border-box` and `device-pixel-content-box`; changing the selected box replaces that target's observation. All three size arrays and `contentRect` are supplied regardless of the selected box. Entries retain their target and snapshot values; their attributes are read-only, sizes use ResizeObserverSize, and the one-item arrays are frozen. ContentRect starts at the padding edge and excludes padding/border. CSS fractions are retained. Device sizes currently round content dimensions multiplied by viewport scale; they are not proof of renderer-specific subpixel snapping. The pinned style configuration does not enable CSS `writing-mode`; vertical text and fragmented/SVG observation have not been accepted.

A nonzero initial size is delivered at the next rendering update. The reported size becomes zero when a previously sized target is hidden or detached, and changes again after insertion or showing. Zero-sized, boxless and non-replaced inline elements do not receive an initial size notification. Position-only changes and transforms do not trigger size observation. Repeating observe resets its reported size; unobserve/disconnect remove target ownership. An active observer keeps its callback and targets alive until disconnected, unobserved or its document is retired; release observations when a widget closes.

Callbacks may change deeper elements and receive another delivery in the same update. Changes at or above the current delivery depth are deferred, with an undelivered-notifications diagnostic if changes remain. Exceptions are diagnosed while other callbacks continue. The update has a soft 20 ms check between passes and at most 32 passes, plus a shared 250 ms cooperative JS deadline for its synchronous window/observer notifications. Native layout cannot be interrupted by that deadline. Promise jobs run at the following checkpoint, so browser-complete observer/microtask ordering is not claimed. A runaway callback suspends document scripting until [reload](development.md).

At most 128 active resize observers and 1,024 combined observer-target registrations are allowed per document. Different observers watching the same target each consume a registration. Unobserve/disconnect return capacity; invalid arguments throw TypeError, and capacity exhaustion throws RangeError. `describe.scriptExecutionLimits.renderingObserverLimits` reports the native bounds. They do not bound total native layout memory or whole-frame latency.

## Window listeners and scrolling

`window.addEventListener` / `removeEventListener` share the bridge's listener identity, capture, once, passive and listener-object behavior. Connected DOM paths now include the window after the document, including composedPath. This is a listener API subset; Event/CustomEvent constructors, arbitrary dispatchEvent and complete Window/EventTarget inheritance are not implemented.

Window `resize` is non-bubbling and non-cancelable, delivered when CSS viewport width/height changes. Actual native element scroll offsets are sampled for registered scroll listeners at rendering time. Element scroll does not bubble, but capture listeners can receive it; viewport scroll targets the document and bubbles to the window. JS scroll calls retain their existing coalesced Promise-checkpoint notifications and update sampling baselines to avoid a second identical delivery. Native updates may be coalesced between rendering opportunities. Property-only onscroll handlers on elements without a registered scroll listener are not sampled; physical wheel/scrollbar behavior still requires desktop acceptance.

Up to 1,024 element scroll-listener targets are sampled. The sampling registry uses weak references, so it does not retain unreachable detached nodes. Removing the last registered scroll listener releases its slot. Window/document scroll listeners use the viewport sample.

The window driver spaces both animation and observation opportunities by at least 16.67 ms. Input/resource/resize redraws still paint immediately, with JS notifications deferred if a previous opportunity is too recent. Pending delivery uses the existing event-loop WaitUntil deadline; inactive or occluded windows wait until eligible. This avoids immediate observer-triggered redraw loops, but does not provide monitor VSync or a physical presentation acknowledgement.

## Embedding and the runnable example

Custom/offscreen hosts set the viewport, call `animation_frame()` followed by `rendering_update()`, then resolve/paint using `layout_animation_time()`. The `_at(css_animation_seconds)` variants accept the host's CSS clock. CPU snapshots perform both updates once; they do not wait for arbitrary asynchronous application work.

The [Floating UI example](../examples/floating-demo/README.md) now calls the actual library's default autoUpdate while open and cleans up when closed. Window resizing and the Resize anchor button trigger its own listeners and ResizeObserver callback. Selected viewport/element cases are tested alongside offset/flip/shift. The viewport-only IntersectionObserver subset below does not implement layout-shift detection; animation-frame tracking is opt-in and is not enabled in this example. This does not establish broad component-library compatibility.

## Viewport intersection observations

`IntersectionObserver` is available for viewport-relative layout visibility checks:

```js
const observer = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (entry.isIntersecting) loadPreview(entry.target);
  }
}, { rootMargin: '100px 0px', threshold: [0, 0.5, 1] });
observer.observe(document.querySelector('.preview'));
```

The observer samples Blitz `getBoundingClientRect()` geometry during the existing rendering update. `root` must be `null` (the viewport); element roots throw `NotSupportedError`. `rootMargin` accepts up to four `px` or `%` values; percentages resolve against the viewport width. Initial observations are delivered once, then entries are delivered when intersection state changes or a configured threshold is crossed. Entry rectangles and ratios are CSS-pixel layout measurements, not proof that pixels were shown.

This is a geometric viewport subset. It does not apply overflow clipping from ancestor elements, nested scroll roots, opacity/occlusion, `trackVisibility`, `scrollMargin`, or Intersection Observer v2 visibility checks. An element covered by another element can still report `isIntersecting: true`. The feature is suitable for simple viewport-triggered component behavior, not security, analytics viewability, or complete browser compatibility.

At most 128 observers and 1,024 combined target registrations are allowed per document, with at most 256 thresholds per observer and root-margin values bounded by the reported rendering observer limits. Disconnect or unobserve targets when components close. The existing per-callback script deadline applies; native layout and the overall rendering opportunity are not hard real-time bounded.

## DOM mutation observations

`MutationObserver` batches supported DOM changes and invokes its callback at the next Promise-job checkpoint:

```js
const observer = new MutationObserver((records) => {
  for (const record of records) console.log(record.type, record.target);
});
observer.observe(document.querySelector('#results'), {
  subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'style']
});
```

The current bridge reports attribute/style changes, child insertion/removal/moves, `innerHTML` replacement, text-node changes and text-content replacement through its DOM wrappers. It supports `subtree`, `attributeFilter`, `attributeOldValue`, `characterDataOldValue`, `takeRecords()`, `disconnect()` and reconfiguration of an existing target. Records and node-list snapshots are immutable objects. A single-text-child update may be reported as `characterData` because Lapui can retain that native text node; `lapui.batch()` coalesces text/style reports by target until the batch closes.

Each document allows at most 128 observers, 1,024 observed targets and 4,096 queued records across observers. Excess records are dropped with one `mutation-observer` diagnostic per document. Disconnected subtrees are not observed for subsequent edits, attribute namespaces and transient-subtree delivery are unsupported, and mutations performed directly by native Rust code do not enter this JS observer stream. This subset is useful for app-owned component updates; it is not full MutationObserver conformance or a replacement for the external MCP application/business change feed.
