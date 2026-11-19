use std::path::PathBuf;

pub const HELP: &str = "\
pdf_measure — time Windows.Data.Pdf first-page preview renders

Usage:
  pdf_measure [options] <pdf> [pdf...]

Options:
  --width <px>            Destination box width (default 900)
  --height <px>           Destination box height (default 1200)
  --repeats <n>           Passes per file in this process (default 8).
                          Pass 1 is kind \"first\"; later passes are \"warm\".
  --cold                  One pass per file, kind \"cold\".
                          Run a fresh process per file for a true cold start.
  --cancel                Cancellation protocol instead of timed passes.
  --cancel-delay-ms <n>   Delay before the second render cancel (default 30)
  --timeout-secs <n>      Timeout for each async WinRT call (default 120)
  --out <file>            Also write the JSON report to this path
  --json-only             JSON only on stdout; startup text goes to stderr
  -h, --help              Show this help
  -V, --version           Print version and git commit

The first page is fitted inside the width x height box. Aspect is preserved
and the page is not upscaled. Aspect ratios beyond 20:1 use the full box.

--cancel, per file:
  1. RenderToStreamAsync, then IAsyncInfo.Cancel immediately
  2. RenderToStreamAsync, wait --cancel-delay-ms, then Cancel
  3. LoadFromStreamAsync, then Cancel immediately
If status is already Started and the render still Completes with a bitmap,
Cancel did not stop the operation.

Exit codes:
  0  report produced (individual PDFs may have failed; see ok/error)
  1  WinRT startup failed, or --out could not be written
  2  bad arguments

Non-Windows builds print a stub message and exit 0. Measurement runs on Windows.
";

#[derive(Debug, Clone)]
pub struct Args {
    pub width: u32,
    pub height: u32,
    pub repeats: u32,
    pub repeats_set: bool,
    pub cold: bool,
    pub cancel: bool,
    pub cancel_delay_ms: u64,
    pub timeout_secs: u64,
    pub out: Option<PathBuf>,
    pub json_only: bool,
    pub help: bool,
    pub version: bool,
    pub paths: Vec<PathBuf>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            width: 900,
            height: 1200,
            repeats: 8,
            repeats_set: false,
            cold: false,
            cancel: false,
            cancel_delay_ms: 30,
            timeout_secs: 120,
            out: None,
            json_only: false,
            help: false,
            version: false,
            paths: Vec::new(),
        }
    }
}

pub fn parse<I>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = String>,
{
    let mut raw = args.into_iter();
    let _argv0 = raw.next();
    let mut args = Args::default();
    let mut positional = false;
    while let Some(arg) = raw.next() {
        if positional {
            args.paths.push(PathBuf::from(arg));
            continue;
        }
        if arg == "--" {
            positional = true;
            continue;
        }
        if arg == "--help" || arg == "-h" {
            args.help = true;
            continue;
        }
        if arg == "--version" || arg == "-V" {
            args.version = true;
            continue;
        }
        if arg == "--cold" {
            args.cold = true;
            continue;
        }
        if arg == "--cancel" {
            args.cancel = true;
            continue;
        }
        if arg == "--json-only" {
            args.json_only = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--width=") {
            args.width = parse_u32("--width", value, 1, 8192)?;
            continue;
        }
        if arg == "--width" {
            args.width = parse_u32("--width", &need(&mut raw, "--width")?, 1, 8192)?;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--height=") {
            args.height = parse_u32("--height", value, 1, 8192)?;
            continue;
        }
        if arg == "--height" {
            args.height = parse_u32("--height", &need(&mut raw, "--height")?, 1, 8192)?;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--repeats=") {
            args.repeats = parse_u32("--repeats", value, 1, 1000)?;
            args.repeats_set = true;
            continue;
        }
        if arg == "--repeats" {
            args.repeats = parse_u32("--repeats", &need(&mut raw, "--repeats")?, 1, 1000)?;
            args.repeats_set = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--timeout-secs=") {
            args.timeout_secs = parse_u64("--timeout-secs", value, 1, 3600)?;
            continue;
        }
        if arg == "--timeout-secs" {
            args.timeout_secs = parse_u64(
                "--timeout-secs",
                &need(&mut raw, "--timeout-secs")?,
                1,
                3600,
            )?;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--cancel-delay-ms=") {
            args.cancel_delay_ms = parse_u64("--cancel-delay-ms", value, 0, 5000)?;
            continue;
        }
        if arg == "--cancel-delay-ms" {
            args.cancel_delay_ms = parse_u64(
                "--cancel-delay-ms",
                &need(&mut raw, "--cancel-delay-ms")?,
                0,
                5000,
            )?;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--out=") {
            args.out = Some(PathBuf::from(value));
            continue;
        }
        if arg == "--out" {
            args.out = Some(PathBuf::from(need(&mut raw, "--out")?));
            continue;
        }
        if let Some(flag) = arg.strip_prefix('-') {
            if !flag.is_empty() {
                return Err(format!("unknown argument: {arg}"));
            }
        }
        args.paths.push(PathBuf::from(arg));
    }
    if args.help || args.version {
        return Ok(args);
    }
    if args.paths.is_empty() {
        return Err("at least one PDF path is required".to_string());
    }
    Ok(args)
}

fn need<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_u32(flag: &str, value: &str, min: u32, max: u32) -> Result<u32, String> {
    let parsed: u32 = value
        .parse()
        .map_err(|_| format!("{flag} expects an integer, got {value:?}"))?;
    if parsed < min || parsed > max {
        return Err(format!("{flag} must be {min}..={max}, got {parsed}"));
    }
    Ok(parsed)
}

fn parse_u64(flag: &str, value: &str, min: u64, max: u64) -> Result<u64, String> {
    let parsed: u64 = value
        .parse()
        .map_err(|_| format!("{flag} expects an integer, got {value:?}"))?;
    if parsed < min || parsed > max {
        return Err(format!("{flag} must be {min}..={max}, got {parsed}"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn parse_from(args: &[&str]) -> Result<super::Args, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn defaults_and_paths() {
        let args = parse_from(&["pdf_measure", "a.pdf", "b.pdf"]).unwrap();
        assert_eq!(args.width, 900);
        assert_eq!(args.height, 1200);
        assert_eq!(args.repeats, 8);
        assert!(!args.repeats_set);
        assert_eq!(args.paths.len(), 2);
        assert!(!args.cold);
        assert!(!args.cancel);
    }

    #[test]
    fn flags() {
        let args = parse_from(&[
            "pdf_measure",
            "--width",
            "640",
            "--height=480",
            "--repeats",
            "3",
            "--cold",
            "--timeout-secs",
            "30",
            "--out",
            "out.json",
            "--",
            "--odd-name.pdf",
        ])
        .unwrap();
        assert_eq!(args.width, 640);
        assert_eq!(args.height, 480);
        assert_eq!(args.repeats, 3);
        assert!(args.repeats_set);
        assert!(args.cold);
        assert_eq!(args.timeout_secs, 30);
        assert_eq!(args.out.unwrap().as_os_str(), "out.json");
        assert_eq!(args.paths[0].as_os_str(), "--odd-name.pdf");
    }

    #[test]
    fn rejects_unknown_and_missing() {
        assert!(parse_from(&["pdf_measure"]).is_err());
        assert!(parse_from(&["pdf_measure", "--nope", "a.pdf"]).is_err());
        assert!(parse_from(&["pdf_measure", "--width", "a.pdf"]).is_err());
        assert!(parse_from(&["pdf_measure", "--width", "0", "a.pdf"]).is_err());
    }
}
