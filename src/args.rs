use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
#[derive(Debug, usage::Cli)]
#[usage(
    bin = "buildprof",
    version,
    about = "Capture a build as an interactive trace of processes, timing, and file access.",
    usage = "Usage: buildprof [FLAGS] -- <COMMAND>…\n       buildprof [FLAGS] <SUBCOMMAND>",
    example(
        "buildprof -- cargo build --release",
        help = "Record a build without spelling the optional `record` subcommand."
    ),
    example(
        "buildprof -o clean-build.buildprof -- make -j8",
        help = "Choose the output path."
    ),
    example(
        "buildprof open clean-build.buildprof",
        help = "Open an existing trace."
    ),
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    unknown_flags = "error"
)]
struct Cli {
    /// Output trace path. [default: output.buildprof]
    #[usage(
        short = 'o',
        long,
        default = "output.buildprof",
        hide_default_value = true,
        global
    )]
    output: PathBuf,

    /// Collect supported compiler-internal traces.
    #[usage(long, global)]
    compiler_traces: bool,

    /// Do not open the completed trace in a browser.
    #[usage(long, global)]
    no_open: bool,

    /// Command to record when the `record` subcommand is omitted.
    #[usage(required, double_dash = "required", hide = true)]
    command: Vec<OsString>,

    #[usage(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, usage::Subcommands)]
enum Action {
    /// Record a command and all of the processes it starts.
    #[usage(
        display_order = 0,
        example("buildprof record -- cargo build --release")
    )]
    Record {
        /// Command to record. Arguments after `--` are passed through unchanged.
        #[usage(required, double_dash = "optional")]
        command: Vec<OsString>,
    },
    /// Serve a trace locally and open it in the Buildprof UI.
    #[usage(display_order = 1, example("buildprof open clean-build.buildprof"))]
    Open {
        /// Path to the trace to open.
        trace: PathBuf,

        /// Open the trace in the local Buildprof development server.
        #[usage(long, conflicts("url"))]
        dev_server: bool,

        /// Buildprof UI URL to open the trace in.
        #[usage(long, value_name = "URL", conflicts("dev-server"))]
        url: Option<String>,
    },
}

#[derive(Debug)]
pub enum Args {
    Record {
        output: PathBuf,
        command: Vec<OsString>,
        compiler_traces: bool,
        open: bool,
    },
    Open {
        trace: PathBuf,
        url: String,
    },
}

pub const DEFAULT_UI_URL: &str = "https://buildprof.lalitm.com";
pub const DEV_UI_URL: &str = "http://localhost:10000";

pub fn parse() -> Args {
    from_cli(Cli::parse())
}

fn from_cli(cli: Cli) -> Args {
    match cli.action {
        Some(Action::Open {
            trace,
            dev_server,
            url,
        }) => Args::Open {
            trace,
            url: url.unwrap_or_else(|| {
                if dev_server {
                    DEV_UI_URL.to_owned()
                } else {
                    DEFAULT_UI_URL.to_owned()
                }
            }),
        },
        Some(Action::Record { command }) => Args::Record {
            output: cli.output,
            command,
            compiler_traces: cli.compiler_traces,
            open: !cli.no_open && default_open(),
        },
        None => Args::Record {
            output: cli.output,
            command: cli.command,
            compiler_traces: cli.compiler_traces,
            open: !cli.no_open && default_open(),
        },
    }
}

fn default_open() -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strings(args: &[&str]) -> Args {
        let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        from_cli(Cli::parse_from_argv(&argv).unwrap())
    }

    fn recording(args: &[&str]) -> (PathBuf, Vec<OsString>, bool, bool) {
        let Args::Record {
            output,
            command,
            compiler_traces,
            open,
        } = parse_strings(args)
        else {
            panic!("expected record arguments")
        };
        (output, command, compiler_traces, open)
    }

    #[test]
    fn parses_explicit_record_subcommand() {
        let (output, command, _, _) = recording(&[
            "buildprof",
            "record",
            "-o",
            "out.pftrace",
            "--",
            "make",
            "-j8",
        ]);
        assert_eq!(output, PathBuf::from("out.pftrace"));
        assert_eq!(command, [OsString::from("make"), OsString::from("-j8")]);
    }

    #[test]
    fn defaults_to_recording_without_subcommand() {
        let (output, command, compiler_traces, _) = recording(&["buildprof", "--", "make"]);
        assert_eq!(output, PathBuf::from("output.buildprof"));
        assert_eq!(command, [OsString::from("make")]);
        assert!(!compiler_traces);
    }

    #[test]
    fn compiler_traces_can_be_enabled() {
        let (_, _, compiler_traces, _) =
            recording(&["buildprof", "--compiler-traces", "--", "make"]);
        assert!(compiler_traces);
    }

    #[test]
    fn opening_can_be_disabled() {
        assert!(!recording(&["buildprof", "--no-open", "--", "make"]).3);
    }

    #[test]
    fn parses_open_subcommand() {
        let Args::Open { trace, url } = parse_strings(&["buildprof", "open", "trace.pftrace"])
        else {
            panic!("expected open arguments")
        };
        assert_eq!(trace, PathBuf::from("trace.pftrace"));
        assert_eq!(url, DEFAULT_UI_URL);
    }

    #[test]
    fn open_supports_dev_server_and_custom_url() {
        let Args::Open { url, .. } =
            parse_strings(&["buildprof", "open", "--dev-server", "trace.pftrace"])
        else {
            panic!("expected open arguments")
        };
        assert_eq!(url, DEV_UI_URL);

        let Args::Open { url, .. } = parse_strings(&[
            "buildprof",
            "open",
            "trace.pftrace",
            "--url",
            "https://example.com/ui/",
        ]) else {
            panic!("expected open arguments")
        };
        assert_eq!(url, "https://example.com/ui/");
    }
}
