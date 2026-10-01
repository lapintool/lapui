# Lapui Guide

Lapui explores a small, Rust-based runtime for local interfaces authored with HTML, CSS, and JavaScript. It reuses Blitz for document parsing and rendering, and QuickJS-ng for script execution. A structured action and observation interface is part of the runtime from the beginning so that people, tests, and future AI clients can operate the same application state.

The repository contains an early runtime with a dynamic DOM subset, bounded DOM mutation and layout observers, local modules and timers, network APIs, Vue/React and plain-module examples, CPU/GPU drawing, PNG export, registered Rust actions/operations, resumable application change subscriptions, and structured control/diagnostic access. P1–P3 acceptance remains in progress. The guide describes what the code does today and where the boundaries are; it does not claim browser-level compatibility.

For AI host integration, see the [Model Context Protocol guide](mcp.md).
