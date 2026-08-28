use crate::model::{FileOpen, Process, Rename, Segment};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use wire::Encoder;

mod proto;
mod wire;

const OUTPUT_BUFFER_BYTES: usize = 64 * 1024;
const PROCESS_MERGE_KEY: &str = "buildprof.processes";
const FILE_MERGE_KEY: &str = "buildprof.files";
const PROCESS_CATEGORY: &str = "buildprof.process";
const FILE_CATEGORY: &str = "buildprof.file";
const RENAME_CATEGORY: &str = "buildprof.rename";
const COMPILER_CATEGORY: &str = "buildprof.compiler";
const TRACK_UUID_PID_SHIFT: u32 = 2;
const PROCESS_TRACK_DISCRIMINATOR: u64 = 1;
const FILE_TRACK_DISCRIMINATOR: u64 = 2;
const COMPILER_TRACK_NAMESPACE: u64 = 1 << 63;
const COMPILER_TRACK_PID_SHIFT: u32 = 31;
const COMPILER_TRACK_BACKEND_SHIFT: u32 = 29;
const COMPILER_TRACK_THREAD_MASK: u64 = (1 << COMPILER_TRACK_BACKEND_SHIFT) - 1;
const MINIMUM_SLICE_DURATION_NS: u64 = 1;

/// Incrementally writes trace packets to a buffered file.
///
/// No protobuf message tree or encoded packet is retained. Nested message
/// lengths are obtained with an allocation-free counting pass immediately
/// before the bytes are written.
pub struct Writer {
    output: BufWriter<File>,
    annotation_names: HashMap<String, u64>,
    annotation_values: HashMap<String, u64>,
}

impl Writer {
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = Self {
            output: BufWriter::with_capacity(OUTPUT_BUFFER_BYTES, file),
            annotation_names: HashMap::new(),
            annotation_values: HashMap::new(),
        };
        writer.write_preamble()?;
        Ok(writer)
    }

    pub fn process_started(&mut self, pid: i32) -> io::Result<()> {
        self.with_encoder(|trace| {
            trace.packet(&mut |packet| {
                packet.sequence()?;
                packet.track_descriptor(
                    process_track_uuid(pid),
                    Some(proto::ROOT_TRACK_UUID),
                    "Processes",
                    Some(PROCESS_MERGE_KEY),
                )
            })?;
            trace.packet(&mut |packet| {
                packet.sequence()?;
                packet.track_descriptor(
                    file_track_uuid(pid),
                    Some(proto::ROOT_TRACK_UUID),
                    "File opens",
                    Some(FILE_MERGE_KEY),
                )
            })
        })
    }

    pub fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()> {
        self.with_encoder(|trace| {
            write_event(
                trace,
                segment.start_ns,
                proto::TYPE_SLICE_BEGIN,
                process_track_uuid(process.pid),
                Some(PROCESS_CATEGORY),
                Some(&segment.name),
                None,
                &mut |args| {
                    args.command(&segment.command)?;
                    args.cwd(&segment.cwd)?;
                    args.pid(process.pid)?;
                    args.parent_pid(process.parent_pid)?;
                    args.build_parent_pid(process.build_parent_pid)?;
                    args.execed(process.execed)?;
                    if let Some(exit_code) = segment.exit_code {
                        args.exit_code(exit_code)?;
                    }
                    Ok(())
                },
            )?;

            let end_ns = segment
                .end_ns
                .max(segment.start_ns.saturating_add(MINIMUM_SLICE_DURATION_NS));
            write_event(
                trace,
                end_ns,
                proto::TYPE_SLICE_END,
                process_track_uuid(process.pid),
                None,
                None,
                None,
                &mut |_| Ok(()),
            )
        })
    }

    pub fn file_open(&mut self, pid: i32, open: &FileOpen) -> io::Result<()> {
        self.with_encoder(|trace| {
            write_event(
                trace,
                open.timestamp_ns,
                proto::TYPE_INSTANT,
                file_track_uuid(pid),
                Some(FILE_CATEGORY),
                Some("open"),
                None,
                &mut |args| {
                    args.path(&open.path)?;
                    args.owner_pid(pid)?;
                    args.flags(open.flags)?;
                    args.fd(open.fd)
                },
            )
        })
    }

    pub fn rename(&mut self, pid: i32, rename: &Rename) -> io::Result<()> {
        self.with_encoder(|trace| {
            write_event(
                trace,
                rename.timestamp_ns,
                proto::TYPE_INSTANT,
                file_track_uuid(pid),
                Some(RENAME_CATEGORY),
                Some("rename"),
                None,
                &mut |args| {
                    args.from(&rename.from)?;
                    args.to(&rename.to)?;
                    args.owner_pid(pid)
                },
            )
        })
    }

    pub fn compiler_track(&mut self, pid: i32, thread_id: u32, backend: &str) -> io::Result<()> {
        let name = format!("{backend} compiler [pid {pid}] · thread {thread_id}");
        self.with_encoder(|trace| {
            trace.packet(&mut |packet| {
                packet.sequence()?;
                packet.track_descriptor(
                    compiler_track_uuid(pid, thread_id, backend),
                    Some(proto::ROOT_TRACK_UUID),
                    &name,
                    None,
                )
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compiler_slice(
        &mut self,
        pid: i32,
        thread_id: u32,
        backend: &str,
        event_category: &str,
        name: &str,
        start_ns: u64,
        duration_ns: u64,
        detail: Option<&str>,
    ) -> io::Result<()> {
        let track_uuid = compiler_track_uuid(pid, thread_id, backend);
        let annotation = detail
            .map(|detail| self.intern_debug_annotation("detail", detail))
            .transpose()?;
        self.with_encoder(|trace| {
            write_event(
                trace,
                start_ns,
                proto::TYPE_SLICE_BEGIN,
                track_uuid,
                Some(COMPILER_CATEGORY),
                Some(name),
                annotation,
                &mut |args| {
                    args.owner_pid(pid)?;
                    args.backend(backend)?;
                    args.compiler_category(event_category)
                },
            )?;
            write_event(
                trace,
                start_ns.saturating_add(duration_ns),
                proto::TYPE_SLICE_END,
                track_uuid,
                None,
                None,
                None,
                &mut |_| Ok(()),
            )
        })
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.output.flush()
    }

    fn write_preamble(&mut self) -> io::Result<()> {
        self.with_encoder(|trace| {
            trace.packet(&mut |packet| packet.extension_descriptor())?;
            trace.packet(&mut |packet| {
                packet.sequence_start()?;
                packet.track_descriptor(proto::ROOT_TRACK_UUID, None, "Build", None)
            })
        })
    }

    fn intern_debug_annotation(&mut self, name: &str, value: &str) -> io::Result<(u64, u64)> {
        let (name_iid, new_name) = intern(&mut self.annotation_names, name);
        let (value_iid, new_value) = intern(&mut self.annotation_values, value);
        if new_name || new_value {
            self.with_encoder(|trace| {
                trace.packet(&mut |packet| {
                    packet.sequence()?;
                    packet.intern_debug_annotation(
                        new_name.then_some((name_iid, name)),
                        new_value.then_some((value_iid, value)),
                    )
                })
            })?;
        }
        Ok((name_iid, value_iid))
    }

    fn with_encoder(
        &mut self,
        encode: impl FnOnce(&mut proto::Trace<'_, '_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut encoder = Encoder::writer(&mut self.output);
        encode(&mut proto::Trace::new(&mut encoder))
    }
}

#[allow(clippy::too_many_arguments)]
fn write_event(
    trace: &mut proto::Trace<'_, '_>,
    timestamp_ns: u64,
    event_type: u32,
    track_uuid: u64,
    category: Option<&str>,
    name: Option<&str>,
    annotation: Option<(u64, u64)>,
    args: &mut dyn FnMut(&mut proto::BuildprofEvent<'_, '_>) -> io::Result<()>,
) -> io::Result<()> {
    trace.packet(&mut |packet| {
        packet.timestamp(timestamp_ns)?;
        packet.sequence()?;
        if annotation.is_some() {
            packet.sequence_needs_incremental_state()?;
        }
        packet.track_event(event_type, track_uuid, category, name, annotation, args)
    })
}

fn intern(table: &mut HashMap<String, u64>, value: &str) -> (u64, bool) {
    if let Some(iid) = table.get(value) {
        return (*iid, false);
    }
    let iid = table.len() as u64 + 1;
    table.insert(value.to_owned(), iid);
    (iid, true)
}

fn process_track_uuid(pid: i32) -> u64 {
    (u64::from(pid as u32) << TRACK_UUID_PID_SHIFT) | PROCESS_TRACK_DISCRIMINATOR
}

fn file_track_uuid(pid: i32) -> u64 {
    (u64::from(pid as u32) << TRACK_UUID_PID_SHIFT) | FILE_TRACK_DISCRIMINATOR
}

fn compiler_track_uuid(pid: i32, thread_id: u32, backend: &str) -> u64 {
    COMPILER_TRACK_NAMESPACE
        | (u64::from(pid as u32) << COMPILER_TRACK_PID_SHIFT)
        | (compiler_backend_discriminator(backend) << COMPILER_TRACK_BACKEND_SHIFT)
        | (u64::from(thread_id) & COMPILER_TRACK_THREAD_MASK)
}

fn compiler_backend_discriminator(backend: &str) -> u64 {
    match backend {
        "Rust" => 0,
        "Clang" => 1,
        "LLD" => 2,
        _ => 3,
    }
}
