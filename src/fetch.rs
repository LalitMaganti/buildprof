// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Downloading a trace named by URL, so `open` can serve it like a local
//! file. GitHub artifacts go through `gh`, which already holds the user's
//! credentials and unpacks the artifact; any other URL goes through `curl`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A downloaded trace, removed with its directory when dropped.
pub struct Download {
    dir: PathBuf,
    pub trace: PathBuf,
}

impl Drop for Download {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn is_url(arg: &str) -> bool {
    arg.starts_with("https://") || arg.starts_with("http://")
}

pub fn fetch(url: &str) -> Result<Download, String> {
    let dir = std::env::temp_dir().join(format!("buildprof-open-{}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    // Owning the directory from here on removes it on every error path too.
    let mut download = Download {
        dir,
        trace: PathBuf::new(),
    };
    match github_artifact(url) {
        Some(artifact) => {
            let endpoint = format!("repos/{}/actions/artifacts/{}", artifact.repo, artifact.id);
            let name = run(
                "gh",
                ["api", endpoint.as_str(), "--jq", ".name"].map(OsStr::new),
                true,
            )?;
            run(
                "gh",
                [
                    OsStr::new("run"),
                    OsStr::new("download"),
                    OsStr::new(artifact.run),
                    OsStr::new("-R"),
                    OsStr::new(artifact.repo),
                    OsStr::new("-n"),
                    OsStr::new(name.trim()),
                    OsStr::new("-D"),
                    download.dir.as_os_str(),
                ],
                false,
            )?;
        }
        None => {
            let file = download.dir.join(file_name(url));
            run(
                "curl",
                [
                    OsStr::new("--fail"),
                    OsStr::new("--silent"),
                    OsStr::new("--show-error"),
                    OsStr::new("--location"),
                    OsStr::new("--output"),
                    file.as_os_str(),
                    OsStr::new(url),
                ],
                false,
            )?;
        }
    }
    download.trace = find_trace(&download.dir)?;
    Ok(download)
}

/// The parts of `https://github.com/OWNER/REPO/actions/runs/RUN/artifacts/ID`,
/// the artifact URL that the Action prints.
#[derive(Debug, PartialEq, Eq)]
struct Artifact<'a> {
    repo: &'a str,
    run: &'a str,
    id: &'a str,
}

fn github_artifact(url: &str) -> Option<Artifact<'_>> {
    let path = url.strip_prefix("https://github.com/")?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, repo, "actions", "runs", run, "artifacts", id] = parts[..] else {
        return None;
    };
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if owner.is_empty() || repo.is_empty() || !numeric(run) || !numeric(id) {
        return None;
    }
    Some(Artifact {
        repo: &path[..owner.len() + 1 + repo.len()],
        run,
        id,
    })
}

/// The last path segment of `url`, so the download keeps its extension.
fn file_name(url: &str) -> &str {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .split_once('/')
        .and_then(|(_, path)| path.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("trace.buildprof")
}

/// Runs `program`, returning its standard output when `capture` is set.
fn run<'a>(
    program: &str,
    args: impl IntoIterator<Item = &'a OsStr>,
    capture: bool,
) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound if program == "gh" => {
                "opening a GitHub artifact needs the GitHub CLI; install it from \
                 https://cli.github.com and run `gh auth login`"
                    .to_owned()
            }
            std::io::ErrorKind::NotFound => format!("opening a URL needs `{program}` on PATH"),
            _ => format!("could not run {program}: {error}"),
        })?;
    if !output.status.success() {
        // The first line says what went wrong; the rest is usually usage text.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().find(|line| !line.trim().is_empty());
        return Err(format!(
            "{program} failed: {}",
            reason.unwrap_or("no output").trim()
        ));
    }
    Ok(if capture {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        String::new()
    })
}

/// The one trace in `dir`, preferring Buildprof and Perfetto files when the
/// download holds others too.
fn find_trace(dir: &Path) -> Result<PathBuf, String> {
    let mut files = Vec::new();
    collect_files(dir, &mut files);
    let traces: Vec<&PathBuf> = files
        .iter()
        .filter(|file| {
            matches!(
                file.extension().and_then(OsStr::to_str),
                Some("buildprof" | "pftrace" | "perfetto-trace")
            )
        })
        .collect();
    match (traces.as_slice(), files.as_slice()) {
        ([trace], _) => Ok((*trace).clone()),
        ([], [file]) => Ok(file.clone()),
        ([], []) => Err("the download was empty".to_owned()),
        _ => Err(format!(
            "the download holds several files and it is not clear which to open: {}",
            files
                .iter()
                .filter_map(|file| file.strip_prefix(dir).ok())
                .map(|file| file.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Artifact, file_name, find_trace, github_artifact, is_url};
    use std::fs;

    #[test]
    fn recognises_urls() {
        assert!(is_url("https://example.com/trace.buildprof"));
        assert!(is_url("http://localhost:8000/trace.buildprof"));
        assert!(!is_url("trace.buildprof"));
        assert!(!is_url("./https/trace.buildprof"));
    }

    #[test]
    fn parses_github_artifact_urls() {
        assert_eq!(
            github_artifact("https://github.com/owner/repo/actions/runs/123/artifacts/456"),
            Some(Artifact {
                repo: "owner/repo",
                run: "123",
                id: "456"
            })
        );
        assert_eq!(
            github_artifact("https://github.com/owner/repo/actions/runs/123/artifacts/456/?x=1")
                .map(|artifact| artifact.id),
            Some("456")
        );
    }

    #[test]
    fn other_github_urls_are_plain_downloads() {
        assert_eq!(
            github_artifact("https://github.com/owner/repo/actions/runs/123"),
            None
        );
        assert_eq!(
            github_artifact("https://github.com/owner/repo/releases/download/v1/t.buildprof"),
            None
        );
        assert_eq!(
            github_artifact("https://github.com/owner/repo/actions/runs/abc/artifacts/456"),
            None
        );
        assert_eq!(
            github_artifact("https://example.com/a/b/actions/runs/1/artifacts/2"),
            None
        );
    }

    #[test]
    fn downloads_keep_the_url_file_name() {
        assert_eq!(
            file_name("https://example.com/x/build.buildprof?raw=1"),
            "build.buildprof"
        );
        assert_eq!(file_name("https://example.com/"), "trace.buildprof");
        assert_eq!(file_name("https://example.com"), "trace.buildprof");
    }

    #[test]
    fn finds_the_trace_in_a_download() {
        let dir = std::env::temp_dir().join(format!("buildprof-fetch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/output.buildprof"), b"trace").unwrap();
        fs::write(dir.join("notes.txt"), b"notes").unwrap();
        assert_eq!(
            find_trace(&dir).unwrap(),
            dir.join("nested/output.buildprof")
        );

        fs::write(dir.join("second.buildprof"), b"trace").unwrap();
        assert!(find_trace(&dir).unwrap_err().contains("several files"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
