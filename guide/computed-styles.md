# Computed styles

`getComputedStyle(element)` reads the live CSS cascade from the same pinned Stylo styles and Blitz document used to draw the interface:

```js
const element = document.getElementById('panel');
const computed = getComputedStyle(element);
console.log(computed.width, computed.backgroundColor);
element.classList.add('compact');
console.log(computed.width); // queries the new cascade/layout
console.log(computed.getPropertyValue('--accent'));
```

The returned view is read-only and retains its owner element. Property reads resolve pending style/layout and commit queued writes inside `lapui.batch()` without ending the batch. They can split a write batch and make repeated read/write loops expensive. Detached/expired elements return empty values and an empty property list; reinserting a retained element makes the same view readable again. Unreachable computed views do not keep native subtrees alive indefinitely.

Camel-case and kebab-case access, cssFloat, getPropertyValue, getPropertyPriority, length, numeric indexes, item and iteration are available. Enabled Stylo longhands are listed in sorted order, followed by non-invalid custom properties; their presence is not proof that Lapui paints every associated CSS feature. Custom property names retain case. Unknown, disabled or over-1,024-byte property names return an empty string. getPropertyPriority and cssText return empty strings, and parentRule is null. Writes, deletion, defineProperty, setProperty and removeProperty throw NoModificationAllowedError; this is stricter than a browser's expando-object behavior. CSSStyleDeclaration/CSSStyleProperties type checks recognize preview style views; constructors are not public factories. Their WebIDL/prototype behavior is not a full browser implementation.

The API follows the [CSSOM live/read-only model](https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle), with a narrower resolved-value implementation. Stylo serializes longhands, custom properties, colors and eligible shorthands; no independent CSS parser is added. Width/height and physical/logical padding/margin longhands use native unrounded layout for eligible generated boxes. Width/height respect content-box/border-box and remain CSS pixels at different viewport scales. Non-atomic inline or boxless elements keep their computed values for these reads. Shorthands currently use Stylo's computed serialization, so a percentage padding shorthand may differ from its used-pixel longhands. Used insets, transforms, all animation/line-height cases, zoom, RTL/writing modes and full CSSOM conformance remain unverified.

The element argument must be an Element. Null/empty pseudo arguments are accepted; other pseudo-element queries throw NotSupportedError, rather than returning the element's style silently. Pseudo styles, rule/sheet editing and Typed OM are unsupported. Native layout/serialization cannot be preempted by QuickJS's cooperative interrupt handler, and returned styles do not acknowledge screen presentation.

The [Floating UI example](../examples/floating-demo/README.md) exercises actual third-party DOM code against these APIs, bounding rectangles and offset parents. Its included bundle runs without Node.js and covers selected offset/flip/shift cases; its own autoUpdate follows selected viewport/anchor-size changes via [observers/window events](observers.md). Layout-shift and broader component compatibility remain unverified.
