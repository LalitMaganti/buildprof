// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Programs whose work a recording is likely to miss. They are noticed by
//! name at exec and reported once, after the build, so the hint is not lost
//! in the build's own output.

use crate::report;
use std::collections::BTreeMap;
use std::path::Path;

/// Stable short links, redirected by the site to the README's troubleshooting
/// sections, so binaries already installed survive README edits.
const DIAGNOSE_URL: &str = "https://buildprof.lalitm.com/diagnose/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum BlindSpot {
    /// A client that asks a daemon to run the container.
    Container,
    /// A build system or cache that hands work to a long-lived server.
    Daemon,
    /// Rootless Podman setting up its user namespace, which cannot work
    /// under tracing because setuid programs gain no privileges.
    PodmanUserNamespace,
}

#[derive(Default)]
pub struct BlindSpots {
    /// The first program seen for each kind, to name it in the hint.
    seen: BTreeMap<BlindSpot, String>,
}

impl BlindSpots {
    pub fn observe(&mut self, argv: &[String]) {
        if let Some((spot, program)) = classify(argv) {
            self.seen.entry(spot).or_insert(program);
        }
    }

    pub fn report(&self) {
        for (spot, program) in &self.seen {
            let program = report::emph(program);
            match spot {
                BlindSpot::Container => hint!(
                    "{program} ran during this build; work inside its containers is not recorded"
                ),
                BlindSpot::Daemon => hint!(
                    "{program} ran during this build; work it hands to a daemon may not be recorded"
                ),
                BlindSpot::PodmanUserNamespace => hint!(
                    "rootless Podman could not set up its user namespace while traced; run {} before recording",
                    report::emph("podman info")
                ),
            }
            let topic = match spot {
                BlindSpot::Container => "containers",
                BlindSpot::Daemon => "daemons",
                BlindSpot::PodmanUserNamespace => "podman",
            };
            hint!(
                "see {}",
                report::emph(format_args!("{DIAGNOSE_URL}{topic}"))
            );
        }
    }
}

fn classify(argv: &[String]) -> Option<(BlindSpot, String)> {
    let base = |arg: &String| {
        Path::new(arg)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    let mut names = vec![base(argv.first()?)?];
    // A script such as gradlew or Bazel's launcher reaches exec as its
    // interpreter followed by the script path.
    if matches!(names[0].as_str(), "sh" | "bash" | "dash" | "zsh")
        && let Some(script) = argv.get(1).and_then(base)
    {
        names.push(script);
    }
    names.into_iter().find_map(|name| {
        let spot = match name.as_str() {
            "docker" | "docker-compose" | "nerdctl" => BlindSpot::Container,
            "bazel" | "bazel-real" | "bazelisk" | "buck2" | "sccache" => BlindSpot::Daemon,
            "gradle" | "gradlew" if !argv.iter().any(|arg| arg == "--no-daemon") => {
                BlindSpot::Daemon
            }
            "newuidmap" | "newgidmap" => BlindSpot::PodmanUserNamespace,
            _ => return None,
        };
        Some((spot, name))
    })
}

#[cfg(test)]
mod tests {
    use super::{BlindSpot, classify};

    fn spot(argv: &[&str]) -> Option<(BlindSpot, String)> {
        classify(&argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn matches_programs_by_file_name() {
        assert_eq!(
            spot(&["/usr/bin/docker", "run", "image"]),
            Some((BlindSpot::Container, "docker".into()))
        );
        assert_eq!(
            spot(&["bazel", "build", "//..."]),
            Some((BlindSpot::Daemon, "bazel".into()))
        );
        assert_eq!(
            spot(&["/usr/bin/newuidmap", "1234", "0", "1000", "1"]),
            Some((BlindSpot::PodmanUserNamespace, "newuidmap".into()))
        );
        assert_eq!(spot(&["cargo", "build"]), None);
        assert_eq!(spot(&["podman", "run", "image"]), None);
        assert_eq!(spot(&[]), None);
    }

    #[test]
    fn looks_through_script_interpreters() {
        assert_eq!(
            spot(&["/bin/sh", "./gradlew", "build"]),
            Some((BlindSpot::Daemon, "gradlew".into()))
        );
        assert_eq!(spot(&["sh", "-c", "docker run image"]), None);
        assert_eq!(spot(&["python3", "docker"]), None);
    }

    #[test]
    fn gradle_without_its_daemon_is_fine() {
        assert_eq!(
            spot(&["/bin/sh", "./gradlew", "--no-daemon", "build"]),
            None
        );
    }
}
