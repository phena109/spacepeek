#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Spacepeek runs on Windows only");
    std::process::ExitCode::from(1)
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // Windows runtime slice: WH_KEYBOARD_LL hook -> FocusContext -> state::step -> preview load.
    // Full window and Explorer interop wiring lands in subsequent slices.
    let _ = spacepeek::should_intercept_space;
    let _ = spacepeek::step;
    std::process::ExitCode::SUCCESS
}
