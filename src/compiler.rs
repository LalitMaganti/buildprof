// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Best-effort compiler-internal trace capture.
//!
//! Clang is asked for `-ftime-trace` through `CCC_OVERRIDE_OPTIONS`, which its
//! driver reads however it was launched. Rust and LLD flags are added by
//! private wrappers. This is opt-in and may change compiler cache keys;
//! requesting the detail trace takes precedence over preserving cache hits.

use crate::perfetto::Writer;
use analyzeme::{EventPayload, ProfilingData, Timestamp};
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const PROFILE_DIR_ENV: &str = "BUILDPROF_COMPILER_PROFILE_DIR";
const REAL_RUST_WRAPPER_ENV: &str = "BUILDPROF_REAL_RUSTC_WRAPPER";
const CLANG_OVERRIDE_ENV: &str = "CCC_OVERRIDE_OPTIONS";
const RUST_WRAPPER_KIND: &str = "buildprof-rustc-wrapper";
const CLANG_WRAPPER_KIND: &str = "clang";
const CLANGXX_WRAPPER_KIND: &str = "clang++";
const RUST_BACKEND: &str = "Rust";
const CLANG_BACKEND: &str = "Clang";
const LLD_BACKEND: &str = "LLD";
const PROFILE_DIRECTORY_PREFIX: &str = "buildprof-compilers";
const CLANG_TRACE_DIRECTORY_NAME: &str = "clang";
// Clang reads at most this much before the first event's pid.
const CLANG_TRACE_PID_WINDOW: u64 = 4096;
// Allows for the recorder's and the compiler's clocks disagreeing slightly.
const CLANG_TRACE_START_SLACK_NS: u64 = 1_000_000_000;
const LLD_TRACE_PREFIX: &str = "lld";
const CLANG_TRACE_EXTENSION: &str = "json";
const RUST_PROFILE_EXTENSION: &str = "mm_profdata";
const RUST_NIGHTLY_CACHE_PREFIX: &str = "rustc-nightly";
// Compiler activities are the user-facing phase timeline. Rust's query
// provider stream is a separate expert-level dataset, not a finer sampling of
// this one, and can be added later without changing what this trace means.
const RUST_SELF_PROFILE_EVENTS: &str = "generic-activity";
const RUST_INFORMATION_FLAGS: &[&str] = &["--version", "-V", "-vV", "--print"];
const RUST_VERSION_FLAGS: &[&str] = &["--version", "-V", "-vV"];
const RUST_PRINT_FLAG_PREFIX: &[u8] = b"--print=";
const WRAPPER_DIRECTORY_NAME: &str = "bin";
const MINIMUM_COMPILER_EVENT_NS: u64 = 1;
const NANOS_PER_MICROSECOND: f64 = 1_000.0;
const WRAPPER_FAILURE_EXIT_CODE: u8 = 126;
const COMMAND_NOT_FOUND_EXIT_CODE: u8 = 127;

pub struct Capture {
    directory: Option<PathBuf>,
    clang_directory: Option<PathBuf>,
    /// Traces seen but not yet claimed, so each is read once, not on every exit.
    clang_owners: HashMap<PathBuf, (ClangTraceStamp, Option<i32>)>,
    child_environment: Vec<(CString, CString)>,
    origin: SystemTime,
    declared_tracks: HashSet<(i32, u32, &'static str)>,
}

impl Capture {
    pub fn new(enabled: bool) -> Self {
        let mut capture = Self {
            directory: None,
            clang_directory: None,
            clang_owners: HashMap::new(),
            child_environment: Vec::new(),
            origin: UNIX_EPOCH,
            declared_tracks: HashSet::new(),
        };
        if enabled && let Err(error) = capture.prepare() {
            error!("compiler tracing unavailable: {error}");
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

        self.import_clang_traces(pid, process_start_ns, writer);

        let lld = directory.join(format!("{LLD_TRACE_PREFIX}-{pid}.{CLANG_TRACE_EXTENSION}"));
        if lld.is_file() {
            if let Err(error) =
                self.import_time_trace(pid, process_start_ns, LLD_BACKEND, &lld, writer)
            {
                error!("could not import {}: {error}", lld.display());
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
                    error!("could not import {}: {error}", path.display());
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
        self.prepare_clang_traces()?;
        self.prepare_lld()?;
        Ok(())
    }

    /// Reports traces no exited process claimed. Clang names a trace after the
    /// object it compiles, so two concurrent compiles of `foo.o` in different
    /// directories write the same file and one trace is lost.
    pub fn report(&self) {
        let Some(directory) = &self.clang_directory else {
            return;
        };
        let unclaimed = fs::read_dir(directory).map_or(0, |entries| entries.count());
        if unclaimed > 0 {
            hint!(
                "{unclaimed} Clang time traces could not be matched to a compiler process; concurrent compiles of objects with the same file name overwrite each other's trace"
            );
        }
    }

    fn prepare_rust(&mut self) -> io::Result<()> {
        let wrapper = self.wrapper_path(RUST_WRAPPER_KIND);
        symlink(env::current_exe()?, &wrapper)?;
        if let Some(existing) = env::var_os("RUSTC_WRAPPER") {
            self.push_env(REAL_RUST_WRAPPER_ENV, &existing)?;
        }
        self.push_env("RUSTC_WRAPPER", wrapper.as_os_str())
    }

    /// Every Clang driver the build runs, by name or by path, writes its trace
    /// into one directory, named after its output. The trace records the pid
    /// that wrote it, which is how it is matched to a process on exit.
    fn prepare_clang_traces(&mut self) -> io::Result<()> {
        let directory = self
            .directory
            .as_ref()
            .expect("prepared directory")
            .join(CLANG_TRACE_DIRECTORY_NAME);
        // The override is split on whitespace and has no quoting.
        if directory
            .as_os_str()
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            error!(
                "Clang tracing unavailable: {} contains whitespace",
                directory.display()
            );
            return Ok(());
        }
        fs::create_dir(&directory)?;
        // Prepended so a `-ftime-trace` the build passes itself still wins, and
        // with no trailing slash so a missing directory cannot fail a compile.
        // `#` silences the driver's echo of our edit but not of the user's own.
        let mut overrides = OsString::new();
        if let Some(existing) = env::var_os(CLANG_OVERRIDE_ENV) {
            overrides.push(existing);
            overrides.push(" ");
        }
        overrides.push("# ^-ftime-trace=");
        overrides.push(&directory);
        self.push_env(CLANG_OVERRIDE_ENV, &overrides)?;
        self.clang_directory = Some(directory);
        Ok(())
    }

    /// LLD has no environment equivalent of `--time-trace`, and Clang does not
    /// forward `-ftime-trace` to it, so explicit LLD links still need a wrapper.
    /// It is only installed when the build could find Clang by name anyway.
    fn prepare_lld(&mut self) -> io::Result<()> {
        if find_on_path("clang").is_none() || find_on_path("clang++").is_none() {
            return Ok(());
        }

        let shim_directory = self
            .wrapper_path(CLANG_WRAPPER_KIND)
            .parent()
            .expect("wrapper has a parent")
            .to_owned();
        let executable = env::current_exe()?;
        symlink(&executable, shim_directory.join(CLANG_WRAPPER_KIND))?;
        symlink(&executable, shim_directory.join(CLANGXX_WRAPPER_KIND))?;

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

    fn import_clang_traces(&mut self, pid: i32, process_start_ns: u64, writer: &mut Writer) {
        let Some(directory) = &self.clang_directory else {
            return;
        };
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let mut unclaimed = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let stamp = (metadata.len(), metadata.modified().ok());
            let owner = match self.clang_owners.get(&path) {
                Some((known, owner)) if *known == stamp => *owner,
                _ => time_trace_pid(&path),
            };
            // A trace from before this process started belongs to an earlier
            // process that had the same pid.
            let current = || {
                time_trace_start_ns(&path, self.origin).is_none_or(|start| {
                    start.saturating_add(CLANG_TRACE_START_SLACK_NS) >= process_start_ns
                })
            };
            if owner != Some(pid) || !current() {
                unclaimed.insert(path, (stamp, owner));
                continue;
            }
            if let Err(error) =
                self.import_time_trace(pid, process_start_ns, CLANG_BACKEND, &path, writer)
            {
                error!("could not import {}: {error}", path.display());
            }
            let _ = fs::remove_file(path);
        }
        self.clang_owners = unclaimed;
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

type ClangTraceStamp = (u64, Option<SystemTime>);

/// The pid of the process that wrote a Clang time trace. Clang writes
/// `{"traceEvents":[{"pid":N,` first, so only the start is read.
fn time_trace_pid(path: &Path) -> Option<i32> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(CLANG_TRACE_PID_WINDOW)
        .read_to_end(&mut head)
        .ok()?;
    parse_time_trace_pid(&head)
}

fn parse_time_trace_pid(head: &[u8]) -> Option<i32> {
    const KEY: &[u8] = b"\"pid\":";
    let start = head.windows(KEY.len()).position(|window| window == KEY)? + KEY.len();
    let digits = head[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    // A number cut off by the window could read as another pid.
    if start + digits == head.len() {
        return None;
    }
    std::str::from_utf8(&head[start..start + digits])
        .ok()?
        .parse()
        .ok()
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

/// Where a recording remembers whether `rustc` accepts `-Z` flags. The build
/// decides which toolchain runs, through `cargo +nightly`, `RUSTUP_TOOLCHAIN`,
/// or a `rust-toolchain` file, so the compiler is asked from the wrapper
/// rather than at startup, and the answer is shared by every wrapper.
fn nightly_cache_path(profile_dir: &Path, rustc: &OsStr) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    rustc.hash(&mut hasher);
    env::var_os("RUSTUP_TOOLCHAIN").hash(&mut hasher);
    profile_dir.join(format!(
        "{RUST_NIGHTLY_CACHE_PREFIX}-{:016x}",
        hasher.finish()
    ))
}

fn remember_nightly(cache: &Path, nightly: bool) {
    let pending = cache.with_extension(std::process::id().to_string());
    if fs::write(&pending, if nightly { b"1" } else { b"0" }).is_ok() {
        let _ = fs::rename(pending, cache);
    }
}

fn rustc_is_nightly(cache: &Path, rustc: &OsStr) -> bool {
    if let Ok(cached) = fs::read(cache) {
        return cached == b"1";
    }
    let nightly = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| rust_version_is_nightly(&output.stdout));
    remember_nightly(cache, nightly);
    nightly
}

/// Run a version query on the compiler's behalf and remember its answer.
/// Cargo asks the compiler for its version before compiling anything, so
/// under Cargo no compile ever has to ask again.
fn relay_version_query(mut command: Command, cache: &Path) -> ExitCode {
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            error!("compiler wrapper failed: {error}");
            return ExitCode::from(WRAPPER_FAILURE_EXIT_CODE);
        }
    };
    let _ = io::stdout().write_all(&output.stdout);
    let _ = io::stderr().write_all(&output.stderr);
    if output.status.success() {
        remember_nightly(cache, rust_version_is_nightly(&output.stdout));
    }
    let code = output
        .status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(WRAPPER_FAILURE_EXIT_CODE);
    ExitCode::from(code)
}

fn rust_version_is_nightly(version: &[u8]) -> bool {
    let text = String::from_utf8_lossy(version);
    text.contains("nightly") || text.contains("-dev")
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
                command.arg(&rustc);
                command
            } else {
                Command::new(&rustc)
            };
            let arguments: Vec<_> = arguments.collect();
            command.args(&arguments);
            let cache = nightly_cache_path(Path::new(&profile_dir), &rustc);
            if arguments
                .iter()
                .any(|argument| is_rust_information_argument(argument))
            {
                let queries_version = arguments
                    .iter()
                    .any(|argument| RUST_VERSION_FLAGS.iter().any(|flag| argument == flag));
                if queries_version && !cache.exists() {
                    return Some(relay_version_query(command, &cache));
                }
            } else if rustc_is_nightly(&cache, &rustc) {
                command.arg(format!(
                    "-Zself-profile={}",
                    Path::new(&profile_dir).display()
                ));
                command.arg(format!("-Zself-profile-events={RUST_SELF_PROFILE_EVENTS}"));
            }
            command
        }
        CLANG_WRAPPER_KIND | CLANGXX_WRAPPER_KIND => {
            // Run whatever the build would have found without the wrapper, which
            // may be a toolchain the build put on `PATH` after recording began.
            let path = path_without_wrappers(&profile_dir).unwrap_or_default();
            let Some(clang) = find_in(&path, kind) else {
                error!("{kind}: command not found");
                return Some(ExitCode::from(COMMAND_NOT_FOUND_EXIT_CODE));
            };
            let mut command = Command::new(clang);
            let arguments: Vec<_> = arguments.collect();
            let trace_lld =
                clang_invocation_links(&arguments) && clang_invocation_uses_lld(&arguments);
            command.args(&arguments);
            if trace_lld {
                let output = Path::new(&profile_dir).join(format!(
                    "{LLD_TRACE_PREFIX}-{}.{}",
                    std::process::id(),
                    CLANG_TRACE_EXTENSION
                ));
                command.arg(format!("-Wl,--time-trace={}", output.display()));
            }
            command.env("PATH", path);
            command
        }
        _ => return None,
    };
    let error = command.exec();
    error!("compiler wrapper failed: {error}");
    Some(ExitCode::from(WRAPPER_FAILURE_EXIT_CODE))
}

/// `PATH` with the wrapper directory taken back out, for the real compiler.
/// The directory is there to intercept what the build launches, not what
/// the compiler launches, and a compiler launcher such as ccache resolves its
/// own name through `PATH`: leaving the directory in place lets it find this
/// wrapper again, which sends it straight back to the launcher, forever.
fn path_without_wrappers(profile_dir: &OsStr) -> Option<OsString> {
    let path = env::var_os("PATH")?;
    let wrappers = Path::new(profile_dir).join(WRAPPER_DIRECTORY_NAME);
    let kept = env::split_paths(&path).filter(|directory| *directory != wrappers);
    env::join_paths(kept).ok()
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    find_in(&env::var_os("PATH")?, program)
}

fn find_in(path: &OsStr, program: &str) -> Option<PathBuf> {
    env::split_paths(path)
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
    fn recognizes_nightly_rust_versions() {
        assert!(rust_version_is_nightly(
            b"rustc 1.100.0-nightly (bff8e12ff 2026-08-26)\n"
        ));
        assert!(rust_version_is_nightly(b"rustc 1.101.0-dev\n"));
        assert!(!rust_version_is_nightly(
            b"rustc 1.98.0 (2c0f4e7a1 2026-07-30)\n"
        ));
        assert!(!rust_version_is_nightly(
            b"rustc 1.99.0-beta.3 (abcdef123 2026-08-10)\n"
        ));
    }

    #[test]
    fn reads_the_pid_that_wrote_a_clang_trace() {
        assert_eq!(
            parse_time_trace_pid(br#"{"traceEvents":[{"pid":3870173,"tid":3870173,"ts":6"#),
            Some(3_870_173)
        );
        assert_eq!(parse_time_trace_pid(br#"{"traceEvents":[{"pid":38"#), None);
        assert_eq!(parse_time_trace_pid(br#"{"traceEvents":["#), None);
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
