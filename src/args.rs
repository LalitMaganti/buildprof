use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

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
        "buildprof -o clean-build.buildprof --no-open -- make -j8",
        help = "Choose the output path and skip opening the browser."
    ),
    example(
        "buildprof open clean-build.buildprof",
        help = "Open an existing trace."
    ),
    example(
        "buildprof open --example ripgrep",
        help = "Explore a hosted example recording."
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

    /// Seconds to wait for a browser to fetch the trace; 0 waits forever.
    /// [default: 600]
    #[usage(
        long,
        value_name = "SECONDS",
        default = "600",
        hide_default_value = true,
        global
    )]
    wait: u64,

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
    #[usage(
        display_order = 1,
        example("buildprof open clean-build.buildprof"),
        example("buildprof open --example ripgrep")
    )]
    Open {
        /// Path to the trace to open.
        #[usage(required_unless("example"), conflicts("example"))]
        trace: Option<PathBuf>,

        /// Open a hosted example recording instead; see `buildprof examples`.
        #[usage(long, value_name = "NAME", conflicts("trace"))]
        example: Option<String>,

        /// Open the trace in the local Buildprof development server.
        #[usage(long, conflicts("url"))]
        dev_server: bool,

        /// Buildprof UI URL to open the trace in.
        #[usage(long, value_name = "URL", conflicts("dev-server"))]
        url: Option<String>,
    },
    /// List the hosted example recordings.
    #[usage(display_order = 2, example("buildprof examples"))]
    Examples,
}

/// How a finished trace reaches a browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// Launch a browser on this machine.
    Browser,
    /// Print port-forwarding instructions and wait for a browser elsewhere.
    Ssh,
}

/// What `open` should show.
#[derive(Debug)]
pub enum Source {
    Trace(PathBuf),
    Example(&'static Example),
}

/// A recording hosted next to the UI.
#[derive(Debug)]
pub struct Example {
    pub name: &'static str,
    pub file: &'static str,
    pub description: &'static str,
}

pub const EXAMPLES: &[Example] = &[Example {
    name: "ripgrep",
    file: "ripgrep-release-clean.buildprof",
    description: "A clean `cargo build --release` of ripgrep",
}];

/// How long to wait for a browser; `None` waits forever.
pub type Wait = Option<Duration>;

#[derive(Debug)]
pub enum Args {
    Record {
        output: PathBuf,
        command: Vec<OsString>,
        compiler_traces: bool,
        /// `None` leaves the trace on disk and prints where to open it.
        handoff: Option<Handoff>,
        wait: Wait,
    },
    Open {
        source: Source,
        url: String,
        handoff: Handoff,
        wait: Wait,
    },
    Examples,
}

/// Every release of the UI stays deployed under its own version directory, so
/// a CLI always opens the UI it was released with.
pub const DEFAULT_UI_URL: &str =
    concat!("https://buildprof.lalitm.com/v", env!("CARGO_PKG_VERSION"));
pub const DEV_UI_URL: &str = "http://localhost:10000";
pub const EXAMPLES_URL: &str = "https://buildprof.lalitm.com/examples";

pub fn parse() -> Args {
    match from_cli(Cli::parse()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("buildprof: {message}");
            std::process::exit(2);
        }
    }
}

fn from_cli(cli: Cli) -> Result<Args, String> {
    let wait = (cli.wait > 0).then(|| Duration::from_secs(cli.wait));
    let record = |command: Vec<OsString>| Args::Record {
        output: cli.output.clone(),
        command,
        compiler_traces: cli.compiler_traces,
        handoff: if cli.no_open { None } else { default_handoff() },
        wait,
    };
    Ok(match cli.action {
        Some(Action::Open {
            trace,
            example,
            dev_server,
            url,
        }) => {
            let source = match (trace, example) {
                (Some(trace), None) => Source::Trace(trace),
                (None, Some(name)) => Source::Example(find_example(&name)?),
                _ => return Err("open needs a trace path or --example <NAME>".to_owned()),
            };
            Args::Open {
                source,
                url: url.unwrap_or_else(|| {
                    if dev_server {
                        DEV_UI_URL.to_owned()
                    } else {
                        DEFAULT_UI_URL.to_owned()
                    }
                }),
                handoff: if in_ssh_session() {
                    Handoff::Ssh
                } else {
                    Handoff::Browser
                },
                wait,
            }
        }
        Some(Action::Examples) => Args::Examples,
        Some(Action::Record { command }) => record(command),
        None => record(cli.command),
    })
}

fn find_example(name: &str) -> Result<&'static Example, String> {
    EXAMPLES
        .iter()
        .find(|example| example.name == name)
        .ok_or_else(|| {
            let names: Vec<&str> = EXAMPLES.iter().map(|example| example.name).collect();
            format!("unknown example `{name}`; available: {}", names.join(", "))
        })
}

/// Whether this terminal is at the far end of an SSH connection, in which
/// case a browser launched here would be invisible.
pub fn in_ssh_session() -> bool {
    ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]
        .iter()
        .any(|variable| std::env::var_os(variable).is_some())
}

fn default_handoff() -> Option<Handoff> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    if in_ssh_session() {
        return Some(Handoff::Ssh);
    }

    #[cfg(target_os = "linux")]
    {
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        has_display.then_some(Handoff::Browser)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Some(Handoff::Browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strings(args: &[&str]) -> Args {
        let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        from_cli(Cli::parse_from_argv(&argv).unwrap()).unwrap()
    }

    struct Recording {
        output: PathBuf,
        command: Vec<OsString>,
        compiler_traces: bool,
        handoff: Option<Handoff>,
        wait: Wait,
    }

    fn recording(args: &[&str]) -> Recording {
        let Args::Record {
            output,
            command,
            compiler_traces,
            handoff,
            wait,
        } = parse_strings(args)
        else {
            panic!("expected record arguments")
        };
        Recording {
            output,
            command,
            compiler_traces,
            handoff,
            wait,
        }
    }

    #[test]
    fn parses_explicit_record_subcommand() {
        let recording = recording(&[
            "buildprof",
            "record",
            "-o",
            "out.pftrace",
            "--",
            "make",
            "-j8",
        ]);
        assert_eq!(recording.output, PathBuf::from("out.pftrace"));
        assert_eq!(
            recording.command,
            [OsString::from("make"), OsString::from("-j8")]
        );
    }

    #[test]
    fn defaults_to_recording_without_subcommand() {
        let recording = recording(&["buildprof", "--", "make"]);
        assert_eq!(recording.output, PathBuf::from("output.buildprof"));
        assert_eq!(recording.command, [OsString::from("make")]);
        assert!(!recording.compiler_traces);
        assert_eq!(recording.wait, Some(Duration::from_secs(600)));
    }

    #[test]
    fn compiler_traces_can_be_enabled() {
        assert!(recording(&["buildprof", "--compiler-traces", "--", "make"]).compiler_traces);
    }

    #[test]
    fn opening_can_be_disabled() {
        assert_eq!(
            recording(&["buildprof", "--no-open", "--", "make"]).handoff,
            None
        );
    }

    #[test]
    fn wait_can_be_disabled() {
        assert_eq!(
            recording(&["buildprof", "--wait", "0", "--", "make"]).wait,
            None
        );
    }

    #[test]
    fn parses_open_subcommand() {
        let Args::Open {
            source: Source::Trace(trace),
            url,
            wait,
            ..
        } = parse_strings(&["buildprof", "open", "--wait", "30", "trace.pftrace"])
        else {
            panic!("expected open arguments for a trace")
        };
        assert_eq!(trace, PathBuf::from("trace.pftrace"));
        assert_eq!(url, DEFAULT_UI_URL);
        assert_eq!(wait, Some(Duration::from_secs(30)));
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

    #[test]
    fn open_can_select_a_hosted_example() {
        let Args::Open {
            source: Source::Example(example),
            ..
        } = parse_strings(&["buildprof", "open", "--example", "ripgrep"])
        else {
            panic!("expected open arguments for an example")
        };
        assert_eq!(example.name, "ripgrep");
    }

    #[test]
    fn unknown_examples_are_rejected_with_the_available_names() {
        let argv = ["buildprof", "open", "--example", "nope"].map(std::ffi::OsStr::new);
        let error = from_cli(Cli::parse_from_argv(&argv).unwrap()).unwrap_err();
        assert!(error.contains("nope"));
        assert!(error.contains("ripgrep"));
    }

    #[test]
    fn open_requires_a_trace_or_an_example() {
        let argv = ["buildprof", "open"].map(std::ffi::OsStr::new);
        assert!(Cli::parse_from_argv(&argv).is_err());
    }

    #[test]
    fn lists_examples() {
        assert!(matches!(
            parse_strings(&["buildprof", "examples"]),
            Args::Examples
        ));
    }
}
