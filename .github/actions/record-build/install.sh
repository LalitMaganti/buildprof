#!/usr/bin/env bash
# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
  echo 'Buildprof recording requires a Linux runner.' >&2
  exit 1
fi
case "$(uname -m)" in
  x86_64 | aarch64)
    target="$(uname -m)-unknown-linux-musl"
    ;;
  *)
    echo 'Buildprof supports x86_64 and aarch64 Linux runners.' >&2
    exit 1
    ;;
esac
directory=$(mktemp -d "$RUNNER_TEMP/buildprof.XXXXXXXX")
# Testing this action against a recorder that has no release yet: the caller
# built it, so there is nothing to download or verify.
if [[ -n ${BUILDPROF_RECORDER:-} ]]; then
  if [[ ! -x $BUILDPROF_RECORDER ]]; then
    echo "recorder is not an executable file: $BUILDPROF_RECORDER" >&2
    exit 1
  fi
  {
    echo "binary=$BUILDPROF_RECORDER"
    echo "trace=$directory/output.buildprof"
  } >> "$GITHUB_OUTPUT"
  exit 0
fi

if [[ ! $BUILDPROF_VERSION =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo 'version must be a release tag such as v0.2.5.' >&2
  exit 1
fi

archive="buildprof-$target.tar.xz"
url="https://github.com/LalitMaganti/buildprof/releases/download/$BUILDPROF_VERSION"
curl --fail --silent --show-error --location "$url/$archive" -o "$directory/$archive"
curl --fail --silent --show-error --location "$url/$archive.sha256" -o "$directory/checksum"
(
  cd "$directory"
  sha256sum --check checksum
  tar -xJf "$archive"
)
{
  echo "binary=$directory/buildprof-$target/buildprof"
  echo "trace=$directory/output.buildprof"
} >> "$GITHUB_OUTPUT"
