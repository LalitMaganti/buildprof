// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

#[macro_use]
mod report;

mod args;
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
mod blind_spots;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod compiler;
mod handoff;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod util;
// The trace model and writer are portable; only recording is platform-specific.
// Building them everywhere keeps the writer's unit tests running on every
// platform the viewer ships on.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
mod model;
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
mod perfetto;

use args::{Handoff, Wait};
use handoff::open_in_ui;
use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    report::init();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            handoff,
            wait,
        } => record(output, command, compiler_traces, file_events, handoff, wait),
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn record(
    output: std::path::PathBuf,
    command: Vec<std::ffi::OsString>,
    compiler_traces: bool,
    file_events: bool,
    handoff: Option<Handoff>,
    wait: Wait,
) -> ExitCode {
    // Collection starts privileged and drops privileges before anything is
    // created, so the trace and the build belong to whoever ran the recording.
    #[cfg(target_os = "macos")]
    let prepared = match macos::prepare() {
        Ok(prepared) => prepared,
        Err(error) => {
            error!("could not start recording: {error}");
            return ExitCode::FAILURE;
        }
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
    #[cfg(target_os = "linux")]
    let result = linux::record(
        &command,
        &mut writer,
        &mut compilers,
        &mut blind_spots,
        file_events,
    );
    #[cfg(target_os = "macos")]
    let result = macos::record(
        prepared,
        &command,
        &mut writer,
        &mut compilers,
        &mut blind_spots,
        file_events,
    );
    let write_result = writer.finish();
    let exit_code = match result {
        Ok(exit_code) => exit_code,
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
    ExitCode::from(exit_code)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn record(
    _output: std::path::PathBuf,
    _command: Vec<std::ffi::OsString>,
    _compiler_traces: bool,
    _file_events: bool,
    _handoff: Option<Handoff>,
    _wait: Wait,
) -> ExitCode {
    error!("recording needs Linux or macOS; this build can only view traces");
    hint!(
        "record on a Linux machine, copy the trace here, and run {}",
        report::emph("buildprof open <TRACE>")
    );
    ExitCode::FAILURE
}
