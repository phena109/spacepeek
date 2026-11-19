use std::sync::mpsc;
use std::time::Duration;

use windows::core::{Error, RuntimeType, HRESULT};
use windows::Foundation::{
    AsyncActionCompletedHandler, AsyncOperationCompletedHandler, AsyncStatus, IAsyncAction,
    IAsyncOperation,
};
use windows::Win32::Foundation::E_FAIL;

pub fn format_winrt_error(error: &Error) -> String {
    let code = error.code();
    let message = error.message();
    if message.is_empty() {
        format!("HRESULT {}", hresult_hex(code))
    } else {
        format!("HRESULT {}: {message}", hresult_hex(code))
    }
}

pub fn hresult_hex(code: HRESULT) -> String {
    format!("0x{:08X}", code.0 as u32)
}

pub fn status_name(status: AsyncStatus) -> String {
    if status == AsyncStatus::Started {
        "Started".to_string()
    } else if status == AsyncStatus::Completed {
        "Completed".to_string()
    } else if status == AsyncStatus::Canceled {
        "Canceled".to_string()
    } else if status == AsyncStatus::Error {
        "Error".to_string()
    } else {
        format!("AsyncStatus({})", status.0)
    }
}

#[derive(Debug)]
pub enum WaitError {
    Winrt(Error),
    Timeout { observed: Option<AsyncStatus> },
    Disconnected,
}

impl WaitError {
    pub fn message(&self) -> String {
        match self {
            Self::Winrt(error) => format_winrt_error(error),
            Self::Timeout { observed } => match observed {
                Some(status) => format!(
                    "timed out; status after cancel attempt is {}",
                    status_name(*status)
                ),
                None => "timed out".to_string(),
            },
            Self::Disconnected => "async completion handler disconnected".to_string(),
        }
    }
}

pub fn wait_action(action: &IAsyncAction, timeout: Duration) -> Result<AsyncStatus, WaitError> {
    let initial = action.Status().map_err(WaitError::Winrt)?;
    if initial != AsyncStatus::Started {
        return Ok(initial);
    }
    let (tx, rx) = mpsc::channel();
    action
        .SetCompleted(&AsyncActionCompletedHandler::new(move |_sender, status| {
            let _ = tx.send(status);
            Ok(())
        }))
        .map_err(WaitError::Winrt)?;
    wait_channel(rx, timeout, || action.Status().ok(), || action.Cancel())
}

pub fn wait_operation_status<T>(
    operation: &IAsyncOperation<T>,
    timeout: Duration,
) -> Result<AsyncStatus, WaitError>
where
    T: RuntimeType + 'static,
{
    let initial = operation.Status().map_err(WaitError::Winrt)?;
    if initial != AsyncStatus::Started {
        return Ok(initial);
    }
    let (tx, rx) = mpsc::channel();
    operation
        .SetCompleted(&AsyncOperationCompletedHandler::new(
            move |_sender, status| {
                let _ = tx.send(status);
                Ok(())
            },
        ))
        .map_err(WaitError::Winrt)?;
    wait_channel(
        rx,
        timeout,
        || operation.Status().ok(),
        || operation.Cancel(),
    )
}

fn wait_channel(
    rx: mpsc::Receiver<AsyncStatus>,
    timeout: Duration,
    current: impl Fn() -> Option<AsyncStatus>,
    cancel: impl Fn() -> windows::core::Result<()>,
) -> Result<AsyncStatus, WaitError> {
    match rx.recv_timeout(timeout) {
        Ok(status) => Ok(status),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = cancel();
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(status) => Err(WaitError::Timeout {
                    observed: Some(status),
                }),
                Err(_) => Err(WaitError::Timeout {
                    observed: current(),
                }),
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(WaitError::Disconnected),
    }
}

pub fn join_operation<T>(operation: &IAsyncOperation<T>, timeout: Duration) -> Result<T, String>
where
    T: RuntimeType + 'static,
{
    let status = wait_operation_status(operation, timeout).map_err(|error| error.message())?;
    if status == AsyncStatus::Completed {
        return operation
            .GetResults()
            .map_err(|error| format_winrt_error(&error));
    }
    let code = operation.ErrorCode().unwrap_or(E_FAIL);
    Err(format!(
        "{} ({})",
        status_name(status),
        format_winrt_error(&Error::from(code))
    ))
}

pub fn join_action(action: &IAsyncAction, timeout: Duration) -> Result<(), String> {
    let status = wait_action(action, timeout).map_err(|error| error.message())?;
    if status == AsyncStatus::Completed {
        return action
            .GetResults()
            .map_err(|error| format_winrt_error(&error));
    }
    let code = action.ErrorCode().unwrap_or(E_FAIL);
    Err(format!(
        "{} ({})",
        status_name(status),
        format_winrt_error(&Error::from(code))
    ))
}
