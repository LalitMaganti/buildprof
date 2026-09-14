// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Reading `eslogger`'s JSON Lines. Apple models the output on
//! `es_message_t` but promises no schema stability, so every field is read
//! defensively: a line that no longer has the expected shape is skipped
//! rather than failing the recording.

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

/// Event types the recorder subscribes to, by `eslogger` short name.
pub const PROCESS_EVENTS: &[&str] = &["fork", "exec", "exit"];
/// A file that did not exist yet reports `create` and no `open`.
pub const FILE_EVENTS: &[&str] = &["open", "create", "rename"];

/// One event from a process the recorder follows.
#[derive(Debug, PartialEq)]
pub struct Message {
    /// `mach_absolute_time()` when the event happened.
    pub mach_time: u64,
    pub pid: i32,
    pub ppid: i32,
    pub event: Event,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Fork {
        child: i32,
    },
    Exec {
        argv: Vec<String>,
        cwd: String,
        executable: String,
    },
    /// `stat` is a `wait(2)` status.
    Exit {
        stat: i32,
    },
    /// `fflag` holds kernel `FREAD`/`FWRITE` bits, not `open(2)` flags.
    Open {
        path: String,
        fflag: u64,
    },
    Rename {
        from: String,
        to: String,
    },
}

pub fn parse(line: &str) -> Option<Message> {
    let root: Value = serde_json::from_str(line).ok()?;
    let process = root.get("process")?;
    let pid = audit_pid(process)?;
    let ppid = int(process.get("ppid")).unwrap_or(0) as i32;
    let mach_time = root.get("mach_time")?.as_u64()?;
    let (kind, body) = root.get("event")?.as_object()?.iter().next()?;
    let event = match kind.as_str() {
        "fork" => Event::Fork {
            child: audit_pid(body.get("child")?)?,
        },
        "exec" => Event::Exec {
            argv: body
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .map(|arg| arg.as_str().unwrap_or_default().to_owned())
                        .collect()
                })
                .unwrap_or_default(),
            cwd: file_path(body.get("cwd")).unwrap_or_default(),
            executable: body
                .get("target")
                .and_then(|target| file_path(target.get("executable")))
                .unwrap_or_default(),
        },
        "exit" => Event::Exit {
            stat: int(body.get("stat"))? as i32,
        },
        "open" => Event::Open {
            path: file_path(body.get("file"))?,
            fflag: int(body.get("fflag")).unwrap_or(0) as u64,
        },
        // Recorded as the write open that created it, as on Linux, where
        // directories are not opens at all.
        "create" => {
            let destination = body.get("destination")?;
            let mode = int(destination.pointer("/existing_file/stat/st_mode"))
                .or_else(|| int(destination.pointer("/new_path/mode")))
                .unwrap_or(0);
            if mode as u32 & u32::from(libc::S_IFMT) == u32::from(libc::S_IFDIR) {
                return None;
            }
            Event::Open {
                path: destination_path(destination)?,
                fflag: FWRITE | DARWIN_CREAT,
            }
        }
        "rename" => Event::Rename {
            from: file_path(body.get("source"))?,
            to: destination_path(body.get("destination")?)?,
        },
        _ => return None,
    };
    Some(Message {
        mach_time,
        pid,
        ppid,
        event,
    })
}

/// `es_process_t` identifies a process by its audit token.
fn audit_pid(process: &Value) -> Option<i32> {
    Some(int(process.get("audit_token")?.get("pid"))? as i32)
}

fn file_path(file: Option<&Value>) -> Option<String> {
    Some(file?.get("path")?.as_str()?.to_owned())
}

/// A rename or create destination is a union: an existing file, or a
/// directory and the name the new file takes inside it.
fn destination_path(destination: &Value) -> Option<String> {
    if let Some(path) = file_path(destination.get("existing_file")) {
        return Some(path);
    }
    let new_path = destination.get("new_path")?;
    let dir = file_path(new_path.get("dir"))?;
    let filename = new_path.get("filename")?.as_str()?;
    Some(format!("{}/{filename}", dir.trim_end_matches('/')))
}

fn int(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| value.as_u64().map(|v| v as i64))
}

const FREAD: u64 = 0x1;
const FWRITE: u64 = 0x2;
const DARWIN_APPEND: u64 = 0x8;
const DARWIN_CREAT: u64 = 0x200;
const DARWIN_TRUNC: u64 = 0x400;
const DARWIN_EXCL: u64 = 0x800;

/// Kernel file flags (`sys/fcntl.h`) as the Linux `open(2)` flags the trace
/// format stores, so the UI decodes write intent the same way on both.
pub fn linux_open_flags(fflag: u64) -> u64 {
    const LINUX_WRONLY: u64 = 0o1;
    const LINUX_RDWR: u64 = 0o2;
    const LINUX_CREAT: u64 = 0o100;
    const LINUX_EXCL: u64 = 0o200;
    const LINUX_TRUNC: u64 = 0o1000;
    const LINUX_APPEND: u64 = 0o2000;

    let mut flags = match (fflag & FREAD != 0, fflag & FWRITE != 0) {
        (true, true) => LINUX_RDWR,
        (false, true) => LINUX_WRONLY,
        _ => 0,
    };
    for (darwin, linux) in [
        (DARWIN_APPEND, LINUX_APPEND),
        (DARWIN_CREAT, LINUX_CREAT),
        (DARWIN_TRUNC, LINUX_TRUNC),
        (DARWIN_EXCL, LINUX_EXCL),
    ] {
        if fflag & darwin != 0 {
            flags |= linux;
        }
    }
    flags
}

/// The few fields the privileged helper needs to decide whether a line
/// belongs to the recording. Typed, so the unrelated system-wide events it
/// discards are not built into a full JSON tree.
#[derive(Deserialize)]
pub struct Envelope {
    pub process: EnvelopeProcess,
    pub global_seq_num: Option<u64>,
    pub event: EnvelopeEvent,
}

#[derive(Deserialize)]
pub struct EnvelopeProcess {
    pub audit_token: AuditToken,
    #[serde(default)]
    pub ppid: i32,
}

#[derive(Deserialize)]
pub struct AuditToken {
    pub pid: i32,
}

#[derive(Deserialize)]
pub struct EnvelopeEvent {
    pub fork: Option<EnvelopeFork>,
    pub exit: Option<IgnoredAny>,
}

#[derive(Deserialize)]
pub struct EnvelopeFork {
    pub child: EnvelopeProcess,
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub fn line(pid: i32, ppid: i32, mach_time: u64, seq: u64, event: &str) -> String {
        format!(
            r#"{{"schema_version":1,"version":7,"mach_time":{mach_time},"time":"2026-09-14T10:00:00.000000000Z","global_seq_num":{seq},"seq_num":{seq},"event_type":0,"process":{{"audit_token":{{"asid":100001,"auid":501,"egid":20,"euid":501,"pid":{pid},"pidversion":1,"rgid":20,"ruid":501}},"ppid":{ppid},"original_ppid":{ppid},"group_id":{pid},"session_id":1,"is_platform_binary":true,"is_es_client":false,"executable":{{"path":"/bin/sh","path_truncated":false}}}},"event":{event}}}"#
        )
    }

    pub fn fork(child: i32) -> String {
        format!(
            r#"{{"fork":{{"child":{{"audit_token":{{"pid":{child},"pidversion":2}},"ppid":0,"original_ppid":0,"executable":{{"path":"/bin/sh","path_truncated":false}}}}}}}}"#
        )
    }

    pub fn exec(argv: &[&str], cwd: &str) -> String {
        format!(
            r#"{{"exec":{{"target":{{"audit_token":{{"pid":1,"pidversion":3}},"executable":{{"path":"/usr/bin/{}","path_truncated":false}}}},"args":{},"cwd":{{"path":"{cwd}","path_truncated":false}},"dyld_exec_path":"/usr/bin/cc","script":null}}}}"#,
            argv[0],
            serde_json::to_string(argv).unwrap()
        )
    }

    pub fn exit(stat: i32) -> String {
        format!(r#"{{"exit":{{"stat":{stat}}}}}"#)
    }

    #[test]
    fn parses_process_lifecycle() {
        let fork_line = line(10, 1, 5, 1, &fork(11));
        assert_eq!(
            parse(&fork_line),
            Some(Message {
                mach_time: 5,
                pid: 10,
                ppid: 1,
                event: Event::Fork { child: 11 },
            })
        );
        let exec_line = line(11, 10, 6, 2, &exec(&["cc", "-c", "a.c"], "/src"));
        assert_eq!(
            parse(&exec_line).unwrap().event,
            Event::Exec {
                argv: vec!["cc".into(), "-c".into(), "a.c".into()],
                cwd: "/src".into(),
                executable: "/usr/bin/cc".into(),
            }
        );
        assert_eq!(
            parse(&line(11, 10, 7, 3, &exit(256))).unwrap().event,
            Event::Exit { stat: 256 }
        );
    }

    #[test]
    fn parses_file_events() {
        let open = r#"{"open":{"fflag":514,"file":{"path":"/src/a.o","path_truncated":false}}}"#;
        assert_eq!(
            parse(&line(11, 10, 8, 4, open)).unwrap().event,
            Event::Open {
                path: "/src/a.o".into(),
                fflag: 514,
            }
        );
        let replace = r#"{"rename":{"source":{"path":"/src/.a.tmp"},"destination_type":0,"destination":{"existing_file":{"path":"/src/a"}}}}"#;
        assert_eq!(
            parse(&line(11, 10, 9, 5, replace)).unwrap().event,
            Event::Rename {
                from: "/src/.a.tmp".into(),
                to: "/src/a".into(),
            }
        );
        let created = r#"{"create":{"destination_type":1,"destination":{"new_path":{"dir":{"path":"/src"},"filename":"out.tmp","mode":420}}}}"#;
        assert_eq!(
            parse(&line(11, 10, 9, 7, created)).unwrap().event,
            Event::Open {
                path: "/src/out.tmp".into(),
                fflag: 0x202,
            }
        );
        let directory = r#"{"create":{"destination_type":0,"destination":{"existing_file":{"path":"/src/target","stat":{"st_mode":16877}}}}}"#;
        assert_eq!(parse(&line(11, 10, 9, 8, directory)), None);
        let create = r#"{"rename":{"source":{"path":"/src/.a.tmp"},"destination_type":1,"destination":{"new_path":{"dir":{"path":"/src/"},"filename":"a"}}}}"#;
        assert_eq!(
            parse(&line(11, 10, 9, 6, create)).unwrap().event,
            Event::Rename {
                from: "/src/.a.tmp".into(),
                to: "/src/a".into(),
            }
        );
    }

    #[test]
    fn skips_lines_of_an_unexpected_shape() {
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"buildprof":"ready"}"#), None);
        assert_eq!(parse(&line(1, 0, 1, 1, r#"{"mmap":{}}"#)), None);
    }

    #[test]
    fn translates_kernel_file_flags() {
        // O_RDONLY
        assert_eq!(linux_open_flags(0x1), 0);
        // O_WRONLY | O_CREAT | O_TRUNC, as FWRITE | O_CREAT | O_TRUNC
        assert_eq!(linux_open_flags(0x2 | 0x200 | 0x400), 0o1 | 0o100 | 0o1000);
        // O_RDWR | O_APPEND
        assert_eq!(linux_open_flags(0x3 | 0x8), 0o2 | 0o2000);
    }
}
