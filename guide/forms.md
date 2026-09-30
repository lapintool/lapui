# Form interaction

Lapui's current form bridge supports text values, checkbox/radio checked state, labels, basic focus and keyboard activation. The same click dispatch runs for `element.click()`, `lapui.activate(ref)`, TCP `activate`, and Blitz pointer events. This is a tested application-UI subset, not complete HTML forms.

```js
const agree = document.getElementById('agree');
agree.addEventListener('click', event => {
  // Checkbox state has already toggled when the listener runs.
  if (!applicationAllowsChange()) event.preventDefault();
});
agree.addEventListener('change', () => updateSettings(agree.checked));
```

Checkbox/radio clicks update checked state before click listeners. Cancelling a checkbox click restores the previous value; cancelling a radio click restores the previously selected member if it still belongs to the group. A successful change on a connected control emits non-cancelable `input` then `change`. Clicking an already selected radio does not emit another change. Reentrant `.click()` on the same target is suppressed during dispatch. A checked property write updates native state and `:checked` without rewriting the original `checked` content attribute.

Text-like input and textarea values can be read and written through `.value`. Checkbox/radio option values can also be updated by frameworks without changing checked state; absent option values read as `"on"`. Input button/submit/reset/hidden value writes are supported. AI `fill` remains restricted to supported text-like inputs and textareas. Full input value sanitization and dirty-value rules are not implemented.

Radio groups require the same nonempty name, tree, and form owner. Ownership follows the nearest form or an explicit `form="id"` reference; a missing/invalid explicit target means no owner. Other forms, detached trees, unnamed radios, and same-name checkboxes are independent. Group membership is read from the current DOM on selection; automatic reconciliation after every attribute/tree change and complete HTML dirty-checkedness/reset/clone rules are still outstanding. Labels can refer to a control using `for`, or contain it implicitly. Their activation forwards one click and focus to an enabled input/button/select/textarea, including clicks on ordinary nested label text. It does not forward from an interactive descendant.

`disabled` and Lapui's `aria-disabled="true"` convention make a control unavailable to structured operations. Inputs/buttons/selects/textareas inside a disabled fieldset are disabled except descendants of its first legend; a nested disabled fieldset still applies. JavaScript `.focus()`, click and AI controls use this check, and the native pointer/default editing path is gated as well. Read-only text fields reject AI fill and native text/IME editing while remaining focusable; programmatic `.value` writes are allowed. Full CSS disabled-state propagation, platform clipboard/keybinding details, and physical IME input are not verified.

For focused buttons and checkable inputs, Space arms activation on keydown and activates once on keyup. Repeated Space/Enter presses do not generate duplicate activation; Enter activates a button on keydown. Preventing the relevant key event or moving focus cancels the armed Space action. Radio arrow keys move focus and select the next enabled member of that form-scoped group, wrapping at its ends. Tab/Shift+Tab follow native DOM order while skipping disabled controls, boxless nodes, and negative tabindex. Positive tabindex priority and radio-group tab-stop policy are not implemented.

Semantic snapshots prefer basic `aria-labelledby` text references, then `aria-label`, associated labels, control text/button value, and title. Multiple labels are combined; implicit labels work without a control ID. Password values remain excluded before and after layout. This is not the full accessible-name algorithm or accessibility tree.

Run the bundled example:

```powershell
cargo run --locked -- --html examples/forms-demo/index.html --watch
```

Constraint validation, form submit/reset APIs and navigation, selection APIs, full input sanitization, indeterminate checkboxes, and complete event conformance remain unfinished. The focused checked-state and group behavior follows the relevant [HTML input rules](https://html.spec.whatwg.org/multipage/input.html#radio-button-state-(type=radio)); the runtime deliberately implements only the subset described here.
