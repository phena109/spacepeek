# pdf_measure

Standalone Windows console tool. It times how fast `Windows.Data.Pdf` can render **page 1** of a PDF into a bitmap sized for a Spacepeek preview (default **900×1200**).

This is a measurement spike. It does not add PDF preview to Spacepeek.

The numbers to bring back:

- Cold and warm `open_ms`, `render_ms`, `decode_ms`, `total_ms` for a small text page, a 300-page text file (first page only), a large scanned image, and a vector-heavy page.
- Whether encrypted and truncated files fail quickly.
- Whether `IAsyncInfo.Cancel` actually stops `RenderToStreamAsync`. If every in-flight render still returns `Completed` with a bitmap, Cancel is a no-op for this API.

`total_ms` is wall time for open + render + decode. `peak_working_set_bytes` is the process peak so far (`GetProcessMemoryInfo` `PeakWorkingSetSize`), not the delta for that one file. `working_set_bytes` is the size when the record was finished. `decode_ms` ends when `BitmapDecoder.GetSoftwareBitmapAsync` has pixels (`LockBuffer`), not when a window shows them.

## Build on Windows 11 ARM64

Build **on the ARM64 machine** (or with an ARM64 target) so the exe is native. An x64 exe runs under emulation; the startup banner says which one you got.

```powershell
cargo build --release --manifest-path spike\pdf_measure\Cargo.toml
.\spike\pdf_measure\target\release\pdf_measure.exe --version
```

Needs the Windows 10/11 SDK pieces that ship with a normal Rust MSVC toolchain. No extra native PDF library. The crate uses `windows` 0.58, same major as Spacepeek. It does not change the app.

If `RoInitialize` or WinRT activation fails, the tool prints `HRESULT` plus the message on stderr and exits **1**. That usually means session 0. Run it in an interactive session, or as a scheduled task that loads the user profile.

### Linux check

`cargo check` / `cargo test` succeed here. The binary is a stub: WinRT code is behind `cfg(windows)` and is not type-checked on Linux.

```bash
cargo test --manifest-path spike/pdf_measure/Cargo.toml
cargo check --manifest-path spike/pdf_measure/Cargo.toml
```

## Run

```powershell
pdf_measure.exe --width 900 --height 1200 --repeats 8 --out results.json .\fixtures\*.pdf
```

| Flag | Meaning |
| --- | --- |
| `--width` `--height` | Fit box. Default 900×1200. Page aspect is kept. Smaller pages are not upscaled. Aspect beyond 20:1 uses the full box. |
| `--repeats N` | N passes per file in this process (default 8). The first is `"first"`, the rest are `"warm"`. |
| `--cold` | One pass, kind `"cold"`. Ignores `--repeats`. |
| `--cancel` | Cancellation protocol instead of timed passes. |
| `--cancel-delay-ms` | Delay before the second render cancel (default 30). |
| `--timeout-secs` | Per async WinRT call (default 120). On timeout the tool cancels and moves on. |
| `--out FILE` | Also write JSON. |
| `--json-only` | JSON only on stdout. The banner goes to stderr. |

Exit **0** after a report, even when a PDF fails to load (`ok: false`). Exit **1** if WinRT did not start or `--out` could not be written. Exit **2** for a bad command line.

In one process, only the first file's first pass is truly cold inside that process. Later files already have WinRT loaded. For a real cold start, use a new process per file:

```powershell
.\spike\pdf_measure\cold_measure.ps1 -Exe .\spike\pdf_measure\target\release\pdf_measure.exe -Pdfs (Get-ChildItem .\spike\pdf_measure\fixtures\*.pdf).FullName
```

Stdout is a banner, a table, then `---JSON---`. `--out` is the clean JSON file.

### Cancel

Per file:

1. `RenderToStreamAsync`, then `Cancel` immediately.
2. `RenderToStreamAsync`, wait `--cancel-delay-ms`, then `Cancel`.
3. `LoadFromStreamAsync`, then `Cancel` immediately.

Use `vector_heavy.pdf` or `scan_image.pdf`. A tiny page can finish before `Cancel` runs; `async_status_before_cancel` shows whether the op was still `Started`.

If status was `Started` and the op still `Completed` with a bitmap, Cancel did not stop it. The tool says that in `cancel_summary` and in the record `note`. `cancel_effective` is true only when the final status is `Canceled`. `render_ms` on these rows includes the delay; `cancel_to_complete_ms` starts at the `Cancel` call.

### JSON

`schema_version` is 1. `results[]` records use `kind`: `first`, `warm`, `cold`, `cancel`, or `error`.

Failed loads and renders use `kind: "error"` and keep the pass in `attempt` (`first`, `warm`, `cold`, or `cancel`). Cancel rows stay `kind: "cancel"`. `ok` on a cancel row means the test observed a terminal status, not that a bitmap was produced.

Other fields: `path`, `file_size_bytes`, `page_count`, `password_protected`, `encrypt_dict_present`, `page_width_dip`, `page_height_dip`, `dest_width`, `dest_height`, `output_pixel_width`, `output_pixel_height`, `encoded_stream_bytes`, `open_ms`, `render_ms`, `decode_ms`, `total_ms`, `peak_working_set_bytes`, `working_set_bytes`, `error`, `failed_phase`, `cancel_target` (`render` or `load`), `cancel_delay_ms`, `cancel_to_complete_ms`, `async_status_before_cancel`, `async_status`, `async_hresult`, `bitmap_produced`, `cancel_effective`, `note`.

`password_protected` is `PdfDocument.IsPasswordProtected` when the document loads. If load fails and the file has an `/Encrypt` dictionary, the tool sets it true and says so in `note`. The dictionary scan runs **after** the timer so it does not warm the file cache.

Open path: `FileRandomAccessStream.OpenAsync` + `PdfDocument.LoadFromStreamAsync` (no password). Render path: `GetPage(0)` + `RenderWithOptionsToStreamAsync` into an in-memory stream, PNG encoder, high-contrast ignored. Decode path: `BitmapDecoder.CreateAsync` + `GetSoftwareBitmapAsync` + `LockBuffer`.

Startup banner: Windows version (`RtlGetVersion`), `PROCESSOR_ARCHITECTURE`, `PROCESSOR_ARCHITEW6432`, `IsWow64Process2` process vs native machine, exe path, crate version, git commit baked in at compile time.

## Fixtures

Synthetic only.

```bash
python3 -m pip install reportlab pillow
python3 spike/pdf_measure/gen_fixtures.py
python3 spike/pdf_measure/gen_fixtures.py --out /tmp/pdfs
```

| File | What it is |
| --- | --- |
| `small_text.pdf` | One page of text. |
| `text_300pp.pdf` | 320 text pages. Only page 1 is rendered; load still sees the whole file. |
| `scan_image.pdf` | One page, embedded 3000×4000 JPEG. |
| `vector_heavy.pdf` | One page, dense grid plus thousands of polylines and curves. |
| `encrypted.pdf` | User password `spacepeek`, owner password `spacepeek-owner`, 128-bit. Must not open with an empty password. |
| `broken.pdf` | `small_text.pdf` cut in half. |

`encrypted.pdf` is not fully deterministic (the encryption header uses random bytes in some configurations). The other files use `Canvas(..., invariant=1)` and a fixed random seed.
