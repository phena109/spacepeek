//! Human-readable rows and the JSON report.
//!
//! `total_ms` is wall time from the start of the open/load sequence through
//! decode (or through the failure). It is not a separately measured sum, so it
//! can differ from open_ms + render_ms + decode_ms by scheduling gaps.
//!
//! `peak_working_set_bytes` is the process peak so far (`PeakWorkingSetSize`),
//! not the delta for that file. `working_set_bytes` is the size at the moment
//! the record was finished.

#![cfg_attr(not(windows), allow(dead_code))]

use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    First,
    Warm,
    Cold,
    Cancel,
    Error,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Warm => "warm",
            Self::Cold => "cold",
            Self::Cancel => "cancel",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    pub windows_version: String,
    pub processor_architecture: Option<String>,
    pub processor_architew6432: Option<String>,
    pub process_machine: String,
    pub native_machine: String,
    pub wow64: bool,
    pub emulated: bool,
    pub image_kind: String,
    pub exe_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOptions {
    pub width: u32,
    pub height: u32,
    pub repeats_requested: u32,
    pub repeats_ran: u32,
    pub cold: bool,
    pub cancel: bool,
    pub timeout_secs: u64,
    pub cancel_delay_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub kind: Kind,
    pub attempt: String,
    pub path: String,
    pub repeat_index: u32,
    pub file_size_bytes: Option<u64>,
    pub page_count: Option<u32>,
    pub password_protected: Option<bool>,
    pub encrypt_dict_present: Option<bool>,
    pub page_width_dip: Option<f64>,
    pub page_height_dip: Option<f64>,
    pub dest_width: Option<u32>,
    pub dest_height: Option<u32>,
    pub output_pixel_width: Option<u32>,
    pub output_pixel_height: Option<u32>,
    pub encoded_stream_bytes: Option<u64>,
    pub open_ms: Option<f64>,
    pub render_ms: Option<f64>,
    pub decode_ms: Option<f64>,
    pub total_ms: Option<f64>,
    pub peak_working_set_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
    pub failed_phase: Option<String>,
    pub cancel_target: Option<String>,
    pub cancel_delay_ms: Option<u64>,
    pub cancel_to_complete_ms: Option<f64>,
    pub async_status_before_cancel: Option<String>,
    pub async_status: Option<String>,
    pub async_hresult: Option<String>,
    pub bitmap_produced: Option<bool>,
    pub cancel_effective: Option<bool>,
    pub note: Option<String>,
}

impl Record {
    pub fn new(path: impl Into<String>, kind: Kind, repeat_index: u32) -> Self {
        Self {
            kind,
            attempt: kind.as_str().to_string(),
            path: path.into(),
            repeat_index,
            file_size_bytes: None,
            page_count: None,
            password_protected: None,
            encrypt_dict_present: None,
            page_width_dip: None,
            page_height_dip: None,
            dest_width: None,
            dest_height: None,
            output_pixel_width: None,
            output_pixel_height: None,
            encoded_stream_bytes: None,
            open_ms: None,
            render_ms: None,
            decode_ms: None,
            total_ms: None,
            peak_working_set_bytes: None,
            working_set_bytes: None,
            ok: false,
            error: None,
            failed_phase: None,
            cancel_target: None,
            cancel_delay_ms: None,
            cancel_to_complete_ms: None,
            async_status_before_cancel: None,
            async_status: None,
            async_hresult: None,
            bitmap_produced: None,
            cancel_effective: None,
            note: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub tool: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
    pub host: HostInfo,
    pub options: RunOptions,
    pub cancel_summary: Option<String>,
    pub results: Vec<Record>,
}

pub fn duration_ms(duration: Duration) -> f64 {
    let ms = duration.as_secs_f64() * 1000.0;
    (ms * 1000.0).round() / 1000.0
}

pub fn append_note(existing: &mut Option<String>, text: impl Into<String>) {
    let text = text.into();
    match existing {
        Some(current) if current.is_empty() => *current = text,
        Some(current) => {
            current.push(' ');
            current.push_str(&text);
        }
        None => *existing = Some(text),
    }
}

pub const TABLE_HEADER: &str = "\
kind   rep path                                       pages   open_ms  render_ms  decode_ms  total_ms dest        pixels      stream      peak  status";

pub fn format_row(record: &Record) -> String {
    format!(
        "{:<6} {:>3} {:<40} {:>5} {:>9} {:>10} {:>10} {:>9} {:<11} {:<11} {:>8} {:>8}  {}",
        record.kind.as_str(),
        record.repeat_index,
        tail(&record.path, 40),
        opt_u32(record.page_count),
        opt_ms(record.open_ms),
        opt_ms(record.render_ms),
        opt_ms(record.decode_ms),
        opt_ms(record.total_ms),
        pair(record.dest_width, record.dest_height),
        pair(record.output_pixel_width, record.output_pixel_height),
        opt_bytes(record.encoded_stream_bytes),
        opt_mib(record.peak_working_set_bytes),
        status_cell(record),
    )
}

pub fn cancel_summary(records: &[Record]) -> String {
    let renders: Vec<&Record> = records
        .iter()
        .filter(|record| record.cancel_target.as_deref() == Some("render"))
        .collect();
    if renders.is_empty() {
        return "CANCEL RESULT: no render cancel records.".to_string();
    }
    let mut stopped = 0;
    let mut ran_on = 0;
    let mut already_done = 0;
    let mut other = 0;
    for record in &renders {
        let before_started = record.async_status_before_cancel.as_deref() == Some("Started");
        let after = record.async_status.as_deref();
        if before_started && after == Some("Canceled") {
            stopped += 1;
        } else if before_started
            && after == Some("Completed")
            && record.bitmap_produced == Some(true)
        {
            ran_on += 1;
        } else if !before_started {
            already_done += 1;
        } else {
            other += 1;
        }
    }
    if ran_on == renders.len() {
        return format!(
            "CANCEL RESULT: Cancel is a no-op for PdfPage.RenderToStreamAsync on this run. All {ran_on} render ops were Started when Cancel was called, then Completed and produced a bitmap."
        );
    }
    if stopped == renders.len() {
        return format!(
            "CANCEL RESULT: IAsyncInfo.Cancel stopped all {stopped} PdfPage renders (status Canceled)."
        );
    }
    let mut text = format!(
        "CANCEL RESULT: mixed render cancels. {stopped} stopped (Canceled after Started), {ran_on} ran to completion with a bitmap after Cancel, {already_done} were already finished when Cancel was called, {other} other."
    );
    if ran_on > 0 {
        text.push_str(
            " Where a render was Started and still produced a bitmap, Cancel did not stop that render.",
        );
    }
    text
}

pub fn summary_line(rows: &[Record]) -> String {
    let measured_ok = rows
        .iter()
        .filter(|row| row.ok && row.kind != Kind::Cancel)
        .count();
    let cancel = rows.iter().filter(|row| row.kind == Kind::Cancel).count();
    let not_ok = rows.iter().filter(|row| !row.ok).count();
    format!("{measured_ok} measurements ok, {cancel} cancel records, {not_ok} not ok")
}

fn status_cell(record: &Record) -> String {
    if record.kind == Kind::Cancel {
        let status = record.async_status.as_deref().unwrap_or("?");
        let bitmap = match record.bitmap_produced {
            Some(true) => "bitmap",
            Some(false) => "no-bitmap",
            None => "bitmap?",
        };
        let effect = match record.cancel_effective {
            Some(true) => "stopped",
            Some(false) => "ran-on",
            None => "unclear",
        };
        if record.ok {
            return format!("{status} {bitmap} {effect}");
        }
        return format!("FAIL {status} {bitmap} {effect}");
    }
    if record.ok {
        return "ok".to_string();
    }
    let err = record.error.as_deref().unwrap_or("error");
    let one_line: String = err.split_whitespace().collect::<Vec<_>>().join(" ");
    tail(&one_line, 72)
}

fn opt_ms(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.1}"),
        None => "-".to_string(),
    }
}

fn opt_u32(value: Option<u32>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "-".to_string(),
    }
}

fn pair(width: Option<u32>, height: Option<u32>) -> String {
    match (width, height) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => "-".to_string(),
    }
}

fn opt_bytes(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "-".to_string(),
    }
}

fn opt_mib(value: Option<u64>) -> String {
    match value {
        Some(value) => format!("{:.1}", value as f64 / (1024.0 * 1024.0)),
        None => "-".to_string(),
    }
}

fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let skip = count - (max_chars - 3);
    let rest: String = text.chars().skip(skip).collect();
    format!("...{rest}")
}

#[cfg(test)]
mod tests {
    use super::{cancel_summary, duration_ms, format_row, summary_line, Kind, Record};
    use std::time::Duration;

    #[test]
    fn kind_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Kind::First).unwrap(), "\"first\"");
        assert_eq!(serde_json::to_string(&Kind::Warm).unwrap(), "\"warm\"");
        assert_eq!(serde_json::to_string(&Kind::Cold).unwrap(), "\"cold\"");
        assert_eq!(serde_json::to_string(&Kind::Cancel).unwrap(), "\"cancel\"");
        assert_eq!(serde_json::to_string(&Kind::Error).unwrap(), "\"error\"");
    }

    #[test]
    fn duration_rounds_to_milliseconds() {
        assert_eq!(duration_ms(Duration::from_micros(1500)), 1.5);
    }

    #[test]
    fn row_and_summary() {
        let mut record = Record::new("small_text.pdf", Kind::First, 0);
        record.ok = true;
        record.page_count = Some(1);
        record.open_ms = Some(1.2);
        record.render_ms = Some(3.4);
        record.decode_ms = Some(0.5);
        record.total_ms = Some(5.1);
        record.dest_width = Some(816);
        record.dest_height = Some(1056);
        let line = format_row(&record);
        assert!(line.contains("first"));
        assert!(line.contains("small_text.pdf"));
        assert!(line.contains("ok"));
        assert!(summary_line(std::slice::from_ref(&record)).contains("1 measurements ok"));
    }

    #[test]
    fn cancel_summary_calls_out_a_noop() {
        let mut record = Record::new("vector.pdf", Kind::Cancel, 0);
        record.cancel_target = Some("render".to_string());
        record.async_status_before_cancel = Some("Started".to_string());
        record.async_status = Some("Completed".to_string());
        record.bitmap_produced = Some(true);
        record.ok = true;
        let text = cancel_summary(&[record]);
        assert!(text.contains("no-op"));
    }
}
