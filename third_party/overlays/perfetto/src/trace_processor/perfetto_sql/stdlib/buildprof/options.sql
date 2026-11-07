--
-- Copyright 2026 The Buildprof Authors.
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     https://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.

-- The settings that decide how fast a known tool runs, read from its command
-- line.
--
-- A setting that was never passed is shown with its default, because a
-- missing flag is as often the cause as a wrong one: `make` without `-j` runs
-- one job at a time.

INCLUDE PERFETTO MODULE buildprof.processes;

-- The executable a command line runs, without its directory.
CREATE PERFETTO FUNCTION buildprof_executable(
  -- A command line, arguments joined by spaces.
  cmd STRING
)
RETURNS STRING AS
SELECT replace(
  substr($cmd, 1, iif(instr($cmd, ' ') = 0, length($cmd), instr($cmd, ' ') - 1)),
  rtrim(
    substr($cmd, 1, iif(instr($cmd, ' ') = 0, length($cmd), instr($cmd, ' ') - 1)),
    replace(
      substr($cmd, 1, iif(instr($cmd, ' ') = 0, length($cmd), instr($cmd, ' ') - 1)),
      '/', '')),
  '');

-- The last value a flag was given. `pattern` is matched against the command
-- line with a space on either side, and holds one group around the value,
-- followed by a space: a leading `.*` makes the last occurrence win, as it
-- does for the tools themselves.
CREATE PERFETTO FUNCTION _buildprof_last(padded STRING, pattern STRING)
RETURNS STRING AS
SELECT regexp_extract($padded, '^.* ' || $pattern || ' ');

CREATE PERFETTO FUNCTION _buildprof_has(padded STRING, pattern STRING)
RETURNS BOOL AS
SELECT $padded REGEXP ' ' || $pattern || ' ';

-- rustc's `-C key=value`, in each spelling it accepts.
CREATE PERFETTO FUNCTION _buildprof_codegen(padded STRING, key STRING)
RETURNS STRING AS
SELECT _buildprof_last($padded, '(?:-C ?|--codegen[ =])' || $key || '=([^ ]+)');

-- Whether a rustc command line compiles a Cargo build script.
CREATE PERFETTO FUNCTION buildprof_is_build_script_compile(
  -- A command line, arguments joined by spaces.
  cmd STRING
)
RETURNS BOOL AS
SELECT ' ' || $cmd || ' ' GLOB '* --crate-name build_script_*';

-- The settings that decide how fast a process's last program ran, for the
-- tools Buildprof knows: rustc, cargo, make, ninja, ctest, C and C++
-- compilers, pytest, and Rust test binaries. Other programs have no rows.
--
-- Values are data, not wording: a number or flag value as the command line
-- gave it, or one of these tokens for a default that depends on the machine
-- or the caller: 'cpus' (one per CPU), 'cpus_plus_2', 'unlimited', and
-- 'inherited' (a sub-make's job slots, which come from its parent).
CREATE PERFETTO FUNCTION buildprof_process_options(
  -- The process.
  pid LONG
)
RETURNS TABLE(
  -- Which tool's settings these are: 'rustc', 'cargo', 'make', 'ninja',
  -- 'ctest', 'cc', 'pytest' or 'libtest'.
  tool STRING,
  -- The setting, such as 'codegen-units' or 'jobs'.
  key STRING,
  -- Its value, or its default when the command line did not set it.
  value STRING,
  -- 1 when the value is the tool's default.
  is_default BOOL,
  -- The order to show settings in.
  ord LONG
) AS
WITH
  p AS (
    SELECT
      ' ' || proc.final_cmd || ' ' AS c,
      buildprof_executable(proc.final_cmd) AS exe,
      parent.final_cmd AS parent_cmd
    FROM buildprof_process AS proc
    LEFT JOIN buildprof_process AS parent ON parent.pid = proc.ppid
    WHERE proc.pid = $pid
  ),
  kind AS (
    SELECT c, exe, parent_cmd,
      CASE
        WHEN exe = 'rustc' THEN 'rustc'
        WHEN exe = 'cargo' THEN 'cargo'
        WHEN exe IN ('make', 'gmake') THEN 'make'
        WHEN exe = 'ninja' THEN 'ninja'
        WHEN exe = 'ctest' THEN 'ctest'
        WHEN exe IN ('cc', 'c++', 'gcc', 'g++', 'clang', 'clang++', 'cc1', 'cc1plus')
          OR exe GLOB 'gcc-[0-9]*' OR exe GLOB 'g++-[0-9]*'
          OR exe GLOB 'clang-[0-9]*' OR exe GLOB 'clang++-[0-9]*' THEN 'cc'
        WHEN exe = 'pytest' OR c GLOB '* -m pytest *' THEN 'pytest'
        WHEN c GLOB '* --test-threads*' THEN 'libtest'
      END AS tool
    FROM p
  ),
  rustc AS (
    SELECT
      tool,
      _buildprof_codegen(c, 'opt-level') AS opt,
      _buildprof_has(c, '-O') AS dash_o,
      _buildprof_codegen(c, 'codegen-units') AS cgu,
      _buildprof_codegen(c, 'debuginfo') AS debug,
      _buildprof_has(c, '-g') AS dash_g,
      _buildprof_has(c, '(?:-C ?|--codegen[ =])incremental=[^ ]+') AS incremental,
      _buildprof_last(c, '(?:-C ?|--codegen[ =])(lto(?:=[^ ]+)?)') AS lto,
      buildprof_is_build_script_compile(c) AS build_script
    FROM kind WHERE tool = 'rustc'
  ),
  jobs AS (
    SELECT
      tool,
      _buildprof_last(c, '(?:-j ?|--jobs[ =]|--parallel[ =])(\d+)') AS n,
      _buildprof_has(c, '(?:-j|--jobs|--parallel)') AS bare,
      _buildprof_last(c, '(?:-l ?|--load-average[ =])(\d+(?:\.\d+)?)') AS load,
      buildprof_executable(ifnull(parent_cmd, '')) IN ('make', 'gmake') AS sub_make
    FROM kind WHERE tool IN ('make', 'ninja', 'ctest', 'cargo')
  ),
  other AS (
    SELECT
      tool,
      _buildprof_last(c, '--profile[ =]([^ ]+)') AS profile,
      _buildprof_has(c, '(?:--release|-r)') AS release,
      c GLOB '* bench *' AS bench,
      _buildprof_last(c, '--timeout[ =](\d+)') AS timeout,
      _buildprof_last(c, '(-O(?:\d|s|z|g|fast)?)') AS cc_opt,
      _buildprof_last(c, '(-g(?:[1-3]|gdb|dwarf[^ ]*)?)') AS cc_debug,
      _buildprof_last(c, '(-flto[^ ]*)') AS cc_lto,
      _buildprof_last(c, '(?:-n ?|--numprocesses[ =])([^ -][^ ]*)') AS workers,
      _buildprof_last(c, '--test-threads[ =](\d+)') AS threads
    FROM kind WHERE tool IS NOT NULL
  )
SELECT tool, 'opt-level' AS key,
  coalesce(opt, iif(dash_o, '2', NULL), '0') AS value,
  opt IS NULL AND NOT dash_o AS is_default, 1 AS ord
FROM rustc
UNION ALL
SELECT tool, 'codegen-units', coalesce(cgu, iif(incremental, '256', '16')), cgu IS NULL, 2
FROM rustc
UNION ALL
SELECT tool, 'debuginfo', coalesce(debug, iif(dash_g, '2', NULL), '0'),
  debug IS NULL AND NOT dash_g, 3
FROM rustc
UNION ALL
SELECT tool, 'incremental', iif(incremental, 'true', 'false'), NOT incremental, 4
FROM rustc
UNION ALL
SELECT tool, 'lto', iif(lto = 'lto', 'true', substr(lto, 5)), 0, 5
FROM rustc WHERE lto IS NOT NULL
UNION ALL
SELECT tool, 'build-script', 'true', 0, 6
FROM rustc WHERE build_script
UNION ALL
SELECT tool, 'profile',
  coalesce(profile, iif(release, 'release', NULL), iif(bench, 'bench', 'dev')),
  profile IS NULL AND NOT release, 1
FROM other WHERE tool = 'cargo'
UNION ALL
SELECT tool, 'jobs',
  CASE
    WHEN n IS NOT NULL THEN n
    WHEN bare AND tool = 'make' THEN 'unlimited'
    WHEN bare AND tool = 'ctest' THEN 'cpus'
    WHEN tool = 'make' AND sub_make THEN 'inherited'
    WHEN tool IN ('make', 'ctest') THEN '1'
    WHEN tool = 'ninja' THEN 'cpus_plus_2'
    ELSE 'cpus'
  END,
  n IS NULL AND NOT bare,
  2
FROM jobs
UNION ALL
SELECT tool, 'load', load, 0, 3
FROM jobs WHERE load IS NOT NULL AND tool IN ('make', 'ninja')
UNION ALL
SELECT tool, 'timeout', coalesce(timeout, '1500'), timeout IS NULL, 3
FROM other WHERE tool = 'ctest'
UNION ALL
SELECT tool, 'opt-level', coalesce(cc_opt, '-O0'), cc_opt IS NULL, 1
FROM other WHERE tool = 'cc'
UNION ALL
SELECT tool, 'debuginfo', coalesce(cc_debug, '-g0'), cc_debug IS NULL, 2
FROM other WHERE tool = 'cc'
UNION ALL
SELECT tool, 'lto', cc_lto, 0, 3
FROM other WHERE tool = 'cc' AND cc_lto IS NOT NULL
UNION ALL
SELECT tool, 'workers', coalesce(workers, '1'), workers IS NULL, 1
FROM other WHERE tool = 'pytest'
UNION ALL
SELECT tool, 'test-threads', coalesce(threads, 'cpus'), threads IS NULL, 1
FROM other WHERE tool = 'libtest';
