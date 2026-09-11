// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Messages for the person running buildprof, on stderr, with colour when
//! that is a terminal. Prefixed lines can interleave with a build's output;
//! the report printed afterwards stands on its own.

use std::fmt::{self, Display};
use std::io::IsTerminal;
use yansi::{Condition, Painted};

const PREFIX: &str = "buildprof:";

pub fn init() {
    yansi::whenever(Condition::cached(colour_wanted(
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0"),
        std::env::var_os("TERM").is_some_and(|value| value == "dumb"),
        std::io::stderr().is_terminal(),
    )));
}

fn colour_wanted(no_color: bool, force: bool, dumb: bool, terminal: bool) -> bool {
    if no_color {
        return false;
    }
    if force {
        return true;
    }
    terminal && !dumb
}

/// Something the person may need to do, or a recovery step.
pub fn hint(message: fmt::Arguments) {
    eprintln!("{} {message}", Painted::new(PREFIX).yellow().bold());
}

pub fn error(message: fmt::Arguments) {
    eprintln!("{} {message}", Painted::new(PREFIX).red().bold());
}

/// The report after a build or handoff starts is set apart from log lines
/// and from the build's own output: no prefix, a bold headline, and blank
/// lines around anything to copy.
pub fn head(message: fmt::Arguments) {
    // wrap() restores the bold after an emphasised value inside the line.
    eprintln!("{}", Painted::new(message).bold().wrap());
}

pub fn line(message: fmt::Arguments) {
    eprintln!("{message}");
}

/// A report line the reader may need to act on.
pub fn note(message: fmt::Arguments) {
    eprintln!("{}", Painted::new(message).yellow());
}

/// A command or URL on its own indented line, ready to copy.
pub fn detail(text: fmt::Arguments) {
    eprintln!("    {}", emph(text));
}

pub fn gap() {
    eprintln!();
}

/// A path, URL, or command mentioned inside a message.
pub fn emph<T: Display>(value: T) -> Painted<T> {
    Painted::new(value).cyan()
}

macro_rules! hint {
    ($($arg:tt)*) => { $crate::report::hint(format_args!($($arg)*)) };
}
macro_rules! error {
    ($($arg:tt)*) => { $crate::report::error(format_args!($($arg)*)) };
}
macro_rules! head {
    ($($arg:tt)*) => { $crate::report::head(format_args!($($arg)*)) };
}
macro_rules! line {
    ($($arg:tt)*) => { $crate::report::line(format_args!($($arg)*)) };
}
macro_rules! note {
    ($($arg:tt)*) => { $crate::report::note(format_args!($($arg)*)) };
}
macro_rules! detail {
    ($($arg:tt)*) => { $crate::report::detail(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::colour_wanted;

    #[test]
    fn colour_follows_the_terminal_and_the_conventions() {
        assert!(colour_wanted(false, false, false, true));
        assert!(!colour_wanted(false, false, false, false));
        assert!(!colour_wanted(true, false, false, true), "NO_COLOR wins");
        assert!(
            colour_wanted(false, true, false, false),
            "CLICOLOR_FORCE wins over a pipe"
        );
        assert!(
            !colour_wanted(true, true, false, true),
            "NO_COLOR wins over CLICOLOR_FORCE"
        );
        assert!(
            !colour_wanted(false, false, true, true),
            "TERM=dumb disables colour"
        );
    }
}
