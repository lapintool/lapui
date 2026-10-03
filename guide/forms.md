# Form interaction

Windows builds enable the native shell's text clipboard provider for Ctrl+C,
Ctrl+X and Ctrl+V. This applies to default and minimal-feature builds. Clipboard
images are excluded. Unicode text replacement and copy/delete/paste are tested
through a real Windows 11 window at 150% DPI; this does not certify IME candidate
composition, clipboard contention or other platforms.

Tab and Shift+Tab remain navigation in read-only editors, including native
Windows events that carry a tab character in their text payload. Read-only
controls still reject text insertion, paste, cut and IME edits.

Lapui's current form bridge supports text values, checkbox/radio checked state, a basic single-select list, labels, focus, keyboard activation, selected constraint validation, and local submit/reset events. The same click dispatch runs for `element.click()`, `lapui.activate(ref)`, TCP `activate`, and Blitz pointer events. This is a tested application-UI subset, not complete HTML forms.

```js
const agree = document.getElementById('agree');
agree.addEventListener('click', event => {
  // Checkbox state has already toggled when the listener runs.
  if (!applicationAllowsChange()) event.preventDefault();
});
agree.addEventListener('change', () => updateSettings(agree.checked));
```

Checkbox/radio clicks update checked state before click listeners. Cancelling a checkbox click restores the previous value; cancelling a radio click restores the previously selected member if it still belongs to the group. A successful change on a connected control emits non-cancelable `input` then `change`. Clicking an already selected radio does not emit another change. Reentrant `.click()` on the same target is suppressed during dispatch. A checked property write updates native state and `:checked` without rewriting the original `checked` content attribute.

Text-like input and textarea values can be read and written through `.value`. Checkbox/radio option values can also be updated by frameworks without changing checked state; absent option values read as `"on"`. Input button/submit/reset/hidden value writes are supported. For a single `<select>`, `.value`, `.selectedIndex`, `option.selected`, and `option.defaultSelected` expose a bounded DOM subset; options can be selected by keyboard arrows/Home/End, structured `fill`, or activating an option node. Required checks, local reset, string FormData, and semantic snapshots use that selected value. The current Blitz renderer does not paint a native `<select>/<option>` widget; the form submission example therefore provides a visible button listbox backed by the same single-select form state. A native popup widget, multiple selection, optgroup semantics, unmatched value/index writes, full select keyboard behavior, and physical pointer/keyboard verification remain unsupported. Text/textarea current values are kept in the native editor separately from `defaultValue` (the input attribute or textarea text). Programmatic `.value` and user/AI input mark the value dirty; changing the default then preserves the current value. Reset clears that flag. Supported text types remove newlines; email/URL values trim ASCII whitespace, textarea normalizes newlines, and number writes reject nonnumeric text. Invalid/empty input types fall back to text for property writes. Type changes and every browser sanitization edge case are not covered. Explicit control heights are advisable for the current native numeric input sizing; the submission example sets them.

The sample's visible horizontal listbox keeps only the selected option in the Tab order. Left/Right and Home/End update focus and selection together, and the backing `<select>` remains the source of form value and AI-control state. This is example behavior; it does not establish physical keyboard verification or complete listbox accessibility behavior.

Text and textarea controls expose `selectionStart`, `selectionEnd`, `selectionDirection`, and `setSelectionRange()` for text, search, telephone, URL, password, and textarea controls. Offsets use DOM UTF-16 code units and are mapped to the native editor's UTF-8 text positions; ranges that split a surrogate pair are rounded to native character boundaries. Number and other unsupported input types throw `InvalidStateError`. This exposes the native editor selection to scripts; it does not establish physical IME candidate-window placement, clipboard shortcuts, or cross-platform selection behavior. See [desktop input validation](platform-validation.md) for checks that require a real desktop and input method.

Radio groups require the same nonempty name, tree, and form owner. Ownership follows the nearest form or an explicit `form="id"` reference; a missing/invalid explicit target means no owner. Other forms, detached trees, unnamed radios, and same-name checkboxes are independent. Group membership is read from the current DOM on selection; selected dirty-checkedness, reset and clone rules have tests: `.checked` preserves the default attribute, default changes preserve a dirty current state, and cloning copies current values/checkedness without sharing mutable form state. Automatic reconciliation after every tree/attribute change and complete browser cloning/type-transition rules remain outstanding. Labels can refer to a control using `for`, or contain it implicitly. Their activation forwards one click and focus to an enabled input/button/select/textarea, including clicks on ordinary nested label text. It does not forward from an interactive descendant.

`disabled` and Lapui's `aria-disabled="true"` convention make a control unavailable to structured operations. Inputs/buttons/selects/textareas inside a disabled fieldset are disabled except descendants of its first legend; a nested disabled fieldset still applies. JavaScript `.focus()`, click and AI controls use this check, and the native pointer/default editing path is gated as well. Read-only text fields reject AI fill and native text/IME editing while remaining focusable; programmatic `.value` writes are allowed. Full CSS disabled-state propagation, platform clipboard/keybinding details, and physical IME input are not verified.

For focused buttons and checkable inputs, Space arms activation on keydown and activates once on keyup. Repeated Space/Enter presses do not generate duplicate activation; Enter activates a button on keydown. Preventing the relevant key event or moving focus cancels the armed Space action. Radio arrow keys move focus and select the next enabled member of that form-scoped group, wrapping at its ends. Tab/Shift+Tab follow native DOM order while skipping disabled controls, boxless nodes, negative tabindex, and controls hidden by `hidden`, `aria-hidden="true"`, `display:none`, or `visibility:hidden/collapse` on themselves or an ancestor. A radio group contributes only its checked member, or its first eligible member when none is checked. Positive tabindex priority is not implemented.

Semantic snapshots prefer basic `aria-labelledby` text references, then `aria-label`, associated labels, control text/button value, and title. Multiple labels are combined; implicit labels work without a control ID. Password values remain excluded before and after layout. This is not the full accessible-name algorithm or accessibility tree.

Run the bundled example:

```powershell
cargo run --locked -- --html examples/forms-demo/index.html --watch
```

## Local submission and reset

```js
const form = document.getElementById('settings');
form.addEventListener('submit', async event => {
  event.preventDefault();
  const values = [...new FormData(form, event.submitter)];
  await lapui.invoke('settings.save', { values }); // Register this action in Rust.
});
form.requestSubmit(); // Checks constraints, then emits submit with a null submitter.
```

Clicking a submit button, including its ordinary nested text, runs the same constraint checks and submit event. `requestSubmit(button)` checks type/ownership and supplies that wrapper as `event.submitter`. `novalidate` and `formnovalidate` skip checks. Enter in supported text-like inputs clicks the first associated submit button when enabled; with no submit button, a single blocking text input can submit with a null submitter. Canceled/repeated/composing Enter is suppressed in that path. Textareas do not implicitly submit. These are selected defaults, not a complete keyboard/IME model.

Handle submission with `preventDefault()`: uncanceled local submission and `form.submit()` report `NotSupportedError`; navigation/HTTP form transport is absent. Submit events are bridge event objects, not a complete SubmitEvent implementation.

`form.reset()` and reset-button activation emit a cancelable bubbling reset before restoring defaults. Cancellation preserves values. Supported text/textarea, single-select and checkbox/radio fields reset without input/change events; custom validity messages remain. Unsupported date/file/range/color resets fail before mutating supported fields. Recursive submission/reset of the same form is guarded.

The stable `form.elements` view updates from native tree order and ownership, including external `form="id"` controls. Numeric access, item, namedItem, length and iteration are supported. Duplicate-name results provide iteration and a radio value accessor; full RadioNodeList/WebIDL behavior and form named-property access are absent. Ordinary childNodes collections remain snapshots.

## Validation and AI observations

Supported checks include required text/single-select/checkbox/form-scoped radio groups, email (including multiple), absolute URL, whole-value Unicode `v`-flag patterns, user/AI-edited minlength/maxlength, numeric min/max/step and interactive bad input, and `setCustomValidity`. Programmatic writes do not trigger user length checks. `validity` is live; disabled/readonly controls are excluded. Multi-select and unimplemented input types report `NotSupportedError` rather than silently validating.

`checkValidity()` dispatches cancelable, non-bubbling invalid events. `reportValidity()` also focuses the first invalid control whose event was not canceled; it does not display a native validation popup. Messages are basic English strings. Full validity-state, number precision and localized reporting conformance are not claimed.

Both `lapui.controls()` and TCP controls report the same `formRef`, `willValidate`, validity flags, message and sanitized current value for supported controls. They omit controls under `hidden`, `aria-hidden="true"`, computed `display:none`, and `visibility:hidden/collapse` on the control or an ancestor; reads resolve pending style/layout changes first. Password values and validation messages are omitted. Unsupported validation is marked `validationAvailable: false`. While scripting is suspended, TCP returns the native semantic snapshot with a top-level `validationAvailable: false`; reload is required to resume checks. This is a selected semantic projection, not a full accessibility tree or browser :valid/:invalid styling implementation.

## String FormData and bounds

`new FormData(form, submitter)` collects enabled named supported controls, checked options, a single-select value and the selected submit button, then emits formdata so handlers can add entries. Duplicate names/order are preserved. append/set/delete/get/getAll/has, iterators and forEach are available. Unpaired surrogates become Unicode replacement characters. File/image/multiple-select entries, Blob values, filename arguments and multipart fetch encoding are unsupported; FormData is string-only.

The advertised `describe.scriptExecutionLimits.formLimits` are 1,024 pattern code units, 65,536 pattern-value code units, 4,096 custom-message code units, and 1,024 FormData entries with 2,097,152 combined name/value code units (UTF-16). Over-limit FormData mutations reject before replacing entries. These limits do not bound all transient/native allocations or guarantee interruptible regex/native calls. Script execution retains its cooperative deadline and heap limits.

Run the complete local form example without Node:

```powershell
cargo run --release --locked -- --html examples/form-submit-demo/index.html --watch
```

Its submit handler records FormData and calls the existing Rust counter action. Reset restores the form while preserving submission history/backend state. The focused behavior follows the relevant [HTML form-control infrastructure](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html); Lapui implements only the subset described and tested here.
