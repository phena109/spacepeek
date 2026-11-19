mod host;
mod measure;
mod wait;

use std::io::{self, Write};
use std::time::Duration;

use crate::cli::Args;
use crate::report::{cancel_summary, summary_line, Kind, Report, RunOptions, TABLE_HEADER};

pub fn run(args: &Args) -> i32 {
    if let Err(message) = host::init_winrt() {
        eprintln!("pdf_measure: {message}");
        return 1;
    }

    let host = host::collect();
    let startup_stderr = args.json_only;
    host::print_startup(&host, startup_stderr);

    if args.cold && args.repeats_set {
        eprintln!(
            "pdf_measure: --cold measures each file once; ignoring --repeats {}",
            args.repeats
        );
    }
    if args.cancel && args.repeats_set {
        eprintln!(
            "pdf_measure: --cancel runs the cancellation protocol once per file; ignoring --repeats {}",
            args.repeats
        );
    }

    let timeout = Duration::from_secs(args.timeout_secs);
    let mut results = Vec::new();
    if !args.json_only {
        println!("{TABLE_HEADER}");
    }

    for path in &args.paths {
        let mut batch = if args.cancel {
            measure::cancel_file(path, args.width, args.height, args.cancel_delay_ms, timeout)
        } else if args.cold {
            vec![measure::measure_file(
                path,
                args.width,
                args.height,
                Kind::Cold,
                0,
                timeout,
            )]
        } else {
            let mut rows = Vec::with_capacity(args.repeats as usize);
            for index in 0..args.repeats {
                let kind = if index == 0 { Kind::First } else { Kind::Warm };
                rows.push(measure::measure_file(
                    path,
                    args.width,
                    args.height,
                    kind,
                    index,
                    timeout,
                ));
            }
            rows
        };
        if !args.json_only {
            for row in &batch {
                println!("{}", crate::report::format_row(row));
            }
            let _ = io::stdout().flush();
        }
        results.append(&mut batch);
    }

    let cancel_summary = if args.cancel {
        Some(cancel_summary(&results))
    } else {
        None
    };
    if let Some(summary) = &cancel_summary {
        if args.json_only {
            eprintln!("pdf_measure: {summary}");
        } else {
            println!();
            println!("{summary}");
        }
    }
    if !args.json_only {
        println!("{}", summary_line(&results));
    }

    let repeats_ran = if args.cancel || args.cold {
        1
    } else {
        args.repeats
    };
    let report = Report {
        schema_version: 1,
        tool: "pdf_measure",
        version: env!("CARGO_PKG_VERSION"),
        commit: env!("PDF_MEASURE_COMMIT"),
        host,
        options: RunOptions {
            width: args.width,
            height: args.height,
            repeats_requested: args.repeats,
            repeats_ran,
            cold: args.cold,
            cancel: args.cancel,
            timeout_secs: args.timeout_secs,
            cancel_delay_ms: args.cancel_delay_ms,
        },
        cancel_summary,
        results,
    };

    let json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("pdf_measure: failed to encode JSON: {error}");
            return 1;
        }
    };

    if let Some(path) = &args.out {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "pdf_measure: failed to create {}: {error}",
                        parent.display()
                    );
                    return 1;
                }
            }
        }
        if let Err(error) = std::fs::write(path, format!("{json}\n")) {
            eprintln!("pdf_measure: failed to write {}: {error}", path.display());
            return 1;
        }
    }

    if args.json_only {
        println!("{json}");
    } else {
        println!();
        println!("---JSON---");
        println!("{json}");
    }
    0
}
