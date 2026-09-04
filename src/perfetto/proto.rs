//! Handwritten Perfetto protobuf schema.
//!
//! The borrowed writer types encode only the public fields buildprof uses.
//! Field numbers come from Perfetto's `TracePacket`, `TrackDescriptor`, and
//! `TrackEvent` proto definitions.

use super::wire::Encoder;
use std::io;

pub(super) const PACKET_SEQUENCE_ID: u32 = 1;
pub(super) const ROOT_TRACK_UUID: u64 = 1;
pub(super) const TYPE_SLICE_BEGIN: u32 = 1;
pub(super) const TYPE_SLICE_END: u32 = 2;
pub(super) const TYPE_INSTANT: u32 = 3;

// Perfetto reserves TrackEvent field numbers 1000 and above for extensions.
const BUILDPROF_EXTENSION_FIELD: u32 = 9900;
const MERGE_BY_KEY: u32 = 3;
const SEQUENCE_INCREMENTAL_STATE_CLEARED: u32 = 1;
const SEQUENCE_NEEDS_INCREMENTAL_STATE: u32 = 2;

mod trace {
    pub(super) const PACKET: u32 = 1;
}

mod packet {
    pub(super) const TIMESTAMP: u32 = 8;
    pub(super) const SEQUENCE_ID: u32 = 10;
    pub(super) const TRACK_EVENT: u32 = 11;
    pub(super) const INTERNED_DATA: u32 = 12;
    pub(super) const SEQUENCE_FLAGS: u32 = 13;
    pub(super) const TRACK_DESCRIPTOR: u32 = 60;
    pub(super) const EXTENSION_DESCRIPTOR: u32 = 72;
    pub(super) const TRACE_ATTRIBUTES: u32 = 126;
    pub(super) const ZSTD_COMPRESSED_PACKETS: u32 = 133;
}

mod trace_attributes {
    pub(super) const ATTRIBUTE: u32 = 1;
}

mod trace_attribute {
    pub(super) const KEY: u32 = 1;
    pub(super) const LONG_VALUE: u32 = 2;
    pub(super) const STRING_VALUE: u32 = 3;
}

mod track_descriptor {
    pub(super) const UUID: u32 = 1;
    pub(super) const NAME: u32 = 2;
    pub(super) const PARENT_UUID: u32 = 5;
    pub(super) const SIBLING_MERGE_BEHAVIOR: u32 = 15;
    pub(super) const SIBLING_MERGE_KEY: u32 = 16;
}

mod track_event {
    pub(super) const DEBUG_ANNOTATIONS: u32 = 4;
    pub(super) const TYPE: u32 = 9;
    pub(super) const TRACK_UUID: u32 = 11;
    pub(super) const CATEGORIES: u32 = 22;
    pub(super) const NAME: u32 = 23;
}

mod interned_data {
    pub(super) const DEBUG_ANNOTATION_NAMES: u32 = 3;
    pub(super) const DEBUG_ANNOTATION_STRING_VALUES: u32 = 29;
}

mod interned_string {
    pub(super) const IID: u32 = 1;
    pub(super) const VALUE: u32 = 2;
}

mod debug_annotation {
    pub(super) const NAME_IID: u32 = 1;
    pub(super) const STRING_VALUE_IID: u32 = 17;
}

pub(super) mod buildprof_event {
    pub(crate) const COMMAND: u32 = 1;
    pub(crate) const CWD: u32 = 2;
    pub(crate) const PID: u32 = 3;
    pub(crate) const PARENT_PID: u32 = 4;
    pub(crate) const BUILD_PARENT_PID: u32 = 5;
    pub(crate) const EXECED: u32 = 6;
    pub(crate) const EXIT_CODE: u32 = 7;
    pub(crate) const PATH: u32 = 8;
    pub(crate) const FLAGS: u32 = 9;
    pub(crate) const FD: u32 = 10;
    pub(crate) const FROM: u32 = 11;
    pub(crate) const TO: u32 = 12;
    pub(crate) const OWNER_PID: u32 = 13;
    pub(crate) const BACKEND: u32 = 14;
    pub(crate) const COMPILER_CATEGORY: u32 = 15;
}

mod extension_descriptor {
    pub(super) const EXTENSION_SET: u32 = 1;
}

mod descriptor_set {
    pub(super) const FILE: u32 = 1;
}

mod file_descriptor {
    pub(super) const NAME: u32 = 1;
    pub(super) const PACKAGE: u32 = 2;
    pub(super) const DEPENDENCY: u32 = 3;
    pub(super) const MESSAGE_TYPE: u32 = 4;
    pub(super) const SYNTAX: u32 = 12;
}

mod message_descriptor {
    pub(super) const NAME: u32 = 1;
    pub(super) const FIELD: u32 = 2;
    pub(super) const EXTENSION: u32 = 6;
}

mod field_descriptor {
    pub(super) const NAME: u32 = 1;
    pub(super) const EXTENDEE: u32 = 2;
    pub(super) const NUMBER: u32 = 3;
    pub(super) const LABEL: u32 = 4;
    pub(super) const TYPE: u32 = 5;
    pub(super) const TYPE_NAME: u32 = 6;

    pub(super) const LABEL_OPTIONAL: u32 = 1;
    pub(super) const TYPE_INT32: u32 = 5;
    pub(super) const TYPE_UINT64: u32 = 4;
    pub(super) const TYPE_BOOL: u32 = 8;
    pub(super) const TYPE_STRING: u32 = 9;
    pub(super) const TYPE_MESSAGE: u32 = 11;
    pub(super) const TYPE_UINT32: u32 = 13;
}

/// A value stored in Perfetto's metadata table under `trace_attribute.<key>`.
#[derive(Clone, Copy, Debug)]
pub(super) enum AttributeValue<'a> {
    Long(i64),
    Str(&'a str),
}

pub(super) struct Trace<'encoder, 'output> {
    encoder: &'encoder mut Encoder<'output>,
}

impl<'encoder, 'output> Trace<'encoder, 'output> {
    pub(super) fn new(encoder: &'encoder mut Encoder<'output>) -> Self {
        Self { encoder }
    }

    pub(super) fn packet(
        &mut self,
        encode: &mut dyn FnMut(&mut Packet<'_, '_>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.encoder.message(trace::PACKET, &mut |encoder| {
            encode(&mut Packet { encoder })
        })
    }
}

pub(super) struct Packet<'encoder, 'output> {
    encoder: &'encoder mut Encoder<'output>,
}

impl Packet<'_, '_> {
    pub(super) fn timestamp(&mut self, timestamp_ns: u64) -> io::Result<()> {
        self.encoder.uint(packet::TIMESTAMP, timestamp_ns)
    }

    pub(super) fn sequence(&mut self) -> io::Result<()> {
        self.encoder
            .uint(packet::SEQUENCE_ID, u64::from(PACKET_SEQUENCE_ID))
    }

    pub(super) fn sequence_start(&mut self) -> io::Result<()> {
        self.sequence()?;
        self.encoder.uint(
            packet::SEQUENCE_FLAGS,
            u64::from(SEQUENCE_INCREMENTAL_STATE_CLEARED),
        )
    }

    pub(super) fn sequence_needs_incremental_state(&mut self) -> io::Result<()> {
        self.encoder.uint(
            packet::SEQUENCE_FLAGS,
            u64::from(SEQUENCE_NEEDS_INCREMENTAL_STATE),
        )
    }

    /// A batch of complete packets compressed with zstd. Trace Processor
    /// expands these while tokenizing, so the batch must not itself contain
    /// compressed packets.
    pub(super) fn zstd_compressed_packets(&mut self, packets: &[u8]) -> io::Result<()> {
        self.encoder.bytes(packet::ZSTD_COMPRESSED_PACKETS, packets)
    }

    pub(super) fn trace_attributes(
        &mut self,
        attributes: &[(&str, AttributeValue<'_>)],
    ) -> io::Result<()> {
        self.encoder
            .message(packet::TRACE_ATTRIBUTES, &mut |message| {
                for (key, value) in attributes {
                    message.message(trace_attributes::ATTRIBUTE, &mut |attribute| {
                        attribute.string(trace_attribute::KEY, key)?;
                        match value {
                            AttributeValue::Long(value) => {
                                attribute.int(trace_attribute::LONG_VALUE, *value)
                            }
                            AttributeValue::Str(value) => {
                                attribute.string(trace_attribute::STRING_VALUE, value)
                            }
                        }
                    })?;
                }
                Ok(())
            })
    }

    pub(super) fn intern_debug_annotation(
        &mut self,
        name: Option<(u64, &str)>,
        value: Option<(u64, &str)>,
    ) -> io::Result<()> {
        self.encoder.message(packet::INTERNED_DATA, &mut |data| {
            if let Some((iid, name)) = name {
                data.message(interned_data::DEBUG_ANNOTATION_NAMES, &mut |entry| {
                    entry.uint(interned_string::IID, iid)?;
                    entry.string(interned_string::VALUE, name)
                })?;
            }
            if let Some((iid, value)) = value {
                data.message(
                    interned_data::DEBUG_ANNOTATION_STRING_VALUES,
                    &mut |entry| {
                        entry.uint(interned_string::IID, iid)?;
                        entry.string(interned_string::VALUE, value)
                    },
                )?;
            }
            Ok(())
        })
    }

    pub(super) fn track_descriptor(
        &mut self,
        uuid: u64,
        parent_uuid: Option<u64>,
        name: &str,
        merge_key: Option<&str>,
    ) -> io::Result<()> {
        self.encoder
            .message(packet::TRACK_DESCRIPTOR, &mut |descriptor| {
                descriptor.uint(track_descriptor::UUID, uuid)?;
                descriptor.string(track_descriptor::NAME, name)?;
                if let Some(parent_uuid) = parent_uuid {
                    descriptor.uint(track_descriptor::PARENT_UUID, parent_uuid)?;
                }
                if let Some(merge_key) = merge_key {
                    descriptor.uint(
                        track_descriptor::SIBLING_MERGE_BEHAVIOR,
                        u64::from(MERGE_BY_KEY),
                    )?;
                    descriptor.string(track_descriptor::SIBLING_MERGE_KEY, merge_key)?;
                }
                Ok(())
            })
    }

    pub(super) fn track_event(
        &mut self,
        event_type: u32,
        track_uuid: u64,
        category: Option<&str>,
        name: Option<&str>,
        annotation: Option<(u64, u64)>,
        args: &mut dyn FnMut(&mut BuildprofEvent<'_, '_>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.encoder.message(packet::TRACK_EVENT, &mut |event| {
            event.uint(track_event::TYPE, u64::from(event_type))?;
            event.uint(track_event::TRACK_UUID, track_uuid)?;
            if let Some(category) = category {
                event.string(track_event::CATEGORIES, category)?;
            }
            if let Some(name) = name {
                event.string(track_event::NAME, name)?;
            }
            if let Some((name_iid, value_iid)) = annotation {
                event.message(track_event::DEBUG_ANNOTATIONS, &mut |annotation| {
                    annotation.uint(debug_annotation::NAME_IID, name_iid)?;
                    annotation.uint(debug_annotation::STRING_VALUE_IID, value_iid)
                })?;
            }
            if event_type != TYPE_SLICE_END {
                event.message(BUILDPROF_EXTENSION_FIELD, &mut |encoder| {
                    args(&mut BuildprofEvent { encoder })
                })?;
            }
            Ok(())
        })
    }

    pub(super) fn extension_descriptor(&mut self) -> io::Result<()> {
        self.encoder
            .message(packet::EXTENSION_DESCRIPTOR, &mut |descriptor| {
                descriptor.message(extension_descriptor::EXTENSION_SET, &mut |set| {
                    set.message(descriptor_set::FILE, &mut |file| {
                        file.string(file_descriptor::NAME, "buildprof_extension.proto")?;
                        file.string(file_descriptor::PACKAGE, "buildprof.protos")?;
                        file.string(
                            file_descriptor::DEPENDENCY,
                            "protos/perfetto/trace/track_event/track_event.proto",
                        )?;
                        file.message(
                            file_descriptor::MESSAGE_TYPE,
                            &mut encode_buildprof_event_descriptor,
                        )?;
                        file.message(
                            file_descriptor::MESSAGE_TYPE,
                            &mut encode_extension_wrapper_descriptor,
                        )?;
                        file.string(file_descriptor::SYNTAX, "proto2")
                    })
                })
            })
    }
}

pub(super) struct BuildprofEvent<'encoder, 'output> {
    encoder: &'encoder mut Encoder<'output>,
}

impl BuildprofEvent<'_, '_> {
    pub(super) fn command(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::COMMAND, value)
    }

    pub(super) fn cwd(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::CWD, value)
    }

    pub(super) fn pid(&mut self, value: i32) -> io::Result<()> {
        self.encoder.uint(buildprof_event::PID, value as u64)
    }

    pub(super) fn parent_pid(&mut self, value: i32) -> io::Result<()> {
        self.encoder.uint(buildprof_event::PARENT_PID, value as u64)
    }

    pub(super) fn execed(&mut self, value: bool) -> io::Result<()> {
        self.encoder.boolean(buildprof_event::EXECED, value)
    }

    pub(super) fn build_parent_pid(&mut self, value: i32) -> io::Result<()> {
        self.encoder
            .uint(buildprof_event::BUILD_PARENT_PID, value as u64)
    }

    pub(super) fn exit_code(&mut self, value: u32) -> io::Result<()> {
        self.encoder
            .uint(buildprof_event::EXIT_CODE, u64::from(value))
    }

    pub(super) fn path(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::PATH, value)
    }

    pub(super) fn flags(&mut self, value: u64) -> io::Result<()> {
        self.encoder.uint(buildprof_event::FLAGS, value)
    }

    pub(super) fn fd(&mut self, value: i32) -> io::Result<()> {
        self.encoder.uint(buildprof_event::FD, value as u64)
    }

    pub(super) fn from(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::FROM, value)
    }

    pub(super) fn to(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::TO, value)
    }

    pub(super) fn owner_pid(&mut self, value: i32) -> io::Result<()> {
        self.encoder.uint(buildprof_event::OWNER_PID, value as u64)
    }

    pub(super) fn backend(&mut self, value: &str) -> io::Result<()> {
        self.encoder.string(buildprof_event::BACKEND, value)
    }

    pub(super) fn compiler_category(&mut self, value: &str) -> io::Result<()> {
        self.encoder
            .string(buildprof_event::COMPILER_CATEGORY, value)
    }
}

fn encode_buildprof_event_descriptor(message: &mut Encoder<'_>) -> io::Result<()> {
    message.string(message_descriptor::NAME, "BuildprofEvent")?;
    for (name, number, field_type) in [
        (
            "cmd",
            buildprof_event::COMMAND,
            field_descriptor::TYPE_STRING,
        ),
        ("cwd", buildprof_event::CWD, field_descriptor::TYPE_STRING),
        ("pid", buildprof_event::PID, field_descriptor::TYPE_INT32),
        (
            "ppid",
            buildprof_event::PARENT_PID,
            field_descriptor::TYPE_INT32,
        ),
        (
            "build_ppid",
            buildprof_event::BUILD_PARENT_PID,
            field_descriptor::TYPE_INT32,
        ),
        (
            "execed",
            buildprof_event::EXECED,
            field_descriptor::TYPE_BOOL,
        ),
        (
            "exit_code",
            buildprof_event::EXIT_CODE,
            field_descriptor::TYPE_UINT32,
        ),
        ("path", buildprof_event::PATH, field_descriptor::TYPE_STRING),
        (
            "flags",
            buildprof_event::FLAGS,
            field_descriptor::TYPE_UINT64,
        ),
        ("fd", buildprof_event::FD, field_descriptor::TYPE_INT32),
        ("from", buildprof_event::FROM, field_descriptor::TYPE_STRING),
        ("to", buildprof_event::TO, field_descriptor::TYPE_STRING),
        (
            "owner_pid",
            buildprof_event::OWNER_PID,
            field_descriptor::TYPE_INT32,
        ),
        (
            "backend",
            buildprof_event::BACKEND,
            field_descriptor::TYPE_STRING,
        ),
        (
            "compiler_category",
            buildprof_event::COMPILER_CATEGORY,
            field_descriptor::TYPE_STRING,
        ),
    ] {
        message.message(message_descriptor::FIELD, &mut |field| {
            encode_field(field, name, number, field_type, None, None)
        })?;
    }
    Ok(())
}

fn encode_extension_wrapper_descriptor(message: &mut Encoder<'_>) -> io::Result<()> {
    message.string(message_descriptor::NAME, "BuildprofExtension")?;
    message.message(message_descriptor::EXTENSION, &mut |field| {
        encode_field(
            field,
            "debug",
            BUILDPROF_EXTENSION_FIELD,
            field_descriptor::TYPE_MESSAGE,
            Some(".buildprof.protos.BuildprofEvent"),
            Some(".perfetto.protos.TrackEvent"),
        )
    })
}

fn encode_field(
    field: &mut Encoder<'_>,
    name: &str,
    number: u32,
    field_type: u32,
    type_name: Option<&str>,
    extendee: Option<&str>,
) -> io::Result<()> {
    field.string(field_descriptor::NAME, name)?;
    if let Some(extendee) = extendee {
        field.string(field_descriptor::EXTENDEE, extendee)?;
    }
    field.uint(field_descriptor::NUMBER, u64::from(number))?;
    field.uint(
        field_descriptor::LABEL,
        u64::from(field_descriptor::LABEL_OPTIONAL),
    )?;
    field.uint(field_descriptor::TYPE, u64::from(field_type))?;
    if let Some(type_name) = type_name {
        field.string(field_descriptor::TYPE_NAME, type_name)?;
    }
    Ok(())
}
