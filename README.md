# Spacepeek

Space-bar file preview for Windows Explorer. Select a file, press Space, and a single topmost window shows a bounded text preview. The preview does not take focus from Explorer. Press Space or Esc again to close it. The header has an × button as well. PNG and JPEG are previewed too: the format is sniffed from the magic bytes, and the file has to fit the size limits.

Text is drawn in a monospace font (Cascadia Mono, then Consolas) with word wrap off and a horizontal scrollbar, so columns and indentation stay aligned. Each line sent to the preview is capped at 2,000 characters. The preview loads at most the first 64 KiB. When that is not the whole file, or when lines were shortened, a notice at the top of the text says what is shown (for example `Showing the first 64 KB of 1.2 MB`). The header repeats that state in short form, such as `12 KB · 2,000 lines (truncated)`.

A slim header shows the file name and that details line. Drag the header to move the window. Drag the edges to resize it. The last size you set is reused for the next preview until Spacepeek exits. The size is stored in logical pixels and scaled for the monitor's DPI. The smallest size is 320×200 logical pixels. Error and empty-file windows start smaller (480×320 logical) until you resize. The extra height fits a short message plus type, size, and modified time. The × button picks up a light grey background while the pointer is over it. Double-click the text or the picture to close the preview and open that file with its usual app. A folder opens too. A double-click on the header or the × button does not.

## Status

First runnable vertical slice.

The preview core (bounded reads, character-boundary truncation, binary sniffing, a deny-list of known binary and image extensions, cancellation, state machine, keyboard rules, header text, hit testing, close-button hover, which double-click region opens the file, quick-info lines for unsupported files, remembered window size, and the sentences shown for errors and empty files) is implemented and unit-tested. The Windows runtime is wired: single-instance mutex, low-level keyboard hook, Explorer selection lookup, and one non-activating `SpacepeekPreview` window. Every UI transition goes through `state::step`.

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

A Spacepeek icon stays in the notification area while it is running. Right-click that icon and choose Exit Spacepeek to quit. A left-click opens the same menu. Or run `spacepeek.exe --quit`. That exits with code 0 when it finds the running instance, and with code 1 when nothing is running. If nothing is running, `--quit` shows a short notice. `spacepeek.exe --version` prints the build version.

A second launch shows a message that Spacepeek is already running and exits with a non-zero code.

On Linux and macOS the binary prints `Spacepeek runs on Windows only` and exits with code 1.

## Manual Explorer test (Windows 11)

1. Build and start `spacepeek.exe`.
2. Open File Explorer and select a text file. The extension does not decide this: `.txt`, source code, `Makefile`, `Dockerfile`, `LICENSE`, and `.gitignore` all preview when the bytes look like text.
3. Press Space. A preview window should open over that Explorer window with the file name in the header and the file text in a monospace font. Explorer keeps the keyboard focus, and the preview text has no caret. Drag the header to move the window, drag an edge to resize it, or click × to close it. Moving the pointer over × turns that button a light grey; moving it away clears the grey. Double-click the text or the picture (not the header, and not ×) to close the preview and open the file the way Explorer would. A folder opens in Explorer. A triple-click does not open it twice. A long file shows a truncation notice at the top of the text and `(truncated)` in the header. Clicking or dragging the header should not leave a caret in the preview or take the keyboard away from Explorer for good.
4. Press Space or Esc. The window closes. Explorer still has the focus. Press Space again. The preview opens without clicking the file list first.
5. Press F2 to rename, then type a space. The space is inserted into the rename edit.
6. Click the address bar and press Space. The space is typed there.
7. In the file list, type the start of a name and, within about a second, press Space. The space continues Explorer's type-ahead search instead of opening a preview.
8. With a preview open, press Down or Up. Explorer moves the selection and the preview follows the new file. Left and Right do the same when Explorer changes the selected file. Page Up, Page Down, Home, and End scroll the preview text. The mouse wheel over the preview and the scrollbars also scroll it. A long line scrolls sideways on the horizontal scrollbar.
9. Select a PNG or JPEG and press Space. The window shows the picture. A PNG renamed to `.txt` still previews, because the type comes from the magic bytes. Select a known binary or other image file (for example `.exe`, `.zip`, `.pdf`, `.gif`) and press Space. The window shows a short message such as `photo.gif can't be previewed — this file type isn't supported yet`, and under that, when the facts can be read without opening the file, lines for Type (the Windows type name, or the extension in capitals plus `file`), Size (such as `1.2 MB (1,234,567 bytes)`), and Modified (local date and time). A text extension whose first 8 KB contains a NUL or invalid UTF-8 shows `<name> doesn't look like text` and the same kind of lines. Access denied, a missing file, and a file that is open in another program stay a single sentence. An empty file shows `This file is empty`. A folder shows `Folder: <name>` and, when the entries can be listed, a count capped at `10,000+`. A `.lnk` shortcut shows `Shortcut to <target>` when the stored target can be read, and otherwise says that previewing shortcuts isn't supported yet. A file locked by another program says it is open in another program and can't be read. A file that was deleted says it no longer exists. A file you cannot access says access was denied.
10. Press Space on a slow file and press Esc before the window appears, or switch to another app. The preview should not appear. With a preview open, activate another app. The preview closes.

If nothing is selected, or the selection has no filesystem path, Space does not open a window. The key was already taken by the hook, which cannot query Explorer; the UI thread checks the selection and then does nothing. Putting the key back would enter the hook again.

## Automated tests

```bash
cargo test
```

The tests run on Linux. They cover the platform-independent core only.

## Limitations

- No process isolation or sandbox. Parsing runs in-process. The mitigations are a bounded read, a deny-list of known binary and image extensions, binary sniffing, and never executing the file.
- Space is intercepted with a global low-level keyboard hook.
- Only the first selected item is previewed.
- Files are text when the first 8 KB has no NUL and is valid UTF-8 (a partial character at that 8 KB edge is allowed), or when the file starts with a UTF-16 BOM. PNG and JPEG are previewed (magic-byte sniffed, with size limits); the extension is ignored. Known binary and other image extensions are refused before that sniff and reported in a sentence that includes the file name. That sentence, and the "doesn't look like text" sentence, can include type, size, and modified time gathered without reading the file contents. The Windows type name comes from `SHGetFileInfoW` with `SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES`, or the extension in capitals plus `file` when the shell name is unavailable. Those shell calls are compile-checked, not run on a desktop. A `.lnk` file is not opened as text; the Windows build reads the shortcut's stored target for the message and does not search or show a link dialog. A zero-byte file is an empty-file message, not a blank preview. A folder is not listed as text; the message names the folder and, when listing succeeds, how many entries it has, stopping at 10,000.
- The preview keeps at most 2,000 lines and 2,000 characters of each line. Cuts are on a character boundary. The notice at the top reports the bytes shown and the file size, and says when lines were left out or shortened.
- Arrow keys are delivered to Explorer while that window still has focus. The preview reloads only when the selected path changes. In Details view, Left and Right often leave the same file selected. Clicking another file does not retarget the preview. Arrow keys are not forwarded when the preview window itself is in the foreground.
- A Space that continues an Explorer type-ahead search is passed through. While a preview is already open, that Space does not move the preview.
- The Windows runtime has not been executed. Compile checks are not a substitute for that.

## License

MIT
