mod cli;
mod fit;
mod report;
mod scan;

#[cfg(windows)]
mod win;

fn main() {
    let args = match cli::parse(std::env::args()) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("pdf_measure: {error}");
            eprintln!("Try pdf_measure --help");
            std::process::exit(2);
        }
    };
    if args.help {
        print!("{}", cli::HELP);
        return;
    }
    if args.version {
        println!(
            "pdf_measure {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("PDF_MEASURE_COMMIT")
        );
        return;
    }

    #[cfg(windows)]
    {
        std::process::exit(win::run(&args));
    }

    #[cfg(not(windows))]
    {
        eprintln!(
            "pdf_measure {} ({}) is a non-Windows stub. Windows.Data.Pdf timing runs only in pdf_measure.exe on Windows. Refusing to measure {} file(s) here.",
            env!("CARGO_PKG_VERSION"),
            env!("PDF_MEASURE_COMMIT"),
            args.paths.len()
        );
    }
}
