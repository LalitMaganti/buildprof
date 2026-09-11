// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

#[macro_use]
mod report;

mod args;
#[cfg(target_os = "linux")]
mod compiler;
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

use args::{Handoff, Source, Wait};
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

#[cfg(target_os = "linux")]
fn record(
    output: std::path::PathBuf,
    command: Vec<std::ffi::OsString>,
    compiler_traces: bool,
    file_events: bool,
    handoff: Option<Handoff>,
    wait: Wait,
) -> ExitCode {
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
    let result = linux::record(&command, &mut writer, &mut compilers, file_events);
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
    report::gap();
    match handoff {
        Some(handoff) => {
            let _ = open_in_ui(
                &Source::Trace(output),
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

#[cfg(not(target_os = "linux"))]
fn record(
    _output: std::path::PathBuf,
    _command: Vec<std::ffi::OsString>,
    _compiler_traces: bool,
    _file_events: bool,
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
