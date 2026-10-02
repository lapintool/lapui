# Large list fixture

This offline example indexes 1,000 stable records and renders a sliding window
of up to 100 DOM rows inside a bounded-height scroll viewport. Filtering updates
the virtual window in place. It is intended for repeatable layout and
conditional-update checks; the scroll viewport itself shows fewer than 100 rows
at a normal desktop size.

```powershell
cargo run --release --locked -- --html examples/list-stress-demo/index.html
```
