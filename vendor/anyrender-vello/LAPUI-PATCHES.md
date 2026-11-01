# Lapui patches for AnyRender Vello 0.15.0

Based on registry version 0.15.0, upstream commit
`cf0f14776102ec775af61bcdcaac2ab345f4fafb`. Retains upstream MIT and
Apache-2.0 licenses.

The window renderer counts successive failed surface acquisitions or
presentations and logs failure and recovery transitions. Lapui reads the count
to schedule a bounded retry rather than treating a skipped frame as presented.
The underlying surface helper does not expose the specific acquisition error.
The adapter/backend is logged at startup, and `LAPUI_GPU_AA=area` selects
area antialiasing for large-window diagnostics without changing the default.
Remove this patch when upstream exposes presentation failure and recovery to
the embedding application; verify minimize/restore and transient failures on
the actual GPU window before removing it.
