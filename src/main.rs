// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

#[macro_use]
mod report;

mod args;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod blind_spots;
#[cfg(target_os = "linux")]
mod compiler;
mod fetch;
mod handoff;
#[cfg(target_os = "linux")]
mod linux;
mod util;
// The trace model and writer are portable; only recording is Linux-specific.
// Building them everywhere keeps the writer's unit tests running on every
// platform the viewer ships on.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod model;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod perfetto;

use args::{Handoff, Wait};
use handoff::open_in_ui;
use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    report::init();
    #[cfg(target_os = "linux")]
    if let Some(code) = compiler::run_wrapper() {
        return code;
    }

    let args = args::parse();
    match args {
        args::Args::Record {
            output,
            command,
            compiler_traces,
            file_events,
            detach_after_exit,
            run_as,
            handoff,
            wait,
        } => record(
            output,
            command,
            compiler_traces,
            file_events,
            detach_after_exit,
            run_as,
            handoff,
            wait,
        ),
        args::Args::Open {
            source,
            url,
            handoff,
            wait,
        } => open_in_ui(&source, &url, handoff, wait, false),
        args::Args::Examples => list_examples(),
    }
}

fn list_examples() -> ExitCode {
    let width = args::EXAMPLES
        .iter()
        .map(|example| example.name.len())
        .max()
        .unwrap_or_default();
    for example in args::EXAMPLES {
        let stdout = yansi::Condition::cached(std::io::stdout().is_terminal());
        println!(
            "{:width$}  {}",
            yansi::Painted::new(example.name).bold().whenever(stdout),
            example.description
        );
        println!(
            "{:width$}  {}",
            "",
            yansi::Painted::new(format_args!("buildprof open --example {}", example.name))
                .dim()
                .whenever(stdout)
        );
    }
    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn record(
    output: std::path::PathBuf,
    command: Vec<std::ffi::OsString>,
    compiler_traces: bool,
    file_events: bool,
    detach_after_exit: Option<std::time::Duration>,
    run_as: Option<String>,
    handoff: Option<Handoff>,
    wait: Wait,
) -> ExitCode {
    let run_as = match run_as.as_deref().map(linux::RunAs::resolve).transpose() {
        Ok(run_as) => run_as,
        Err(error) => {
            error!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let owner = run_as.as_ref().map(linux::RunAs::owner);
    // Leftover processes may outlive the recording; the caller waits only
    // for the front process, which exits when the recording is done.
    let front = match detach_after_exit.map(|_| linux::Front::split()).transpose() {
        Ok(front) => front,
        Err(error) => {
            error!("could not start recording: {error}");
            return ExitCode::FAILURE;
        }
    };
    let options = linux::Options {
        detach_after_exit,
        run_as,
    };
    let mut writer = match perfetto::Writer::create(&output) {
        Ok(writer) => writer,
        Err(error) => {
            error!(
                "could not write {}: {error}",
                report::emph(output.display())
            );
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = writer.collection_options(file_events, compiler_traces) {
        error!("could not write recording options: {error}");
        return ExitCode::FAILURE;
    }
    let mut compilers = compiler::Capture::new(compiler_traces);
    let mut blind_spots = blind_spots::BlindSpots::default();
    let result = linux::record(
        &command,
        &mut writer,
        &mut compilers,
        &mut blind_spots,
        file_events,
        &options,
    );
    let write_result = writer
        .data_loss(&blind_spots.findings())
        .and_then(|()| writer.finish());
    let (exit_code, leftovers) = match result {
        Ok(recorded) => recorded,
        Err(error) => {
            error!("recording failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = write_result {
        error!(
            "could not write {}: {error}",
            report::emph(output.display())
        );
        return ExitCode::FAILURE;
    }
    // The trace belongs to the user the command ran as, not root.
    if let Some((uid, gid)) = owner {
        let _ = std::os::unix::fs::chown(&output, Some(uid), Some(gid));
    }
    blind_spots.report();
    report::gap();
    match handoff {
        Some(handoff) => {
            let _ = open_in_ui(
                &args::Source::Trace(output),
                args::DEFAULT_UI_URL,
                handoff,
                wait,
                true,
            );
        }
        None => head!(
            "Recorded {}; open {} and choose it",
            report::emph(output.display()),
            report::emph(args::DEFAULT_UI_URL)
        ),
    }
    match front {
        Some(front) => front.finish(exit_code, leftovers),
        None => ExitCode::from(exit_code),
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn record(
    _output: std::path::PathBuf,
    _command: Vec<std::ffi::OsString>,
    _compiler_traces: bool,
    _file_events: bool,
    _detach_after_exit: Option<std::time::Duration>,
    _run_as: Option<String>,
    _handoff: Option<Handoff>,
    _wait: Wait,
) -> ExitCode {
    error!("recording needs Linux; this build can only view traces");
    hint!(
        "record on a Linux machine, copy the trace here, and run {}",
        report::emph("buildprof open <TRACE>")
    );
    ExitCode::FAILURE
}
