# Spacepeek

Space-bar file previewer for Windows Explorer (like macOS Quick Look).

## What it does

Select a file in File Explorer, press Space, and a single preview window shows its contents. Press Space or Esc to close it.

## Status

First vertical slice. The preview core (bounded reads, binary detection, allow-list, cancellation, state machine, key interception decisions) is implemented and unit-tested in `src/lib.rs`. The Windows runtime (low-level keyboard hook, preview window, Explorer interop) is not yet implemented; `cargo build` and `cargo test` pass on Linux for the platform-independent core.

## Building

```bash
cargo build --release
```

Supported target: **Windows 11 only** (x86_64). No compatibility code for older Windows versions.

## Running

Run `spacepeek.exe` on Windows. On Linux/macOS the binary prints "Spacepeek runs on Windows only" and exits with code 1.

## Manual testing steps (Windows 11)

1. Build and run `spacepeek.exe`.
2. Open File Explorer (`CabinetWClass`).
3. Select a text file (`.txt`, `.md`, `.log`, `.json`, `.toml`, `.yaml`, `.yml`, `.ini`, `.rs`, `.py`, `.js`, `.ts`, `.c`, `.h`, `.cpp`, `.cs`, `.xml`, `.html`, `.css`, `.sh`, `.ps1`, `.bat`).
4. Press Space. The preview window should appear showing the file content.
5. Press Space or Esc to close the window.

## Automated tests

```bash
cargo test
```

## Limitations

- First slice only; Windows runtime (hook + window) not yet wired up.
- No process isolation/sandbox yet (parsing runs in-process, mitigated only by bounded reads, allow-list, no execution).
- Low-level keyboard hook means Space interception is global-hook based.
- Only first selected item.
- Text only; binary files show error.
- No timings claimed.

## License

MIT
