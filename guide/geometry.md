# Layout measurements

Elements expose `getBoundingClientRect()`, backed by Blitz's native CSS layout:

```js
const element = document.getElementById('panel');
element.style.width = '50%';
const rect = element.getBoundingClientRect();
console.log(rect.x, rect.y, rect.width, rect.height);
```

The returned `DOMRect` is a mutable snapshot in CSS pixels, with x/y/width/height, top/right/bottom/left, and `toJSON()`. Mutating it does not change the element. Construct `DOMRect` or `DOMRectReadOnly`, or use their `fromRect()` factory; negative dimensions produce the corresponding min/max edges.

A measurement synchronously resolves pending style/layout at the renderer's last sampled CSS animation time. It commits queued text/style writes inside `lapui.batch()` without ending the outer batch. Read-after-write sees those writes, but can split a batch and make repeated read/write loops expensive. Prefer grouping writes, then reading once. Native layout cannot be interrupted by the JS deadline.

Coordinates account for viewport scroll, and viewport scale does not turn CSS pixels into physical pixels. Detached/expired references return zero rectangles. Old snapshots do not update after mutation or resize; query again. Layout may still be unavailable while critical stylesheets load.

A scroll container's own content offset does not move its border rectangle.
Ancestor scroll offsets still move descendant rectangles. Lapui applies this
correction consistently to DOM box rectangles, IntersectionObserver geometry
and AI page bounds; native inline fragments retain their scrolled text origin.
Generated box positions and client/content sizes use unrounded CSS layout
before paint snapping. Measurements describe layout rather than physical pixels.

`getClientRects()` returns a snapshot array of DOMRect values, with `length`, indexes, iteration and `item(index)` (null out of range). Non-atomic inline elements return their native per-line fragments. Boxless, hidden and detached elements return an empty list; a zero-size generated box still has a rectangle. This is not a native DOMRectList class.

This is an initial API. Inline unions, nested positioning/scrolling and transforms follow current Blitz behavior with the generated-box scroll correction above; full browser equivalence is unverified. Offset metrics and [computed styles](computed-styles.md) are available as described below. Native HTML box ResizeObserver and bounded MutationObserver subsets are available; see [observations](observers.md). The [animation example](getting-started.md#run-the-animation-and-measurement-example) demonstrates frame-driven position changes. Neither measurements nor callback completion acknowledge screen presentation.


## Element and viewport scrolling

Elements provide scrollLeft/scrollTop, clientWidth/clientHeight, clientLeft/clientTop and scrollWidth/scrollHeight. Offsets can be fractional; size/border metrics are rounded CSS-pixel integers. Client dimensions describe the padding box excluding borders and native scrollbar space. Content dimensions describe the reachable content extent, rather than the maximum offset. Each query resolves current native layout, including pending batch writes.

```js
const list = document.getElementById('list');
list.scrollTop = 120;
list.scrollBy({ top: 60 });
list.scrollTo({ top: list.scrollHeight }); // clamps to its maximum offset
list.scrollTo(0, 0);
```

scroll/scrollTo and scrollBy accept numeric x/y or an options object with left/top/behavior. An omitted absolute axis is preserved; an omitted relative axis is zero. Non-finite coordinates become zero. The native engine clamps to the target's range, without forwarding unused motion to its ancestor. Programmatic scrolling supports overflow:hidden; overflow:clip stays unscrollable. Detached/boxless elements report zero dimensions and do not scroll. Window scrollX/scrollY, pageXOffset/pageYOffset, innerWidth/innerHeight and scroll/scrollTo/scrollBy target the document element/viewport. document.scrollingElement returns documentElement; quirks-mode body alias behavior is unsupported.

The preview supports immediate scrolling only. auto and instant both use native Instant behavior; auto does not honor CSS scroll-behavior:smooth. Explicit smooth throws NotSupportedError, and invalid behavior throws TypeError. This avoids claiming a separately unverified smooth-animation path. scrollIntoView and scrollend are unsupported.

Changed programmatic offsets coalesce per target until a Promise microtask checkpoint, then dispatch a non-cancelable scroll notification. Element scroll does not bubble; viewport scroll targets the document and bubbles to the window. Unchanged/clamped requests emit no new notification. Registered scroll listeners also receive changed native offsets at rendering updates; window capture listeners share the connected event path. See [observers/window events](observers.md) for bounds and sampling limits. Delivery at a microtask checkpoint differs from browser rendering-task ordering. Reads made after writes see the updated native offset before event delivery.

Run `cargo run --release --locked -- --html examples/scroll-demo/index.html` for a horizontal/vertical list with extent/offset measurements, selection and Home/End controls. Automated tests cover padding/borders, 2x scale, nested offset geometry, clamping, hidden/clip overflow, coalescing, batch writes, multiline fragments and detached/hidden nodes. Physical wheel/scrollbar input and broader RTL/writing-mode/transform/zoom conformance still need verification.

The controlled browser references in `tests/fixtures/layout-reference*.json`
record Edge 154 CSS geometry at matching 100% and 150% device scale for two fixed root sizes, before and after
horizontal/vertical programmatic scrolling. The Rust regression compares eight
boxes and six scroll metrics at 100% and 150% scale. It covers flex gaps and
growth, absolute positioning, border/padding boxes, percentage widths and scroll
translation. Scroll requests align to physical pixels at both scales; Chromium
quantizes other fractional-device-pixel requests differently from Lapui, which
retains native fractional offsets. That difference remains outside this fixture.
It excludes text, native controls, transforms and pixel painting;
passing this fixture does not establish general browser conformance.
Regenerate it with `py -3 tests/update_layout_reference.py --browser <msedge.exe>`
using an installed Edge executable. The generator uses a disposable headless
profile and synthetic local HTML; normal Rust tests need neither Edge nor Python.
Use `--scale 1.5 --output tests/fixtures/layout-reference-150.json` for the
second baseline. Comparisons use a 0.02 CSS-pixel numeric tolerance, without
allowing a whole pixel of drift.


## Offset metrics

Elements provide offsetLeft, offsetTop, offsetWidth, offsetHeight and offsetParent. Positions follow Blitz's native layout-parent/padding-edge calculation; sizes are rounded CSS-pixel border-box dimensions, including native inline-fragment unions. Element/root scroll does not change offsetTop/Left. Body/html, fixed-positioned and boxless elements have a null offsetParent; hidden/detached elements report zero metrics. A native offset parent is exposed through the canonical node wrapper, with normal document-lifetime rules.

These initial metrics pass positioned-container border/padding, 2x scale, scroll, hidden ancestor, fixed/body/root and detach tests, plus selected Floating UI DOM cases. Full browser behavior for table/zoom/transform-containing blocks, non-atomic inline positions and broader writing modes remains unverified. They are layout offsets, not physical screen coordinates.
