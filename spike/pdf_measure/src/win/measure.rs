//! Timed open / render / decode, and the cancellation protocol.
//!
//! The file is not read before the timed `FileRandomAccessStream` open, so the
//! first pass still pays for a cold file cache. The `/Encrypt` scan runs after
//! the clock stops.
//!
//! `render_ms` includes `GetPage(0)`, option setup, and `RenderToStreamAsync`.
//! On a cancel run it also includes the intentional delay before `Cancel`.
//! `cancel_to_complete_ms` starts at the `Cancel` call.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{Error, HSTRING};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Foundation::AsyncStatus;
use windows::Graphics::Imaging::{
    BitmapBufferAccessMode, BitmapDecoder, BitmapEncoder, SoftwareBitmap,
};
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::{
    FileRandomAccessStream, IRandomAccessStream, InMemoryRandomAccessStream,
};
use windows::Win32::Foundation::E_FAIL;

use crate::fit::fit_destination;
use crate::report::{append_note, duration_ms, Kind, Record};
use crate::scan::encrypt_dict_present;

use super::host::working_set;
use super::wait::{
    format_winrt_error, hresult_hex, join_action, join_operation, status_name, wait_action,
    wait_operation_status,
};

struct Loaded {
    stream: IRandomAccessStream,
    doc: PdfDocument,
    open_ms: f64,
    page_count: u32,
    password_protected: Option<bool>,
}

impl Loaded {
    fn close(self) {
        let _ = self.stream.Close();
    }
}

struct LoadError {
    open_ms: f64,
    message: String,
}

pub fn measure_file(
    path: &Path,
    box_w: u32,
    box_h: u32,
    kind: Kind,
    repeat_index: u32,
    timeout: Duration,
) -> Record {
    let opened = absolute_path(path);
    let mut record = Record::new(opened.display().to_string(), kind, repeat_index);
    let wall = Instant::now();

    if let Err(message) = prepare_file(&opened, &mut record) {
        fail(&mut record, "file", message, wall);
        return record;
    }

    let loaded = match load_document(&opened, timeout) {
        Ok(loaded) => loaded,
        Err(error) => {
            record.open_ms = Some(error.open_ms);
            fail(&mut record, "load", error.message, wall);
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };
    record.open_ms = Some(loaded.open_ms);
    record.page_count = Some(loaded.page_count);
    record.password_protected = loaded.password_protected;

    if loaded.page_count == 0 {
        loaded.close();
        fail(&mut record, "load", "PDF has no pages".to_string(), wall);
        finish_file_metadata(&opened, &mut record);
        return record;
    }

    let rendered = match render_first_page(&loaded.doc, box_w, box_h, timeout, None) {
        Ok(rendered) => rendered,
        Err(error) => {
            loaded.close();
            apply_render_error(&mut record, &error);
            fail(&mut record, &error.phase, error.message, wall);
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };
    record.page_width_dip = Some(f64::from(rendered.page_w));
    record.page_height_dip = Some(f64::from(rendered.page_h));
    record.dest_width = Some(rendered.dest_w);
    record.dest_height = Some(rendered.dest_h);
    record.render_ms = Some(rendered.render_ms);
    record.encoded_stream_bytes = Some(rendered.encoded_bytes);

    match decode_output(&rendered.output, timeout) {
        Ok(decoded) => {
            record.decode_ms = Some(decoded.decode_ms);
            record.output_pixel_width = Some(decoded.width);
            record.output_pixel_height = Some(decoded.height);
            record.ok = true;
            record.total_ms = Some(duration_ms(wall.elapsed()));
            fill_memory(&mut record);
        }
        Err(error) => {
            record.decode_ms = Some(error.elapsed_ms);
            fail(&mut record, "decode", error.message, wall);
        }
    }
    let _ = rendered.output.Close();
    let _ = rendered.page.Close();
    loaded.close();
    finish_file_metadata(&opened, &mut record);
    record
}

pub fn cancel_file(
    path: &Path,
    box_w: u32,
    box_h: u32,
    delay_ms: u64,
    timeout: Duration,
) -> Vec<Record> {
    vec![
        cancel_render(path, box_w, box_h, 0, timeout),
        cancel_render(path, box_w, box_h, delay_ms, timeout),
        cancel_load(path, timeout),
    ]
}

fn cancel_render(path: &Path, box_w: u32, box_h: u32, delay_ms: u64, timeout: Duration) -> Record {
    let opened = absolute_path(path);
    let mut record = Record::new(opened.display().to_string(), Kind::Cancel, 0);
    record.cancel_target = Some("render".to_string());
    record.cancel_delay_ms = Some(delay_ms);
    let wall = Instant::now();

    if let Err(message) = prepare_file(&opened, &mut record) {
        fail(&mut record, "file", message, wall);
        return record;
    }

    let loaded = match load_document(&opened, timeout) {
        Ok(loaded) => loaded,
        Err(error) => {
            record.open_ms = Some(error.open_ms);
            fail(&mut record, "load", error.message, wall);
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };
    record.open_ms = Some(loaded.open_ms);
    record.page_count = Some(loaded.page_count);
    record.password_protected = loaded.password_protected;
    if loaded.page_count == 0 {
        loaded.close();
        fail(&mut record, "load", "PDF has no pages".to_string(), wall);
        finish_file_metadata(&opened, &mut record);
        return record;
    }

    let rendered = match render_first_page(&loaded.doc, box_w, box_h, timeout, Some(delay_ms)) {
        Ok(rendered) => rendered,
        Err(error) => {
            loaded.close();
            apply_render_error(&mut record, &error);
            // A cancel observation that ended in Canceled/Error is still a result.
            if error.cancel.is_some() {
                apply_cancel_outcome(&mut record, &error, wall);
            } else {
                fail(&mut record, &error.phase, error.message, wall);
            }
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };

    record.page_width_dip = Some(f64::from(rendered.page_w));
    record.page_height_dip = Some(f64::from(rendered.page_h));
    record.dest_width = Some(rendered.dest_w);
    record.dest_height = Some(rendered.dest_h);
    record.render_ms = Some(rendered.render_ms);
    record.encoded_stream_bytes = Some(rendered.encoded_bytes);
    record.async_status_before_cancel = rendered.status_before.clone();
    record.async_status = rendered.status_after.clone();
    record.async_hresult = rendered.hresult.clone();
    record.cancel_to_complete_ms = rendered.cancel_to_complete_ms;
    if let Some(note) = rendered.note.clone() {
        append_note(&mut record.note, note);
    }
    note_cancel_timing(&mut record, delay_ms);

    let produced = confirm_bitmap(&rendered.output, &mut record, timeout);
    record.bitmap_produced = Some(produced);
    record.cancel_effective = Some(rendered.status_after.as_deref() == Some("Canceled"));
    record.ok = true;
    if rendered.status_before.as_deref() == Some("Started")
        && rendered.status_after.as_deref() == Some("Completed")
        && produced
    {
        append_note(
            &mut record.note,
            "Cancel did not stop the render: status was Started, then Completed, and a bitmap was produced. Cancel behaved as a no-op (or the renderer ignored it).",
        );
    } else if rendered.status_before.as_deref() != Some("Started") {
        append_note(
            &mut record.note,
            format!(
                "Cancel was called when status was already {}. This run did not observe an in-flight render.",
                rendered.status_before.as_deref().unwrap_or("unknown")
            ),
        );
    } else if rendered.status_after.as_deref() == Some("Canceled") && !produced {
        append_note(
            &mut record.note,
            "Cancel stopped the render: status is Canceled and no bitmap was produced.",
        );
    } else if rendered.status_after.as_deref() == Some("Canceled") && produced {
        append_note(
            &mut record.note,
            "Status is Canceled but the output stream decoded as a bitmap. Partial or raced output.",
        );
    } else if rendered.status_after.as_deref() == Some("Error") {
        append_note(
            &mut record.note,
            "Render ended with Error after Cancel. async_hresult has the code.",
        );
    } else if rendered.status_after.as_deref() == Some("Completed") && !produced {
        append_note(
            &mut record.note,
            "Render Completed after Cancel but the stream did not decode to a bitmap.",
        );
    }
    record.total_ms = Some(duration_ms(wall.elapsed()));
    fill_memory(&mut record);
    let _ = rendered.output.Close();
    let _ = rendered.page.Close();
    loaded.close();
    finish_file_metadata(&opened, &mut record);
    record
}

fn cancel_load(path: &Path, timeout: Duration) -> Record {
    let opened = absolute_path(path);
    let mut record = Record::new(opened.display().to_string(), Kind::Cancel, 0);
    record.cancel_target = Some("load".to_string());
    record.cancel_delay_ms = Some(0);
    let wall = Instant::now();

    if let Err(message) = prepare_file(&opened, &mut record) {
        fail(&mut record, "file", message, wall);
        return record;
    }

    let open_started = Instant::now();
    let stream = match open_read_stream(&opened, timeout) {
        Ok(stream) => stream,
        Err(error) => {
            record.open_ms = Some(duration_ms(open_started.elapsed()));
            fail(&mut record, "open", error, wall);
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };

    let load_op = match PdfDocument::LoadFromStreamAsync(&stream) {
        Ok(op) => op,
        Err(error) => {
            record.open_ms = Some(duration_ms(open_started.elapsed()));
            let _ = stream.Close();
            fail(
                &mut record,
                "load",
                format!("LoadFromStreamAsync: {}", format_winrt_error(&error)),
                wall,
            );
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };

    let before = match load_op.Status() {
        Ok(status) => status_name(status),
        Err(error) => {
            record.open_ms = Some(duration_ms(open_started.elapsed()));
            let _ = stream.Close();
            fail(
                &mut record,
                "load",
                format!("Status: {}", format_winrt_error(&error)),
                wall,
            );
            finish_file_metadata(&opened, &mut record);
            return record;
        }
    };
    record.async_status_before_cancel = Some(before.clone());
    let cancel_at = Instant::now();
    if let Err(error) = load_op.Cancel() {
        append_note(
            &mut record.note,
            format!("Cancel() failed: {}", format_winrt_error(&error)),
        );
    }
    match wait_operation_status(&load_op, timeout) {
        Ok(status) => {
            record.async_status = Some(status_name(status));
            record.async_hresult = load_op.ErrorCode().ok().map(hresult_hex);
            record.cancel_to_complete_ms = Some(duration_ms(cancel_at.elapsed()));
            record.open_ms = Some(duration_ms(open_started.elapsed()));
            if status == AsyncStatus::Completed {
                if let Ok(doc) = load_op.GetResults() {
                    record.page_count = doc.PageCount().ok();
                    record.password_protected = doc.IsPasswordProtected().ok();
                }
                record.bitmap_produced = Some(false);
                record.cancel_effective = Some(false);
                if before == "Started" {
                    append_note(
                        &mut record.note,
                        "LoadFromStreamAsync was Started when Cancel was called and still Completed. Cancel did not stop the load.",
                    );
                } else {
                    append_note(
                        &mut record.note,
                        "Load finished before Cancel was observed.",
                    );
                }
            } else if status == AsyncStatus::Canceled {
                record.cancel_effective = Some(true);
                record.bitmap_produced = Some(false);
                append_note(&mut record.note, "LoadFromStreamAsync returned Canceled.");
            } else {
                record.cancel_effective = Some(false);
                record.bitmap_produced = Some(false);
                let code = load_op.ErrorCode().unwrap_or(E_FAIL);
                append_note(
                    &mut record.note,
                    format!(
                        "Load ended with {} ({}).",
                        status_name(status),
                        format_winrt_error(&Error::from(code))
                    ),
                );
            }
            record.ok = true;
            record.total_ms = Some(duration_ms(wall.elapsed()));
            fill_memory(&mut record);
        }
        Err(error) => {
            record.open_ms = Some(duration_ms(open_started.elapsed()));
            record.cancel_to_complete_ms = Some(duration_ms(cancel_at.elapsed()));
            fail(&mut record, "load", error.message(), wall);
        }
    }
    let _ = stream.Close();
    finish_file_metadata(&opened, &mut record);
    record
}

struct Rendered {
    page: PdfPage,
    output: InMemoryRandomAccessStream,
    page_w: f32,
    page_h: f32,
    dest_w: u32,
    dest_h: u32,
    render_ms: f64,
    encoded_bytes: u64,
    status_before: Option<String>,
    status_after: Option<String>,
    hresult: Option<String>,
    cancel_to_complete_ms: Option<f64>,
    note: Option<String>,
}

struct RenderError {
    phase: String,
    message: String,
    page_w: Option<f32>,
    page_h: Option<f32>,
    dest_w: Option<u32>,
    dest_h: Option<u32>,
    render_ms: Option<f64>,
    encoded_bytes: Option<u64>,
    cancel: Option<CancelSnap>,
}

struct CancelSnap {
    status_before: String,
    status_after: Option<String>,
    hresult: Option<String>,
    cancel_to_complete_ms: Option<f64>,
    delay_ms: u64,
}

struct DecodeOut {
    width: u32,
    height: u32,
    decode_ms: f64,
}

struct DecodeError {
    message: String,
    elapsed_ms: f64,
}

fn render_first_page(
    doc: &PdfDocument,
    box_w: u32,
    box_h: u32,
    timeout: Duration,
    cancel_delay_ms: Option<u64>,
) -> Result<Rendered, RenderError> {
    let started = Instant::now();
    let fail = |phase: &str, message: String| RenderError {
        phase: phase.to_string(),
        message,
        page_w: None,
        page_h: None,
        dest_w: None,
        dest_h: None,
        render_ms: Some(duration_ms(started.elapsed())),
        encoded_bytes: None,
        cancel: None,
    };

    let page = match doc.GetPage(0) {
        Ok(page) => page,
        Err(error) => {
            return Err(fail(
                "render",
                format!("GetPage(0): {}", format_winrt_error(&error)),
            ))
        }
    };
    let size = match page.Size() {
        Ok(size) => size,
        Err(error) => {
            let _ = page.Close();
            return Err(fail(
                "render",
                format!("PdfPage.Size: {}", format_winrt_error(&error)),
            ));
        }
    };
    let (dest_w, dest_h) = fit_destination(size.Width, size.Height, box_w, box_h);
    let options = match PdfPageRenderOptions::new() {
        Ok(options) => options,
        Err(error) => {
            let _ = page.Close();
            return Err(fail(
                "render",
                format!("PdfPageRenderOptions: {}", format_winrt_error(&error)),
            ));
        }
    };
    if let Err(error) = options.SetDestinationWidth(dest_w) {
        let _ = page.Close();
        return Err(fail(
            "render",
            format!("SetDestinationWidth: {}", format_winrt_error(&error)),
        ));
    }
    if let Err(error) = options.SetDestinationHeight(dest_h) {
        let _ = page.Close();
        return Err(fail(
            "render",
            format!("SetDestinationHeight: {}", format_winrt_error(&error)),
        ));
    }
    // High contrast can change rasterization cost. Ignore it for a stable number.
    if let Err(error) = options.SetIsIgnoringHighContrast(true) {
        let _ = page.Close();
        return Err(fail(
            "render",
            format!("SetIsIgnoringHighContrast: {}", format_winrt_error(&error)),
        ));
    }
    if let Ok(encoder) = BitmapEncoder::PngEncoderId() {
        if let Err(error) = options.SetBitmapEncoderId(encoder) {
            let _ = page.Close();
            return Err(fail(
                "render",
                format!("SetBitmapEncoderId: {}", format_winrt_error(&error)),
            ));
        }
    }
    let output = match InMemoryRandomAccessStream::new() {
        Ok(output) => output,
        Err(error) => {
            let _ = page.Close();
            return Err(fail(
                "render",
                format!("InMemoryRandomAccessStream: {}", format_winrt_error(&error)),
            ));
        }
    };
    let action = match page.RenderWithOptionsToStreamAsync(&output, &options) {
        Ok(action) => action,
        Err(error) => {
            let _ = output.Close();
            let _ = page.Close();
            return Err(fail(
                "render",
                format!(
                    "RenderWithOptionsToStreamAsync: {}",
                    format_winrt_error(&error)
                ),
            ));
        }
    };

    if let Some(delay_ms) = cancel_delay_ms {
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        let before = match action.Status() {
            Ok(status) => status_name(status),
            Err(error) => {
                let _ = output.Close();
                let _ = page.Close();
                return Err(fail(
                    "render",
                    format!("Status: {}", format_winrt_error(&error)),
                ));
            }
        };
        let cancel_at = Instant::now();
        let mut cancel_note = None;
        if let Err(error) = action.Cancel() {
            cancel_note = Some(format!("Cancel() failed: {}", format_winrt_error(&error)));
        }
        match wait_action(&action, timeout) {
            Ok(status) => {
                // Completed, Canceled, and Error are all observations. Keep the
                // stream so the caller can see whether a bitmap was produced.
                let encoded_bytes = output.Size().unwrap_or(0);
                let hresult = action.ErrorCode().ok().map(hresult_hex);
                return Ok(Rendered {
                    page,
                    output,
                    page_w: size.Width,
                    page_h: size.Height,
                    dest_w,
                    dest_h,
                    render_ms: duration_ms(started.elapsed()),
                    encoded_bytes,
                    status_before: Some(before),
                    status_after: Some(status_name(status)),
                    hresult,
                    cancel_to_complete_ms: Some(duration_ms(cancel_at.elapsed())),
                    note: cancel_note,
                });
            }
            Err(error) => {
                let encoded_bytes = output.Size().unwrap_or(0);
                let hresult = action.ErrorCode().ok().map(hresult_hex);
                let _ = output.Close();
                let _ = page.Close();
                let message = match cancel_note {
                    Some(note) => format!("{note}; {}", error.message()),
                    None => error.message(),
                };
                return Err(RenderError {
                    phase: "render".to_string(),
                    message,
                    page_w: Some(size.Width),
                    page_h: Some(size.Height),
                    dest_w: Some(dest_w),
                    dest_h: Some(dest_h),
                    render_ms: Some(duration_ms(started.elapsed())),
                    encoded_bytes: Some(encoded_bytes),
                    cancel: Some(CancelSnap {
                        status_before: before,
                        status_after: None,
                        hresult,
                        cancel_to_complete_ms: Some(duration_ms(cancel_at.elapsed())),
                        delay_ms,
                    }),
                });
            }
        }
    }

    if let Err(error) = join_action(&action, timeout) {
        let encoded_bytes = output.Size().unwrap_or(0);
        let _ = output.Close();
        let _ = page.Close();
        return Err(RenderError {
            phase: "render".to_string(),
            message: error,
            page_w: Some(size.Width),
            page_h: Some(size.Height),
            dest_w: Some(dest_w),
            dest_h: Some(dest_h),
            render_ms: Some(duration_ms(started.elapsed())),
            encoded_bytes: Some(encoded_bytes),
            cancel: None,
        });
    }
    let encoded_bytes = match output.Size() {
        Ok(size) => size,
        Err(error) => {
            let _ = output.Close();
            let _ = page.Close();
            return Err(fail(
                "render",
                format!("output stream Size: {}", format_winrt_error(&error)),
            ));
        }
    };
    Ok(Rendered {
        page,
        output,
        page_w: size.Width,
        page_h: size.Height,
        dest_w,
        dest_h,
        render_ms: duration_ms(started.elapsed()),
        encoded_bytes,
        status_before: None,
        status_after: None,
        hresult: None,
        cancel_to_complete_ms: None,
        note: None,
    })
}

fn decode_output(
    output: &InMemoryRandomAccessStream,
    timeout: Duration,
) -> Result<DecodeOut, DecodeError> {
    let started = Instant::now();
    let fail = |message: String| DecodeError {
        message,
        elapsed_ms: duration_ms(started.elapsed()),
    };
    if let Err(error) = output.Seek(0) {
        return Err(fail(format!("Seek(0): {}", format_winrt_error(&error))));
    }
    let decoder_op = match BitmapDecoder::CreateAsync(output) {
        Ok(op) => op,
        Err(error) => {
            return Err(fail(format!(
                "BitmapDecoder.CreateAsync: {}",
                format_winrt_error(&error)
            )))
        }
    };
    let decoder = match join_operation(&decoder_op, timeout) {
        Ok(decoder) => decoder,
        Err(error) => return Err(fail(format!("BitmapDecoder.CreateAsync: {error}"))),
    };
    let bitmap_op = match decoder.GetSoftwareBitmapAsync() {
        Ok(op) => op,
        Err(error) => {
            return Err(fail(format!(
                "GetSoftwareBitmapAsync: {}",
                format_winrt_error(&error)
            )))
        }
    };
    let bitmap = match join_operation(&bitmap_op, timeout) {
        Ok(bitmap) => bitmap,
        Err(error) => return Err(fail(format!("GetSoftwareBitmapAsync: {error}"))),
    };
    let result = read_bitmap(&bitmap);
    let _ = bitmap.Close();
    match result {
        Ok((width, height)) => Ok(DecodeOut {
            width,
            height,
            decode_ms: duration_ms(started.elapsed()),
        }),
        Err(message) => Err(fail(message)),
    }
}

fn read_bitmap(bitmap: &SoftwareBitmap) -> Result<(u32, u32), String> {
    let width = bitmap
        .PixelWidth()
        .map_err(|error| format!("PixelWidth: {}", format_winrt_error(&error)))?;
    let height = bitmap
        .PixelHeight()
        .map_err(|error| format!("PixelHeight: {}", format_winrt_error(&error)))?;
    if width <= 0 || height <= 0 {
        return Err(format!("SoftwareBitmap is {width}x{height}"));
    }
    let buffer = bitmap
        .LockBuffer(BitmapBufferAccessMode::Read)
        .map_err(|error| format!("LockBuffer: {}", format_winrt_error(&error)))?;
    let plane = buffer
        .GetPlaneDescription(0)
        .map_err(|error| format!("GetPlaneDescription: {}", format_winrt_error(&error)))?;
    let _ = buffer.Close();
    if plane.Width <= 0 || plane.Height <= 0 {
        return Err(format!("bitmap plane is {}x{}", plane.Width, plane.Height));
    }
    Ok((width as u32, height as u32))
}

fn confirm_bitmap(
    output: &InMemoryRandomAccessStream,
    record: &mut Record,
    timeout: Duration,
) -> bool {
    let bytes = output.Size().unwrap_or(0);
    record.encoded_stream_bytes = Some(bytes);
    if bytes == 0 {
        record.decode_ms = Some(0.0);
        return false;
    }
    match decode_output(output, timeout) {
        Ok(decoded) => {
            record.decode_ms = Some(decoded.decode_ms);
            record.output_pixel_width = Some(decoded.width);
            record.output_pixel_height = Some(decoded.height);
            true
        }
        Err(error) => {
            record.decode_ms = Some(error.elapsed_ms);
            append_note(
                &mut record.note,
                format!(
                    "Output stream has {bytes} bytes but did not decode: {}",
                    error.message
                ),
            );
            false
        }
    }
}

fn load_document(path: &Path, timeout: Duration) -> Result<Loaded, LoadError> {
    let started = Instant::now();
    let stream = match open_read_stream(path, timeout) {
        Ok(stream) => stream,
        Err(message) => {
            return Err(LoadError {
                open_ms: duration_ms(started.elapsed()),
                message,
            })
        }
    };
    let op = match PdfDocument::LoadFromStreamAsync(&stream) {
        Ok(op) => op,
        Err(error) => {
            let _ = stream.Close();
            return Err(LoadError {
                open_ms: duration_ms(started.elapsed()),
                message: format!("LoadFromStreamAsync: {}", format_winrt_error(&error)),
            });
        }
    };
    let doc = match join_operation(&op, timeout) {
        Ok(doc) => doc,
        Err(message) => {
            let _ = stream.Close();
            return Err(LoadError {
                open_ms: duration_ms(started.elapsed()),
                message: format!("LoadFromStreamAsync: {message}"),
            });
        }
    };
    let page_count = match doc.PageCount() {
        Ok(count) => count,
        Err(error) => {
            let _ = stream.Close();
            return Err(LoadError {
                open_ms: duration_ms(started.elapsed()),
                message: format!("PageCount: {}", format_winrt_error(&error)),
            });
        }
    };
    let password_protected = doc.IsPasswordProtected().ok();
    Ok(Loaded {
        stream,
        doc,
        open_ms: duration_ms(started.elapsed()),
        page_count,
        password_protected,
    })
}

fn open_read_stream(path: &Path, timeout: Duration) -> Result<IRandomAccessStream, String> {
    let path_h = HSTRING::from(path);
    let op = FileRandomAccessStream::OpenAsync(&path_h, FileAccessMode::Read)
        .map_err(|error| format!("OpenAsync: {}", format_winrt_error(&error)))?;
    join_operation(&op, timeout).map_err(|message| format!("OpenAsync: {message}"))
}

fn prepare_file(path: &Path, record: &mut Record) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("{error}"))?;
    if !meta.is_file() {
        return Err("path is not a file".to_string());
    }
    record.file_size_bytes = Some(meta.len());
    Ok(())
}

fn finish_file_metadata(path: &Path, record: &mut Record) {
    match encrypt_dict_present(path) {
        Ok(present) => {
            record.encrypt_dict_present = Some(present);
            if record.password_protected.is_none() && present {
                record.password_protected = Some(true);
                append_note(
                    &mut record.note,
                    "password_protected inferred from a /Encrypt dictionary because the document did not load, so PdfDocument.IsPasswordProtected was unavailable",
                );
            }
        }
        Err(error) => append_note(&mut record.note, format!("encrypt scan failed: {error}")),
    }
}

fn apply_render_error(record: &mut Record, error: &RenderError) {
    record.page_width_dip = error.page_w.map(f64::from);
    record.page_height_dip = error.page_h.map(f64::from);
    record.dest_width = error.dest_w;
    record.dest_height = error.dest_h;
    record.render_ms = error.render_ms;
    record.encoded_stream_bytes = error.encoded_bytes;
    if let Some(cancel) = &error.cancel {
        record.async_status_before_cancel = Some(cancel.status_before.clone());
        record.async_status = cancel.status_after.clone();
        record.async_hresult = cancel.hresult.clone();
        record.cancel_to_complete_ms = cancel.cancel_to_complete_ms;
        record.cancel_delay_ms = Some(cancel.delay_ms);
        record.bitmap_produced = Some(false);
        record.cancel_effective = Some(cancel.status_after.as_deref() == Some("Canceled"));
    }
}

fn apply_cancel_outcome(record: &mut Record, error: &RenderError, wall: Instant) {
    record.ok = false;
    record.failed_phase = Some(error.phase.clone());
    record.error = Some(format!("{}: {}", error.phase, error.message));
    record.total_ms = Some(duration_ms(wall.elapsed()));
    fill_memory(record);
    append_note(
        &mut record.note,
        "Cancellation wait did not reach a terminal status inside the timeout.",
    );
}

fn note_cancel_timing(record: &mut Record, delay_ms: u64) {
    if delay_ms > 0 {
        append_note(
            &mut record.note,
            format!(
                "render_ms includes the {delay_ms} ms delay before Cancel. cancel_to_complete_ms does not."
            ),
        );
    }
}

fn fail(record: &mut Record, phase: &str, message: String, wall: Instant) {
    if record.kind != Kind::Cancel {
        record.kind = Kind::Error;
    }
    record.ok = false;
    record.failed_phase = Some(phase.to_string());
    record.error = Some(format!("{phase}: {message}"));
    record.total_ms = Some(duration_ms(wall.elapsed()));
    fill_memory(record);
}

fn fill_memory(record: &mut Record) {
    if let Some((peak, current)) = working_set() {
        record.peak_working_set_bytes = Some(peak);
        record.working_set_bytes = Some(current);
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}
