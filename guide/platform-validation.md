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
