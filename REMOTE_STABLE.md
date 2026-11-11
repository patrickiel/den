# Turning remote folders on for everyone

Remote folders are hidden behind `DEN_REMOTE=1` until they're tested. Once they've run fine on Linux, Windows and macOS (see `TODO.md`), remove the switch:

1. **`src/remote.rs`**: delete `ENABLED_ENV` and `enabled()`.
2. **Callers**: drop the check and keep what it guarded:
   - `src/main.rs` (`remote_root`): remove the `if !remote::enabled() { … return None; }` block.
   - `src/keymap.rs` (`if crate::remote::enabled()`): keep the body, unconditionally.
   - `src/frame.rs` (`if crate::remote::enabled()`): same.
   - `src/workspace.rs` (`.when(crate::remote::enabled(), |menu| …)`): apply the closure's items directly.
3. **Check nothing is left**: `grep -rn "DEN_REMOTE\b\|remote::enabled" src crates README.md` should find nothing (`DEN_REMOTE_COMMAND` and `DEN_SERVER_BIN` stay, since they're for testing and development).
4. **`README.md`**: in "Folders on a server", drop "(in the works)", the "switched off until…" sentence and `DEN_REMOTE=1` from the example.
5. **`TODO.md`**: tick the item.
6. `cargo build && cargo test`, try **Open Remote Folder…** and `den ssh://host/path` without the variable, then release with `/release` (minor).
