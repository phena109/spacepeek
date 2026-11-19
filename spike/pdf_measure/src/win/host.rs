use std::io::{self, Write};

use windows::core::HSTRING;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::{BitmapDecoder, BitmapEncoder};
use windows::Storage::Streams::InMemoryRandomAccessStream;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
    IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN, OSVERSIONINFOEXW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::report::HostInfo;

use super::wait::format_winrt_error;

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OSVERSIONINFOEXW) -> i32;
}

pub fn init_winrt() -> Result<(), String> {
    unsafe {
        if let Err(error) = RoInitialize(RO_INIT_MULTITHREADED) {
            return Err(format!(
                "RoInitialize(RO_INIT_MULTITHREADED) failed: {}",
                format_winrt_error(&error)
            ));
        }
    }
    if let Err(error) = activate_classes() {
        return Err(format!(
            "WinRT activation failed: {}. If this process is in session 0, run pdf_measure from an interactive user session (or a scheduled task that loads the user profile).",
            format_winrt_error(&error)
        ));
    }
    Ok(())
}

fn activate_classes() -> windows::core::Result<()> {
    // Touch the factories we need so a missing WinRT class fails before timing.
    PdfDocument::IPdfDocumentStatics(|_| Ok(()))?;
    let _options = PdfPageRenderOptions::new()?;
    let _png = BitmapEncoder::PngEncoderId()?;
    let _png_decoder = BitmapDecoder::PngDecoderId()?;
    let stream = InMemoryRandomAccessStream::new()?;
    stream.Close()?;
    // Keep the HSTRING path warm; constructing one does not activate a class.
    let _ = HSTRING::from("Windows.Data.Pdf.PdfDocument");
    Ok(())
}

pub fn collect() -> HostInfo {
    let (process_machine, native_machine, wow64) = machine_info();
    let emulated = wow64;
    let image_kind = if emulated {
        format!("{process_machine} emulated on {native_machine}")
    } else {
        format!("native {native_machine}")
    };
    HostInfo {
        windows_version: windows_version(),
        processor_architecture: env_nonempty("PROCESSOR_ARCHITECTURE"),
        processor_architew6432: env_nonempty("PROCESSOR_ARCHITEW6432"),
        process_machine,
        native_machine,
        wow64,
        emulated,
        image_kind,
        exe_path: std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
    }
}

pub fn print_startup(host: &HostInfo, to_stderr: bool) {
    let lines = format!(
        "\
pdf_measure {version} (commit {commit})
windows_version: {win}
PROCESSOR_ARCHITECTURE: {arch}
PROCESSOR_ARCHITEW6432: {arch32}
process_machine: {process}
native_machine: {native}
wow64: {wow}
emulated: {emulated}
image_kind: {kind}
exe: {exe}
",
        version = env!("CARGO_PKG_VERSION"),
        commit = env!("PDF_MEASURE_COMMIT"),
        win = host.windows_version,
        arch = host.processor_architecture.as_deref().unwrap_or("(unset)"),
        arch32 = host.processor_architew6432.as_deref().unwrap_or("(unset)"),
        process = host.process_machine,
        native = host.native_machine,
        wow = host.wow64,
        emulated = host.emulated,
        kind = host.image_kind,
        exe = host.exe_path,
    );
    if to_stderr {
        eprint!("{lines}");
        let _ = io::stderr().flush();
    } else {
        print!("{lines}");
        let _ = io::stdout().flush();
    }
}

pub fn working_set() -> Option<(u64, u64)> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    let process: HANDLE = unsafe { GetCurrentProcess() };
    if unsafe { GetProcessMemoryInfo(process, &mut counters, counters.cb) }.is_err() {
        return None;
    }
    Some((
        counters.PeakWorkingSetSize as u64,
        counters.WorkingSetSize as u64,
    ))
}

fn machine_info() -> (String, String, bool) {
    let mut process = IMAGE_FILE_MACHINE_UNKNOWN;
    let mut native = IMAGE_FILE_MACHINE_UNKNOWN;
    let query = unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &mut process,
            Some(std::ptr::addr_of_mut!(native)),
        )
    };
    if let Err(error) = query {
        let message = format_winrt_error(&error);
        return (
            format!("unavailable ({message})"),
            format!("unavailable ({message})"),
            false,
        );
    }
    let process_name = machine_name(process);
    let native_name = machine_name(native);
    // A native process reports IMAGE_FILE_MACHINE_UNKNOWN as the process machine.
    let wow64 = process != IMAGE_FILE_MACHINE_UNKNOWN && process != native;
    let process_label = if process == IMAGE_FILE_MACHINE_UNKNOWN {
        format!("{native_name} (process machine UNKNOWN, not WOW64)")
    } else {
        process_name
    };
    (process_label, native_name, wow64)
}

fn machine_name(machine: IMAGE_FILE_MACHINE) -> String {
    if machine == IMAGE_FILE_MACHINE_ARM64 {
        "ARM64".to_string()
    } else if machine == IMAGE_FILE_MACHINE_AMD64 {
        "AMD64".to_string()
    } else if machine == IMAGE_FILE_MACHINE_I386 {
        "I386".to_string()
    } else if machine == IMAGE_FILE_MACHINE_UNKNOWN {
        "UNKNOWN".to_string()
    } else {
        format!("OTHER(0x{:04X})", machine.0)
    }
}

fn windows_version() -> String {
    let mut info = OSVERSIONINFOEXW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
        ..OSVERSIONINFOEXW::default()
    };
    let status = unsafe { RtlGetVersion(&mut info) };
    if status < 0 {
        return format!("unknown (RtlGetVersion status {status})");
    }
    let name = if info.dwMajorVersion == 10 && info.dwBuildNumber >= 22000 {
        "Windows 11"
    } else if info.dwMajorVersion == 10 {
        "Windows 10"
    } else {
        "Windows"
    };
    format!(
        "{name} {}.{}.{}",
        info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
    )
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
