# Desktop input and platform validation

Automated Rust tests and CPU document images verify DOM, layout and event routing. They do not exercise a real platform input method or its candidate window. The following checks record those separate desktop behaviors; passing them does not imply full browser input compatibility.

On a desktop with a Chinese input method, run:

```powershell
cargo run --release --locked -- --html examples/forms-demo/index.html
```

Unlock the draft field using the checkbox in the first legend. Focus the draft, compose Chinese text, change candidates and commit several times. Check that preedit text appears at the caret, the candidate window follows the field, each commit inserts the intended text once, and subsequent edits keep the caret in the expected place. Record whether selection replacement and Backspace work. Repeat after resizing and after switching between the app and another window. Attempt the same input in the read-only field; it should remain unchanged.

Use Tab/Shift+Tab and Space/Enter to move focus and activate controls. Verify disabled inputs are skipped, the two same-name radio groups stay independent, arrow keys move within one group, and pointer shapes return correctly after hovering text and buttons. These checks need physical input; a TCP activation test proves a different path.

For a cross-platform report, include OS/version, Rust version, window backend (Windows, X11, Wayland or macOS), renderer and input-method name. Run `cargo test --locked`, `cargo test --locked --no-default-features` and `cargo clippy --locked --all-targets -- -D warnings`; then test the form, Vue, React and file-tool windows using both human input and structured control calls. Compare behavior rather than exact pixels because installed fonts differ.

Windows and Ubuntu 24.04/WSLg X11 have passed automated tests and CPU window/TCP form, reload and script-timeout recovery scenarios. Current WSLg Wayland startup failed; macOS and physical Chinese IME behavior remain unverified. The unexecuted GitHub workflow is a test configuration, not platform acceptance evidence. Long-running resource and frame-latency measurements are also separate work; see [rendering](rendering.md).

A Windows release and Linux X11 window/TCP stream scenario also passed: 100 ordered SSE updates, text and typed-view WebSocket sends, managed binary replies, send-then-close ordering, a ninth-stream rejection and full native slot/queue-budget reclamation. `networkStatus` and its CLI were checked through the real controller; diagnostics were empty. This is a structured-control and transport verification, not physical-input or frame-latency acceptance.


Windows release and Linux X11 CPU windows also passed structured animation controls: shared-clock movement, pause/resume, completion stopping callbacks and full reload retiring the old callbacks. The native JS callback driver caps batches at a soft 60 Hz maximum; an uncontrolled Windows sample had over 2,000 callbacks in three seconds before this cap. Geometry and fixed-state CPU pixels were also checked. This is not monitor-VSync, measured frame latency or a physical-input claim.

The scrollable-list window/TCP scenario passed on Windows release and Linux X11: horizontal/vertical clamping, updated descendant coordinates, Home/End, row selection, reload and stale-reference rejection. Both platforms reported the same client/content metrics and maximum offsets for the selected scenario; diagnostics were empty. Physical wheel/scrollbar use remains unverified.

The actual Floating UI DOM 1.8.0 bundle and its default autoUpdate also passed in Windows release and Linux X11 CPU windows: centered placement, bottom-right flip to `top-end`, left-edge shift, automatic anchor-size changes, eight repeated size toggles, native OS-window resizing without Reposition, accept/close, reload and stale-reference rejection. The verification script resized only its own process window through Win32/X11. Both platforms moved from x=468 to x=470 after widening the anchor; narrowed-window coordinates differed because Windows outer dimensions and X11 client dimensions differ. Containment and diagnostics passed on both. This is native window/structured-control evidence; layout-shift tracking, physical pointer/IME behavior and general observer conformance remain unverified.

The local form submission window/TCP scenario passed on Windows release and Linux
X11 debug: user-length/email/required-checkbox failures prevented the Rust action,
reporting focused the invalid control, valid FormData and radio/textarea values
reached the handler, reset restored defaults without resetting backend history,
and document reload rejected stale references. The same Rust counter reached two
through successful submissions; diagnostics were empty. The public example uses
explicit text/number field heights; complete browser intrinsic control sizing and
physical typing/IME behavior remain unverified. An exported CPU image was also
inspected with all initial fields and actions visible.

The Windows release CPU file-tool window also passed its structured-control smoke:
an AI rename preserved the human draft, the stale save reported a conflict, refresh
allowed the user's new name to save, retry reused the same scan operation, and the
final file/action state matched the expected versions with six trace records and no
diagnostics. These calls use the local TCP control interface; they do not verify
physical typing, pointer input, or screen presentation.

The same smoke also exercised `waitForControl` cancellation: a deliberately
unmatchable condition was canceled through `cancelWait`, and the waiting client
returned `wait_cancelled`. A control-triggered `waitForRender` returned only after
the causally linked layout resolved and the renderer returned; physical presentation
remained unknown.

The single-select follow-up passed in a Windows release window. A visible button
listbox updates a hidden single-select form value through the same JS/default path;
the control snapshot reports option roles and selected state, form submission
includes the selected value, and the Rust counter action completes with no script
diagnostics. The bundled offscreen image was visually inspected at 900×1000 and the
listbox is visible with its selected option and no clipping. This is structured
control and CPU snapshot evidence, not physical mouse/keyboard or native popup
verification.
