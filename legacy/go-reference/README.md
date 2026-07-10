# Legacy Go Reference

This directory contains copied Hive-specific Go code from the former MindWeaver
implementation. It is an oracle/reference for the Rust port, not the active build
target for this repository.

Active Rust targets live under:

- `crates/hive`
- `crates/hive-core`
- `crates/hive-server`

Use this reference to verify protocol behavior, conflict resolution semantics,
CLI flags, and DB/client migration details while porting.
