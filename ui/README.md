# ui/ — external user interface (reserved)

The UI is a **separate application** with its own toolchain (e.g. React Native,
Qt, or WinUI 3). It is intentionally not part of the Rust Cargo workspace and is
not built by `cargo`.

## Engine contract

The UI talks to the Rust **engine** (`../engine`) through a machine-readable
contract, never by importing Rust code directly:

- **Today:** the `disk_organizer` CLI runs one-shot and emits a JSON array of
  classified items to stdout. The UI invokes the binary and parses that JSON.
- **Future:** a long-lived `engine/src/bin/server.rs` will expose a local
  JSON-RPC / WebSocket endpoint, adding streaming progress and interactive
  commands (scan, classify, delete) on top of the same data model.

## Status

This directory is a placeholder reserved for that app. No UI code lives here yet.
