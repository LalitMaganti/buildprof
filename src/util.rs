// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Small pure helpers shared by the commands.

use std::time::Duration;

/// The scheme and host of `url`, which is how the browser names the site in
/// its permission prompts.
pub fn site_origin(url: &str) -> &str {
    let host_start = url.find("://").map_or(0, |index| index + 3);
    match url[host_start..].find('/') {
        Some(path_start) => &url[..host_start + path_start],
        None => url,
    }
}

pub fn humanize_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        1 => "1 second".to_owned(),
        s if s < 60 || s % 60 != 0 => format!("{s} seconds"),
        60 => "1 minute".to_owned(),
        s => format!("{} minutes", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::{humanize_duration, site_origin};
    use std::time::Duration;

    #[test]
    fn humanize_reads_naturally() {
        assert_eq!(humanize_duration(Duration::from_secs(1)), "1 second");
        assert_eq!(humanize_duration(Duration::from_secs(45)), "45 seconds");
        assert_eq!(humanize_duration(Duration::from_secs(60)), "1 minute");
        assert_eq!(humanize_duration(Duration::from_secs(90)), "90 seconds");
        assert_eq!(humanize_duration(Duration::from_secs(600)), "10 minutes");
    }

    #[test]
    fn site_origin_drops_the_path() {
        assert_eq!(
            site_origin("https://buildprof.lalitm.com/v0.2.3"),
            "https://buildprof.lalitm.com"
        );
        assert_eq!(
            site_origin("http://localhost:10000"),
            "http://localhost:10000"
        );
    }
}
