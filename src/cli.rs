/// What `spacepeek.exe` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// No arguments. Run until the user exits.
    Run,
    /// `--quit`. Ask the running instance to exit.
    Quit,
    /// `--help`, `-h`, or `/?`.
    Help,
    /// `--version` or `-V`.
    Version,
    /// An argument this build does not understand.
    Unknown,
}

/// Parse process arguments. The first item is the program name and is ignored.
///
/// `--quit` is the only way to ask a running instance to exit from the command
/// line. `--version` / `-V` prints the build version. Anything else with an
/// argument is [`Command::Unknown`], so a typo cannot start a second previewer.
pub fn parse_command<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    let Some(flag) = args.next() else {
        return Command::Run;
    };
    if args.next().is_some() {
        return Command::Unknown;
    }
    let flag = flag.as_ref();
    if flag.eq_ignore_ascii_case("--quit") {
        Command::Quit
    } else if flag.eq_ignore_ascii_case("--help")
        || flag.eq_ignore_ascii_case("-h")
        || flag.eq_ignore_ascii_case("/?")
    {
        Command::Help
    } else if flag.eq_ignore_ascii_case("--version") || flag.eq_ignore_ascii_case("-V") {
        Command::Version
    } else {
        Command::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_runs() {
        assert_eq!(parse_command(["spacepeek"]), Command::Run);
        assert_eq!(parse_command(["C:\\Tools\\spacepeek.exe"]), Command::Run);
        assert_eq!(parse_command(std::iter::empty::<&str>()), Command::Run);
    }

    #[test]
    fn quit_flag() {
        assert_eq!(parse_command(["spacepeek.exe", "--quit"]), Command::Quit);
        assert_eq!(parse_command(["spacepeek", "--QUIT"]), Command::Quit);
    }

    #[test]
    fn help_and_unknown() {
        assert_eq!(parse_command(["spacepeek", "--help"]), Command::Help);
        assert_eq!(parse_command(["spacepeek", "-h"]), Command::Help);
        assert_eq!(parse_command(["spacepeek", "/?"]), Command::Help);
        assert_eq!(parse_command(["spacepeek", "--nope"]), Command::Unknown);
        assert_eq!(
            parse_command(["spacepeek", "--quit", "now"]),
            Command::Unknown
        );
        assert_eq!(parse_command(["spacepeek", "-quit"]), Command::Unknown);
    }

    #[test]
    fn version_flag() {
        assert_eq!(parse_command(["spacepeek", "--version"]), Command::Version);
        assert_eq!(parse_command(["spacepeek", "--VERSION"]), Command::Version);
        assert_eq!(parse_command(["spacepeek", "-V"]), Command::Version);
        assert_eq!(parse_command(["spacepeek", "-v"]), Command::Version);
        assert_eq!(
            parse_command(["spacepeek", "--version", "now"]),
            Command::Unknown
        );
    }
}
