# Local form example

Run from the repository root without Node or a bundler:

```powershell
cargo run --release --locked -- --html examples/form-submit-demo/index.html --watch
```

Edit the required name/email/quantity, toggle agreement and delivery, and submit.
The handler prevents navigation, reads string-only FormData, and calls the existing
Rust `counter.increment` action. Validate focuses the first uncanceled invalid
field; Reset restores input defaults and the textarea's original text. Submission
history and Rust state are outside the form and survive Reset.

AI `controls`, `fill`, `check` and `activate` use these same values, constraints,
and events. Invalid fields expose validity flags and their owning form reference.
See [supported form semantics and bounds](../../guide/forms.md) for limitations.
