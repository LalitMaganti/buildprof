//! Best-effort compiler-internal trace capture.
//!
//! Compiler flags are added by private wrappers. This is opt-in and may change
//! compiler cache keys; requesting the detail trace takes precedence over
//! preserving cache hits.

use crate::perfetto::Writer;
use analyzeme::{EventPayload, ProfilingData, Timestamp};
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::HashSet;
use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const PROFILE_DIR_ENV: &str = "BUILDPROF_COMPILER_PROFILE_DIR";
const REAL_RUST_WRAPPER_ENV: &str = "BUILDPROF_REAL_RUSTC_WRAPPER";
const REAL_CLANG_ENV: &str = "BUILDPROF_REAL_CLANG";
const REAL_CLANGXX_ENV: &str = "BUILDPROF_REAL_CLANGXX";
const RUST_WRAPPER_KIND: &str = "buildprof-rustc-wrapper";
const CLANG_WRAPPER_KIND: &str = "clang";
const CLANGXX_WRAPPER_KIND: &str = "clang++";
const RUST_BACKEND: &str = "Rust";
const CLANG_BACKEND: &str = "Clang";
const LLD_BACKEND: &str = "LLD";
const PROFILE_DIRECTORY_PREFIX: &str = "buildprof-compilers";
const CLANG_TRACE_PREFIX: &str = "clang";
const LLD_TRACE_PREFIX: &str = "lld";
const CLANG_TRACE_EXTENSION: &str = "json";
const RUST_PROFILE_EXTENSION: &str = "mm_profdata";
// Compiler activities are the user-facing phase timeline. Rust's query
// provider stream is a separate expert-level dataset, not a finer sampling of
// this one, and can be added later without changing what this trace means.
const RUST_SELF_PROFILE_EVENTS: &str = "generic-activity";
const RUST_INFORMATION_FLAGS: &[&str] = &["--version", "-V", "-vV", "--print"];
const RUST_PRINT_FLAG_PREFIX: &[u8] = b"--print=";
const WRAPPER_DIRECTORY_NAME: &str = "bin";
const MINIMUM_COMPILER_EVENT_NS: u64 = 1;
const NANOS_PER_MICROSECOND: f64 = 1_000.0;
const WRAPPER_FAILURE_EXIT_CODE: u8 = 126;

pub struct Capture {
    directory: Option<PathBuf>,
    child_environment: Vec<(CString, CString)>,
    origin: SystemTime,
    declared_tracks: HashSet<(i32, u32, &'static str)>,
}

impl Capture {
    pub fn new(enabled: bool) -> Self {
        let mut capture = Self {
            directory: None,
            child_environment: Vec::new(),
            origin: UNIX_EPOCH,
            declared_tracks: HashSet::new(),
        };
        if enabled {
            if let Err(error) = capture.prepare() {
                eprintln!("buildprof: compiler tracing unavailable: {error}");
            }
        }
        capture
    }

    pub fn set_origin(&mut self, origin: SystemTime) {
        self.origin = origin;
    }

    /// Apply already-prepared variables in the forked child without allocating.
    pub unsafe fn configure_child(&self) {
        for (name, value) in &self.child_environment {
            // SAFETY: both strings are retained by `self`, NUL terminated, and
            // this recorder is single-threaded when it forks.
            unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) };
        }
    }

    pub fn process_exited(&mut self, pid: i32, process_start_ns: u64, writer: &mut Writer) {
        let Some(directory) = self.directory.clone() else {
            return;
        };

        let clang = directory.join(format!(
            "{CLANG_TRACE_PREFIX}-{pid}.{CLANG_TRACE_EXTENSION}"
        ));
        if clang.is_file() {
            if let Err(error) =
                self.import_time_trace(pid, process_start_ns, CLANG_BACKEND, &clang, writer)
            {
                eprintln!("buildprof: could not import {}: {error}", clang.display());
            }
            let _ = fs::remove_file(clang);
        }

        let lld = directory.join(format!("{LLD_TRACE_PREFIX}-{pid}.{CLANG_TRACE_EXTENSION}"));
        if lld.is_file() {
            if let Err(error) =
                self.import_time_trace(pid, process_start_ns, LLD_BACKEND, &lld, writer)
            {
                eprintln!("buildprof: could not import {}: {error}", lld.display());
            }
            let _ = fs::remove_file(lld);
        }

        let Ok(entries) = fs::read_dir(&directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if rust_profile_pid(&path) == Some(pid) {
                if let Err(error) = self.import_rust(pid, &path, writer) {
                    eprintln!("buildprof: could not import {}: {error}", path.display());
                }
                let _ = fs::remove_file(path);
            }
        }
    }

    fn prepare(&mut self) -> io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = env::temp_dir().join(format!(
            "{PROFILE_DIRECTORY_PREFIX}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        fs::create_dir(directory.join(WRAPPER_DIRECTORY_NAME))?;
        self.directory = Some(directory.clone());
        self.push_env(PROFILE_DIR_ENV, directory.as_os_str())?;

        self.prepare_rust()?;
        self.prepare_clang()?;
        Ok(())
    }

    fn prepare_rust(&mut self) -> io::Result<()> {
        let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
        let version = Command::new(&rustc).arg("--version").output();
        let is_nightly = version
            .ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| {
                let text = String::from_utf8_lossy(&output.stdout);
                text.contains("nightly") || text.contains("-dev")
            });
        if !is_nightly {
            return Ok(());
        }
        let wrapper = self.wrapper_path(RUST_WRAPPER_KIND);
        symlink(env::current_exe()?, &wrapper)?;
        if let Some(existing) = env::var_os("RUSTC_WRAPPER") {
            self.push_env(REAL_RUST_WRAPPER_ENV, &existing)?;
        }
        self.push_env("RUSTC_WRAPPER", wrapper.as_os_str())
    }

    fn prepare_clang(&mut self) -> io::Result<()> {
        let Some(clang) = find_on_path("clang") else {
            return Ok(());
        };
        let Some(clangxx) = find_on_path("clang++") else {
            return Ok(());
        };

        let shim_directory = self
            .wrapper_path(CLANG_WRAPPER_KIND)
            .parent()
            .expect("wrapper has a parent")
            .to_owned();
        let executable = env::current_exe()?;
        symlink(&executable, shim_directory.join(CLANG_WRAPPER_KIND))?;
        symlink(&executable, shim_directory.join(CLANGXX_WRAPPER_KIND))?;

        self.push_env(REAL_CLANG_ENV, clang.as_os_str())?;
        self.push_env(REAL_CLANGXX_ENV, clangxx.as_os_str())?;
        let old_path = env::var_os("PATH").unwrap_or_default();
        let mut path = shim_directory.into_os_string();
        path.push(":");
        path.push(old_path);
        self.push_env("PATH", path.as_os_str())
    }

    fn wrapper_path(&self, name: &str) -> PathBuf {
        self.directory
            .as_ref()
            .expect("prepared directory")
            .join(WRAPPER_DIRECTORY_NAME)
            .join(name)
    }

    fn push_env(&mut self, name: &str, value: &OsStr) -> io::Result<()> {
        let name = CString::new(name).expect("constant has no NUL");
        let value = CString::new(value.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment contains a NUL byte",
            )
        })?;
        self.child_environment.push((name, value));
        Ok(())
    }

    fn import_time_trace(
        &mut self,
        pid: i32,
        process_start_ns: u64,
        backend: &'static str,
        path: &Path,
        writer: &mut Writer,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let trace_start_ns = time_trace_start_ns(path, self.origin).unwrap_or(process_start_ns);
        let input = BufReader::new(File::open(path)?);
        let mut deserializer = serde_json::Deserializer::from_reader(input);
        ClangProfileSeed {
            capture: self,
            writer,
            pid,
            process_start_ns: trace_start_ns,
            backend,
        }
        .deserialize(&mut deserializer)?;
        Ok(())
    }

    fn import_rust(
        &mut self,
        pid: i32,
        path: &Path,
        writer: &mut Writer,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let stem = path.with_extension("");
        let profile = ProfilingData::new(&stem)?;
        for event in profile.iter_full() {
            let EventPayload::Timestamp(Timestamp::Interval { start, end }) = event.payload else {
                continue;
            };
            let start_ns = system_time_ns(start, self.origin);
            let duration_ns = system_time_ns(end, start).max(MINIMUM_COMPILER_EVENT_NS);
            self.write_event(
                writer,
                pid,
                event.thread_id,
                RUST_BACKEND,
                &event.label,
                &event.event_kind,
                start_ns,
                duration_ns,
                None,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_event(
        &mut self,
        writer: &mut Writer,
        pid: i32,
        thread_id: u32,
        backend: &'static str,
        name: &str,
        category: &str,
        start_ns: u64,
        duration_ns: u64,
        detail: Option<&str>,
    ) -> io::Result<()> {
        if self.declared_tracks.insert((pid, thread_id, backend)) {
            writer.compiler_track(pid, thread_id, backend)?;
        }
        writer.compiler_slice(
            pid,
            thread_id,
            backend,
            category,
            name,
            start_ns,
            duration_ns,
            detail,
        )
    }
}

#[derive(Deserialize)]
struct TimeTraceMetadata {
    #[serde(rename = "beginningOfTime")]
    beginning_of_time_us: Option<u64>,
}

fn time_trace_start_ns(path: &Path, origin: SystemTime) -> Option<u64> {
    let input = BufReader::new(File::open(path).ok()?);
    let metadata: TimeTraceMetadata = serde_json::from_reader(input).ok()?;
    let start = UNIX_EPOCH.checked_add(std::time::Duration::from_micros(
        metadata.beginning_of_time_us?,
    ))?;
    Some(system_time_ns(start, origin))
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(directory) = &self.directory {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

#[derive(Deserialize)]
struct ClangEvent {
    #[serde(rename = "ph")]
    phase: String,
    name: String,
    #[serde(rename = "cat", default)]
    category: String,
    #[serde(rename = "ts")]
    timestamp_us: f64,
    #[serde(rename = "dur", default)]
    duration_us: f64,
    #[serde(rename = "tid", default)]
    thread_id: u32,
    #[serde(default)]
    args: ClangEventArgs,
}

#[derive(Default, Deserialize)]
struct ClangEventArgs {
    detail: Option<String>,
}

struct ClangProfileSeed<'capture, 'writer> {
    capture: &'capture mut Capture,
    writer: &'writer mut Writer,
    pid: i32,
    process_start_ns: u64,
    backend: &'static str,
}

impl<'de> DeserializeSeed<'de> for ClangProfileSeed<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ClangProfileVisitor(self))
    }
}

struct ClangProfileVisitor<'capture, 'writer>(ClangProfileSeed<'capture, 'writer>);

impl<'de> Visitor<'de> for ClangProfileVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a Clang time-trace object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seed = Some(self.0);
        while let Some(key) = map.next_key::<String>()? {
            if key == "traceEvents" {
                let profile = seed
                    .take()
                    .ok_or_else(|| serde::de::Error::custom("duplicate Clang traceEvents field"))?;
                map.next_value_seed(ClangEventsSeed(profile))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct ClangEventsSeed<'capture, 'writer>(ClangProfileSeed<'capture, 'writer>);

impl<'de> DeserializeSeed<'de> for ClangEventsSeed<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ClangEventsVisitor(self.0))
    }
}

struct ClangEventsVisitor<'capture, 'writer>(ClangProfileSeed<'capture, 'writer>);

impl<'de> Visitor<'de> for ClangEventsVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the Clang traceEvents array")
    }

    fn visit_seq<A>(self, mut events: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(event) = events.next_element::<ClangEvent>()? {
            if event.phase != "X" || event.name.starts_with("Total ") {
                continue;
            }
            let start_ns = self
                .0
                .process_start_ns
                .saturating_add(micros_to_nanos(event.timestamp_us));
            let duration_ns = micros_to_nanos(event.duration_us).max(MINIMUM_COMPILER_EVENT_NS);
            self.0
                .capture
                .write_event(
                    self.0.writer,
                    self.0.pid,
                    event.thread_id,
                    self.0.backend,
                    &event.name,
                    &event.category,
                    start_ns,
                    duration_ns,
                    event.args.detail.as_deref(),
                )
                .map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}

pub fn run_wrapper() -> Option<ExitCode> {
    let invoked_as = env::args_os()
        .next()
        .and_then(|path| PathBuf::from(path).file_name().map(OsStr::to_owned));
    let kind = invoked_as
        .as_deref()
        .and_then(OsStr::to_str)
        .filter(|kind| {
            matches!(
                *kind,
                RUST_WRAPPER_KIND | CLANG_WRAPPER_KIND | CLANGXX_WRAPPER_KIND
            )
        });
    let kind = kind?;

    let profile_dir = env::var_os(PROFILE_DIR_ENV)?;
    let mut arguments = env::args_os().skip(1);
    let mut command = match kind {
        RUST_WRAPPER_KIND => {
            let rustc = arguments.next()?;
            let mut command = if let Some(wrapper) = env::var_os(REAL_RUST_WRAPPER_ENV) {
                let mut command = Command::new(wrapper);
                command.arg(rustc);
                command
            } else {
                Command::new(rustc)
            };
            let mut profile = true;
            for argument in arguments {
                profile &= !is_rust_information_argument(&argument);
                command.arg(argument);
            }
            if profile {
                command.arg(format!(
                    "-Zself-profile={}",
                    Path::new(&profile_dir).display()
                ));
                command.arg(format!("-Zself-profile-events={RUST_SELF_PROFILE_EVENTS}"));
            }
            command
        }
        CLANG_WRAPPER_KIND | CLANGXX_WRAPPER_KIND => {
            let variable = if kind == CLANG_WRAPPER_KIND {
                REAL_CLANG_ENV
            } else {
                REAL_CLANGXX_ENV
            };
            let mut command = Command::new(env::var_os(variable)?);
            let arguments: Vec<_> = arguments.collect();
            let trace_lld =
                clang_invocation_links(&arguments) && clang_invocation_uses_lld(&arguments);
            command.args(&arguments);
            let output = Path::new(&profile_dir).join(format!(
                "{CLANG_TRACE_PREFIX}-{}.{}",
                std::process::id(),
                CLANG_TRACE_EXTENSION
            ));
            command.arg(format!("-ftime-trace={}", output.display()));
            if trace_lld {
                let output = Path::new(&profile_dir).join(format!(
                    "{LLD_TRACE_PREFIX}-{}.{}",
                    std::process::id(),
                    CLANG_TRACE_EXTENSION
                ));
                command.arg(format!("-Wl,--time-trace={}", output.display()));
            }
            command
        }
        _ => return None,
    };
    let error = command.exec();
    eprintln!("buildprof: compiler wrapper failed: {error}");
    Some(ExitCode::from(WRAPPER_FAILURE_EXIT_CODE))
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

fn is_rust_information_argument(argument: &OsStr) -> bool {
    RUST_INFORMATION_FLAGS
        .iter()
        .any(|flag| argument == OsStr::new(flag))
        || argument.as_bytes().starts_with(RUST_PRINT_FLAG_PREFIX)
}

fn clang_invocation_links(arguments: &[OsString]) -> bool {
    !arguments.iter().any(|argument| {
        matches!(
            argument.to_str(),
            Some("-c" | "-S" | "-E" | "-M" | "-MM" | "-fsyntax-only" | "-cc1")
        )
    })
}

fn clang_invocation_uses_lld(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        let bytes = argument.as_bytes();
        bytes == b"-fuse-ld=lld"
            || bytes
                .strip_prefix(b"-fuse-ld=")
                .or_else(|| bytes.strip_prefix(b"--ld-path="))
                .is_some_and(|linker| {
                    Path::new(OsStr::from_bytes(linker)).file_name() == Some(OsStr::new("ld.lld"))
                })
    })
}

fn micros_to_nanos(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value * NANOS_PER_MICROSECOND).min(u64::MAX as f64) as u64
    }
}

fn system_time_ns(time: SystemTime, origin: SystemTime) -> u64 {
    time.duration_since(origin)
        .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn rust_profile_pid(path: &Path) -> Option<i32> {
    if path.extension()? != RUST_PROFILE_EXTENSION {
        return None;
    }
    path.file_stem()?.to_str()?.rsplit_once('-')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_zero_padded_rust_profile_pid() {
        assert_eq!(
            rust_profile_pid(Path::new("compiler_fixture-0011754.mm_profdata")),
            Some(11_754)
        );
    }

    #[test]
    fn ignores_other_profile_files() {
        assert_eq!(rust_profile_pid(Path::new("compiler_fixture.json")), None);
        assert_eq!(
            rust_profile_pid(Path::new("compiler_fixture.mm_profdata")),
            None
        );
    }

    #[test]
    fn identifies_rust_information_only_arguments() {
        assert!(is_rust_information_argument(OsStr::new("--print")));
        assert!(is_rust_information_argument(OsStr::new("--print=cfg")));
        assert!(is_rust_information_argument(OsStr::new("-vV")));
        assert!(!is_rust_information_argument(OsStr::new("--crate-name")));
    }

    #[test]
    fn identifies_clang_link_invocations() {
        assert!(clang_invocation_links(&[
            OsString::from("main.o"),
            OsString::from("-o"),
            OsString::from("app"),
        ]));
        assert!(clang_invocation_links(&[
            OsString::from("main.cc"),
            OsString::from("-o"),
            OsString::from("app"),
        ]));
        assert!(!clang_invocation_links(&[
            OsString::from("-c"),
            OsString::from("main.cc"),
        ]));
        assert!(!clang_invocation_links(&[
            OsString::from("-fsyntax-only"),
            OsString::from("main.cc"),
        ]));
    }

    #[test]
    fn identifies_explicit_lld_selection() {
        assert!(clang_invocation_uses_lld(&[OsString::from("-fuse-ld=lld")]));
        assert!(clang_invocation_uses_lld(&[OsString::from(
            "--ld-path=/usr/lib/llvm/bin/ld.lld",
        )]));
        assert!(!clang_invocation_uses_lld(&[OsString::from(
            "-fuse-ld=gold"
        )]));
        assert!(!clang_invocation_uses_lld(&[OsString::from("main.o")]));
    }
}
