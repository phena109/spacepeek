# Spacepeek

Space-bar file preview for Windows Explorer. Select a file, press Space, and a single topmost window shows a bounded text preview. Press Space or Esc again to close it.

## Status

First runnable vertical slice.

The preview core (bounded reads, binary sniffing, extension allow-list, cancellation, state machine, and Space interception rules) is implemented and unit-tested. The Windows runtime is wired: single-instance mutex, low-level keyboard hook, Explorer selection lookup, and one `SpacepeekPreview` window. Every UI transition goes through `state::step`.

Compile-checked for the Windows targets below on a Linux host. **Never executed on Windows.** No timings are claimed.

## Supported platform

Windows 11 only, x86_64 and aarch64. There is no compatibility code for older Windows versions.

## Build

On Windows 11:

```bash
cargo build --release
```

From Linux, cross-check the Windows targets (this does not produce a machine that has been run):

```bash
cargo check --target x86_64-pc-windows-gnu
cargo check --target aarch64-pc-windows-msvc
```

## Run

Start `spacepeek.exe` and leave it running. Spacepeek does not start automatically with Windows. To start it when you sign in, put a shortcut in the Startup folder (`shell:startup`).

A Spacepeek icon stays in the notification area while it is running. Right-click that icon and choose Exit Spacepeek to quit. A left-click opens the same menu. Or run `spacepeek.exe --quit`. That exits with code 0 when it finds the running instance, and with code 1 when nothing is running. If nothing is running, `--quit` shows a short notice.

A second launch shows a message that Spacepeek is already running and exits with a non-zero code.

On Linux and macOS the binary prints `Spacepeek runs on Windows only` and exits with code 1.

## Manual Explorer test (Windows 11)

1. Build and start `spacepeek.exe`.
2. Open File Explorer and select a text file (`.txt`, `.md`, `.log`, `.csv`, `.json`, `.toml`, `.yaml`, `.yml`, `.ini`, `.rs`, `.py`, `.js`, `.ts`, `.c`, `.h`, `.cpp`, `.cs`, `.xml`, `.html`, `.css`, `.sh`, `.ps1`, `.bat`).
3. Press Space. A preview window should open over that Explorer window with the file text.
4. Press Space or Esc. The window closes and focus returns to Explorer.
5. Press F2 to rename, then type a space. The space is inserted into the rename edit.
6. Click the address bar and press Space. The space is typed there.
7. Select a binary file or an extension outside the allow-list and press Space. The window shows an error message.
8. With the preview open, activate another app. The preview closes.

If nothing is selected, or the selection has no filesystem path, the window shows `io error: No file is selected.`

## Automated tests

```bash
cargo test
```

The tests run on Linux. They cover the platform-independent core only.

## Limitations

- No process isolation or sandbox. Parsing runs in-process. The mitigations are a bounded read, an extension allow-list, binary sniffing, and never executing the file.
- Space is intercepted with a global low-level keyboard hook.
- Only the first selected item is previewed.
- Text formats only. Binary and unsupported files show a message.
- The Windows runtime has not been executed. Compile checks are not a substitute for that.

## License

MIT
