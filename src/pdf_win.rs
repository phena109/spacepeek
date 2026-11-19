//! First page of a PDF via `Windows.Data.Pdf`.
//!
//! The `spacepeek-pdf` worker calls [`render_pdf_page`] after that thread has
//! called `RoInitialize(RO_INIT_MULTITHREADED)`. This module does not
//! initialize COM or WinRT, and nothing here runs at process start or on the
//! UI thread. Text, raster, Markdown, and SVG loads do not call it.
//!
//! The WinRT document, page, and streams are closed before the function
//! returns. The preview keeps the bitmap and the page count only.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{Interface, HSTRING};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Foundation::{AsyncStatus, IAsyncAction, IAsyncOperation};
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBuffer, BitmapBufferAccessMode, BitmapDecoder, BitmapEncoder,
    BitmapPixelFormat, SoftwareBitmap,
};
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::{
    FileRandomAccessStream, IRandomAccessStream, InMemoryRandomAccessStream,
};
use windows::Win32::System::WinRT::IMemoryBufferByteAccess;

use crate::image_preview::{DecodedImage, ImageFormat};
use crate::pdf::{
    classify_pdf_open_failure, gate_pdf_file, pack_bgra_rows, pdf_error_from_hresult,
    pdf_page_pixels, read_encrypt_probe, settle_pdf_job, PdfError, PdfGate, PdfJobPhase,
    PdfJobSnap, PdfLimits, PdfPagePx, PdfSettle, PdfTarget,
};
use crate::preview::{CancelToken, Generation};

/// How often a still-running `IAsyncOperation` is polled. Cancel and the
/// timeout are noticed on this cadence; the UI thread is not involved.
const PDF_POLL: Duration = Duration::from_millis(15);

/// Pixels and page count. The `PdfDocument` is already released.
pub struct RenderedPdf {
    pub total_bytes: u64,
    pub pages: u32,
    pub image: DecodedImage,
}

/// Open `path`, render page 1 into `target`, and copy the pixels out.
///
/// `cancel` / `gen` drop the result when the user has moved on. The budget
/// in `limits` covers open and render together. A bitmap that finishes after
/// that budget is a timeout, not a page.
pub fn render_pdf_page(
    path: &Path,
    target: PdfTarget,
    limits: &PdfLimits,
    cancel: &CancelToken,
    gen: Generation,
) -> Result<RenderedPdf, PdfError> {
    let clock = JobClock {
        started: Instant::now(),
        limits,
        cancel,
        gen,
    };
    let total_bytes = file_len(path, limits)?;
    clock.gate(PdfJobPhase::Opening)?;
    let stream = open_stream(path, clock)?;
    let outcome = render_stream(&stream, path, target, clock);
    let _ = stream.Close();
    let (pages, image) = outcome?;
    Ok(RenderedPdf {
        total_bytes,
        pages,
        image,
    })
}

#[derive(Clone, Copy)]
struct JobClock<'a> {
    started: Instant,
    limits: &'a PdfLimits,
    cancel: &'a CancelToken,
    gen: Generation,
}

impl JobClock<'_> {
    fn snap(self, phase: PdfJobPhase) -> PdfJobSnap {
        PdfJobSnap {
            phase,
            job_gen: self.gen,
            current_gen: self.cancel.current_generation(),
            cancel_requested: self.cancel.is_cancelled(&self.gen),
            elapsed: self.started.elapsed(),
            budget: self.limits.timeout,
        }
    }

    /// Stop before starting the next WinRT call when the job is already stale
    /// or the budget is already spent.
    fn gate(self, phase: PdfJobPhase) -> Result<(), PdfError> {
        match settle_pdf_job(self.snap(phase), false) {
            PdfSettle::KeepWaiting | PdfSettle::ShowPage => Ok(()),
            PdfSettle::Discard => Err(PdfError::Cancelled),
            PdfSettle::Fallback(err) => Err(err),
        }
    }
}

fn file_len(path: &Path, limits: &PdfLimits) -> Result<u64, PdfError> {
    let meta = std::fs::metadata(path).map_err(|_| PdfError::Damaged)?;
    if !meta.is_file() {
        return Err(PdfError::Damaged);
    }
    match gate_pdf_file(meta.len(), limits) {
        PdfGate::Render => Ok(meta.len()),
        PdfGate::Empty => Err(PdfError::Damaged),
        PdfGate::TooLarge { bytes } => Err(PdfError::FileTooLarge { bytes }),
    }
}

fn open_stream(path: &Path, clock: JobClock<'_>) -> Result<IRandomAccessStream, PdfError> {
    clock.gate(PdfJobPhase::Opening)?;
    let path_h = HSTRING::from(path);
    let op = FileRandomAccessStream::OpenAsync(&path_h, FileAccessMode::Read)
        .map_err(|err| pdf_error_from_hresult(err.code().0))?;
    take_operation(&op, PdfJobPhase::Opening, clock)
}

fn render_stream(
    stream: &IRandomAccessStream,
    path: &Path,
    target: PdfTarget,
    clock: JobClock<'_>,
) -> Result<(u32, DecodedImage), PdfError> {
    clock.gate(PdfJobPhase::Opening)?;
    let op = PdfDocument::LoadFromStreamAsync(stream)
        .map_err(|err| pdf_error_from_hresult(err.code().0))?;
    let doc = take_operation(&op, PdfJobPhase::Opening, clock)
        .map_err(|err| classify_failed_load(path, err))?;
    let outcome = page_from_document(&doc, target, clock);
    drop(doc);
    outcome
}

fn classify_failed_load(path: &Path, err: PdfError) -> PdfError {
    if err != PdfError::Damaged {
        return err;
    }
    let encrypt = read_encrypt_probe(path).unwrap_or(false);
    classify_pdf_open_failure(None, encrypt)
}

fn page_from_document(
    doc: &PdfDocument,
    target: PdfTarget,
    clock: JobClock<'_>,
) -> Result<(u32, DecodedImage), PdfError> {
    if doc.IsPasswordProtected().unwrap_or(false) {
        return Err(PdfError::PasswordProtected);
    }
    let pages = doc.PageCount().map_err(|_| PdfError::Damaged)?;
    if pages == 0 {
        return Err(PdfError::Damaged);
    }
    let image = render_first_page(doc, target, clock)?;
    Ok((pages, image))
}

fn render_first_page(
    doc: &PdfDocument,
    target: PdfTarget,
    clock: JobClock<'_>,
) -> Result<DecodedImage, PdfError> {
    clock.gate(PdfJobPhase::Rendering)?;
    let page = doc.GetPage(0).map_err(|_| PdfError::Damaged)?;
    let image = rasterize(&page, target, clock);
    let _ = page.Close();
    image
}

fn rasterize(
    page: &PdfPage,
    target: PdfTarget,
    clock: JobClock<'_>,
) -> Result<DecodedImage, PdfError> {
    let size = page.Size().map_err(|_| PdfError::Damaged)?;
    let px = pdf_page_pixels(size.Width, size.Height, target, clock.limits)?;
    let options =
        PdfPageRenderOptions::new().map_err(|err| pdf_error_from_hresult(err.code().0))?;
    options
        .SetDestinationWidth(px.render_width)
        .map_err(|_| PdfError::Damaged)?;
    options
        .SetDestinationHeight(px.render_height)
        .map_err(|_| PdfError::Damaged)?;
    // The page stays as drawn. High contrast would recolour it.
    options
        .SetIsIgnoringHighContrast(true)
        .map_err(|_| PdfError::Damaged)?;
    if let Ok(encoder) = BitmapEncoder::PngEncoderId() {
        options
            .SetBitmapEncoderId(encoder)
            .map_err(|_| PdfError::Damaged)?;
    }
    let output =
        InMemoryRandomAccessStream::new().map_err(|err| pdf_error_from_hresult(err.code().0))?;
    let image = render_into(page, &output, &options, &px, clock);
    let _ = output.Close();
    image
}

fn render_into(
    page: &PdfPage,
    output: &InMemoryRandomAccessStream,
    options: &PdfPageRenderOptions,
    px: &PdfPagePx,
    clock: JobClock<'_>,
) -> Result<DecodedImage, PdfError> {
    clock.gate(PdfJobPhase::Rendering)?;
    let action = page
        .RenderWithOptionsToStreamAsync(output, options)
        .map_err(|err| pdf_error_from_hresult(err.code().0))?;
    finish_action(&action, clock)?;
    decode_page(output, px, clock)
}

fn decode_page(
    output: &InMemoryRandomAccessStream,
    px: &PdfPagePx,
    clock: JobClock<'_>,
) -> Result<DecodedImage, PdfError> {
    clock.gate(PdfJobPhase::Rendering)?;
    output.Seek(0).map_err(|_| PdfError::Damaged)?;
    let created =
        BitmapDecoder::CreateAsync(output).map_err(|err| pdf_error_from_hresult(err.code().0))?;
    let decoder = take_operation(&created, PdfJobPhase::Rendering, clock)?;
    let converted = decoder
        .GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Ignore)
        .map_err(|err| pdf_error_from_hresult(err.code().0))?;
    let bitmap = take_operation(&converted, PdfJobPhase::Rendering, clock)?;
    let image = copy_bitmap(&bitmap, px);
    let _ = bitmap.Close();
    image
}

fn copy_bitmap(bitmap: &SoftwareBitmap, px: &PdfPagePx) -> Result<DecodedImage, PdfError> {
    let width_i = bitmap.PixelWidth().map_err(|_| PdfError::Damaged)?;
    let height_i = bitmap.PixelHeight().map_err(|_| PdfError::Damaged)?;
    if width_i <= 0 || height_i <= 0 {
        return Err(PdfError::InvalidSize);
    }
    let width = u32::try_from(width_i).map_err(|_| PdfError::InvalidSize)?;
    let height = u32::try_from(height_i).map_err(|_| PdfError::InvalidSize)?;
    let buffer = bitmap
        .LockBuffer(BitmapBufferAccessMode::Read)
        .map_err(|_| PdfError::Damaged)?;
    let bgra = copy_plane(&buffer, width, height);
    let _ = buffer.Close();
    Ok(DecodedImage {
        width,
        height,
        source_width: px.source_width,
        source_height: px.source_height,
        format: ImageFormat::Pdf,
        bgra: bgra?,
    })
}

fn copy_plane(buffer: &BitmapBuffer, width: u32, height: u32) -> Result<Vec<u8>, PdfError> {
    let plane = buffer
        .GetPlaneDescription(0)
        .map_err(|_| PdfError::Damaged)?;
    let reference = buffer.CreateReference().map_err(|_| PdfError::Damaged)?;
    let access = reference
        .cast::<IMemoryBufferByteAccess>()
        .map_err(|_| PdfError::Damaged)?;
    let copied = read_plane(&access, plane.StartIndex, width, height, plane.Stride);
    let _ = reference.Close();
    copied
}

fn read_plane(
    access: &IMemoryBufferByteAccess,
    start: i32,
    width: u32,
    height: u32,
    stride: i32,
) -> Result<Vec<u8>, PdfError> {
    let mut ptr = std::ptr::null_mut();
    let mut cap = 0_u32;
    unsafe { access.GetBuffer(&mut ptr, &mut cap) }.map_err(|_| PdfError::Damaged)?;
    if ptr.is_null() {
        return Err(PdfError::Damaged);
    }
    let len = usize::try_from(cap).map_err(|_| PdfError::Damaged)?;
    // SAFETY: `GetBuffer` returns a pointer and a byte count that stay valid
    // until the `IMemoryBufferReference` behind `access` is closed. The
    // caller closes that reference only after this function returns, and the
    // rows are copied into an owned buffer before that.
    let src = unsafe { std::slice::from_raw_parts(ptr, len) };
    pack_bgra_rows(src, start, width, height, stride)
}

trait AsyncInfo {
    fn status(&self) -> windows::core::Result<AsyncStatus>;
    fn cancel_op(&self) -> windows::core::Result<()>;
    fn error_code(&self) -> windows::core::Result<windows::core::HRESULT>;
    fn close_op(&self);
}

impl AsyncInfo for IAsyncAction {
    fn status(&self) -> windows::core::Result<AsyncStatus> {
        self.Status()
    }

    fn cancel_op(&self) -> windows::core::Result<()> {
        self.Cancel()
    }

    fn error_code(&self) -> windows::core::Result<windows::core::HRESULT> {
        self.ErrorCode()
    }

    fn close_op(&self) {
        let _ = self.Close();
    }
}

impl<T> AsyncInfo for IAsyncOperation<T>
where
    T: windows::core::RuntimeType + 'static,
{
    fn status(&self) -> windows::core::Result<AsyncStatus> {
        self.Status()
    }

    fn cancel_op(&self) -> windows::core::Result<()> {
        self.Cancel()
    }

    fn error_code(&self) -> windows::core::Result<windows::core::HRESULT> {
        self.ErrorCode()
    }

    fn close_op(&self) {
        let _ = self.Close();
    }
}

enum Waited {
    Completed,
    Failed(windows::core::HRESULT),
}

/// Poll until the op leaves `Started`, or cancel it when the job is stale
/// or over budget. A completed op that is already over budget is a timeout:
/// the decision is made before `GetResults`.
fn drive(op: &impl AsyncInfo, phase: PdfJobPhase, clock: JobClock<'_>) -> Result<Waited, PdfError> {
    loop {
        let status = op.status();
        let terminal = !matches!(status, Ok(AsyncStatus::Started));
        match settle_pdf_job(clock.snap(phase), terminal) {
            PdfSettle::KeepWaiting => std::thread::sleep(PDF_POLL),
            PdfSettle::ShowPage => return accept(op, status),
            PdfSettle::Discard => {
                let _ = op.cancel_op();
                return Err(PdfError::Cancelled);
            }
            PdfSettle::Fallback(err) => {
                let _ = op.cancel_op();
                return Err(err);
            }
        }
    }
}

fn accept(
    op: &impl AsyncInfo,
    status: windows::core::Result<AsyncStatus>,
) -> Result<Waited, PdfError> {
    match status {
        Ok(AsyncStatus::Completed) => Ok(Waited::Completed),
        Ok(AsyncStatus::Error) => {
            // E_FAIL when the op does not report a code of its own.
            let hr = op
                .error_code()
                .unwrap_or(windows::core::HRESULT(0x8000_4005_u32 as i32));
            Ok(Waited::Failed(hr))
        }
        // We cancel through `Discard` before this. A cancel we did not ask
        // for is a failed render, not a silent drop of the placeholder.
        Ok(AsyncStatus::Canceled) => Err(PdfError::Damaged),
        Ok(_) => Err(PdfError::Damaged),
        Err(err) => Err(pdf_error_from_hresult(err.code().0)),
    }
}

fn take_operation<T>(
    op: &IAsyncOperation<T>,
    phase: PdfJobPhase,
    clock: JobClock<'_>,
) -> Result<T, PdfError>
where
    T: windows::core::RuntimeType + 'static,
{
    let result = drive(op, phase, clock).and_then(|waited| match waited {
        Waited::Completed => op
            .GetResults()
            .map_err(|err| pdf_error_from_hresult(err.code().0)),
        Waited::Failed(hr) => Err(pdf_error_from_hresult(hr.0)),
    });
    op.close_op();
    result
}

fn finish_action(action: &IAsyncAction, clock: JobClock<'_>) -> Result<(), PdfError> {
    let result = drive(action, PdfJobPhase::Rendering, clock).and_then(|waited| match waited {
        Waited::Completed => action
            .GetResults()
            .map_err(|err| pdf_error_from_hresult(err.code().0)),
        Waited::Failed(hr) => Err(pdf_error_from_hresult(hr.0)),
    });
    action.close_op();
    result
}
