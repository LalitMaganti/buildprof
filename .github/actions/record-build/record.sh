#!/usr/bin/env bash
# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

start=$SECONDS
status=0
"$BUILDPROF_BINARY" --no-open -o "$BUILDPROF_TRACE" -- \
  bash -eo pipefail -c "$BUILDPROF_COMMAND" || status=$?
{
  echo "status=$status"
  echo "elapsed=$((SECONDS - start))"
  if [[ -s "$BUILDPROF_TRACE" ]]; then
    echo "trace=$BUILDPROF_TRACE"
  fi
} >> "$GITHUB_OUTPUT"
exit "$status"
