#[derive(Clone, Copy, Debug)]
pub struct Process {
    pub pid: i32,
    pub parent_pid: i32,
    /// Nearest ancestor known to have exec'd when this process was observed.
    pub build_parent_pid: i32,
    /// Whether this pid ever ran a program of its own.
    pub execed: bool,
}

#[derive(Debug)]
pub struct Segment {
    pub start_ns: u64,
    pub end_ns: u64,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<u32>,
}

/// A path moved from a temporary or cache location to its final destination.
#[derive(Debug)]
pub struct Rename {
    pub timestamp_ns: u64,
    pub from: String,
    pub to: String,
}

#[derive(Debug)]
pub struct FileOpen {
    pub timestamp_ns: u64,
    pub path: String,
    pub flags: u64,
    pub fd: i32,
}
