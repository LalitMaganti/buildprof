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

-- What looks wrong with a build, ranked by how much of it went there.
--
-- Each insight records where the time went and, when the command line
-- explains it, the setting responsible. Insights are data: how to word them is
-- up to whoever shows them.

INCLUDE PERFETTO MODULE buildprof.processes;
INCLUDE PERFETTO MODULE buildprof.concurrency;
INCLUDE PERFETTO MODULE buildprof.options;

-- The recording's span, and the most processes it ran at once.
CREATE PERFETTO TABLE _buildprof_recording AS
SELECT
  (SELECT max(ts + dur) - min(ts) FROM buildprof_process) AS wall,
  (SELECT ifnull(max(value), 0) FROM buildprof_concurrency) AS peak;

-- Stretches where exactly one process was doing work.
CREATE PERFETTO TABLE _buildprof_single AS
FROM buildprof_concurrency
|> WHERE value = 1
|> SELECT ts, dur;

-- How long each leaf process was the only thing running.
CREATE PERFETTO TABLE _buildprof_alone AS
INTERVAL INTERSECTION OF (buildprof_leaf AS l, _buildprof_single)
|> AGGREGATE SUM(dur) AS alone GROUP BY l.pid AS pid;

-- Tools that run others and can be told to run several at once.
CREATE PERFETTO TABLE _buildprof_serial_driver AS
SELECT p.pid, p.final_cmd, buildprof_executable(p.final_cmd) AS exe, c.children
FROM buildprof_process AS p
JOIN buildprof_child_parallelism AS c USING (pid)
WHERE buildprof_executable(p.final_cmd) IN ('make', 'gmake', 'ninja', 'cargo', 'ctest')
  AND c.children >= 2
  AND c.peak = 1;

-- How long the build was waiting on a serial driver's one running child.
CREATE PERFETTO TABLE _buildprof_serial AS
INTERVAL INTERSECTION OF (
  (
    SELECT c.ppid AS driver, c.ts, c.dur
    FROM buildprof_process AS c
    JOIN _buildprof_serial_driver AS d ON d.pid = c.ppid
  ) AS k,
  _buildprof_single
)
|> AGGREGATE SUM(dur) AS alone GROUP BY k.driver AS pid;

-- Command lines run many times over.
CREATE PERFETTO TABLE _buildprof_repeated_cmd AS
FROM buildprof_process
|> AGGREGATE COUNT(*) AS runs GROUP BY final_cmd AS cmd
|> WHERE runs >= 50;

-- Which parents ran each repeated command line, and how often.
CREATE PERFETTO TABLE _buildprof_repeated_runs AS
FROM (
  SELECT p.final_cmd AS cmd, p.ppid
  FROM buildprof_process AS p
  JOIN _buildprof_repeated_cmd AS r ON r.cmd = p.final_cmd
)
|> AGGREGATE COUNT(*) AS runs GROUP BY cmd, ppid;

CREATE PERFETTO TABLE _buildprof_repeated_cover AS
FROM (
  SELECT final_cmd AS cmd, ts, dur
  FROM buildprof_process
  WHERE final_cmd IN (SELECT cmd FROM _buildprof_repeated_runs)
)
|> INTERVAL FLATTEN PER cmd AGGREGATE COUNT(*) AS running
|> AGGREGATE SUM(dur) AS covered GROUP BY cmd;

-- Shares of the recording an insight needs before it is shown.
CREATE PERFETTO TABLE _buildprof_threshold AS
SELECT 0.10 AS alone, 0.10 AS serial, 0.05 AS repeated;

-- Processes that ran alone long enough to be worth saying so. Ran-alone
-- needs a build that ran several things at once somewhere: in a build that
-- never did, everything ran alone.
CREATE PERFETTO TABLE _buildprof_alone_insight AS
SELECT a.pid, a.alone AS dur
FROM _buildprof_alone AS a, _buildprof_recording AS r, _buildprof_threshold AS t
WHERE r.peak >= 2 AND a.alone >= t.alone * r.wall;

-- The time already blamed on a ran-alone process, for each of its ancestors,
-- so a serial `cargo` is not blamed again for the compile that held it up.
CREATE PERFETTO TABLE _buildprof_blamed_below AS
FROM (
  SELECT p.pid AS id, iif(parent.pid IS NULL, NULL, p.ppid) AS parent_id,
    ifnull(a.dur, 0) AS blamed
  FROM buildprof_process AS p
  LEFT JOIN buildprof_process AS parent ON parent.pid = p.ppid
  LEFT JOIN _buildprof_alone_insight AS a ON a.pid = p.pid
)
|> TREE ACCUMULATE UP SUM(blamed) AS below
|> SELECT id AS pid, below;

-- The value a process's command line gives a setting, or NULL when it gives
-- none and the tool's default applies.
CREATE PERFETTO FUNCTION _buildprof_set_option(pid LONG, key STRING)
RETURNS STRING AS
SELECT value FROM buildprof_process_options($pid)
WHERE key = $key AND NOT is_default;

-- The compile that built a Cargo build script, found by its output
-- directory: cargo runs the script from the directory rustc wrote it to.
CREATE PERFETTO TABLE _buildprof_build_script_compile AS
SELECT
  rtrim(regexp_extract(final_cmd, '--out-dir ([^ ]+)'), '/') AS dir,
  coalesce(_buildprof_codegen(' ' || final_cmd || ' ', 'opt-level'), '0') AS opt_level
FROM buildprof_process
WHERE buildprof_is_build_script_compile(final_cmd)
  AND final_cmd GLOB '* --out-dir *';

-- A process and everything above it, nearest first.
CREATE PERFETTO FUNCTION _buildprof_ancestors(pid LONG)
RETURNS TABLE(pid LONG, cmd STRING, depth LONG) AS
WITH RECURSIVE up(pid, ppid, cmd, depth) AS (
  SELECT pid, ppid, final_cmd, 0 FROM buildprof_process WHERE pid = $pid
  UNION ALL
  SELECT p.pid, p.ppid, p.final_cmd, up.depth + 1
  FROM buildprof_process AS p
  JOIN up ON p.pid = up.ppid
)
SELECT pid, cmd, depth FROM up;

-- Everything that looks wrong with the build, most costly first.
CREATE PERFETTO TABLE buildprof_insight(
  -- Rank: 1 is where the most time went.
  id LONG,
  -- Which rule found it: 'ran_alone', 'serial_driver' or 'repeated_command'.
  rule STRING,
  -- What explains it, when something on the command line does, or NULL.
  -- 'ran_alone': 'codegen_units' (rustc was given one codegen unit) or
  -- 'unoptimized_build_script' (a Cargo build script compiled at opt-level 0).
  -- 'serial_driver': 'serial_by_default' (make or ctest given no job count,
  -- so one at a time) or 'one_job' (told to run one job at a time).
  cause STRING,
  -- The process the insight is about: the long process, the serial driver,
  -- or the parent that launched most of the repeated runs.
  pid LONG,
  -- The wall time the build spent on it.
  dur DURATION,
  -- That time as a share of the recording, from 0 to 1.
  share DOUBLE,
  -- The serial driver's children, or the repeated command's runs.
  count LONG,
  -- The repeated command line, or the process's own.
  cmd STRING,
  -- The setting responsible, as buildprof_process_options names it, if one is.
  option_key STRING,
  -- 'cargo_fuzz' when cargo-fuzz started the build that this is part of: it
  -- passes its own settings, so the way to change them differs.
  via STRING,
  -- The Cargo profile of the nearest cargo above the process, if any.
  profile STRING
) AS
WITH
  alone AS (
    SELECT
      'ran_alone' AS rule,
      CASE
        WHEN _buildprof_set_option(a.pid, 'codegen-units') = '1' THEN 'codegen_units'
        WHEN bs.opt_level = '0' THEN 'unoptimized_build_script'
      END AS cause,
      a.pid,
      a.dur,
      NULL AS count,
      p.final_cmd AS cmd
    FROM _buildprof_alone_insight AS a
    JOIN buildprof_process AS p USING (pid)
    LEFT JOIN _buildprof_build_script_compile AS bs
      ON buildprof_executable(p.final_cmd) GLOB 'build-script-*'
      AND bs.dir || '/' || buildprof_executable(p.final_cmd)
        = substr(p.final_cmd, 1, iif(instr(p.final_cmd, ' ') = 0,
            length(p.final_cmd), instr(p.final_cmd, ' ') - 1))
  ),
  serial AS (
    SELECT
      'serial_driver' AS rule,
      CASE
        WHEN _buildprof_set_option(s.pid, 'jobs') = '1' THEN 'one_job'
        WHEN d.exe IN ('make', 'gmake', 'ctest')
          AND _buildprof_set_option(s.pid, 'jobs') IS NULL
          AND (SELECT value FROM buildprof_process_options(s.pid) WHERE key = 'jobs') = '1'
          THEN 'serial_by_default'
      END AS cause,
      s.pid,
      s.alone - ifnull(b.below, 0) AS dur,
      d.children AS count,
      d.final_cmd AS cmd
    FROM _buildprof_serial AS s
    JOIN _buildprof_serial_driver AS d USING (pid)
    LEFT JOIN _buildprof_blamed_below AS b USING (pid)
  ),
  repeated AS (
    SELECT
      'repeated_command' AS rule,
      NULL AS cause,
      (
        SELECT ppid FROM _buildprof_repeated_runs AS r2
        WHERE r2.cmd = c.cmd ORDER BY runs DESC LIMIT 1
      ) AS pid,
      c.covered AS dur,
      (SELECT sum(runs) FROM _buildprof_repeated_runs AS r3 WHERE r3.cmd = c.cmd) AS count,
      c.cmd
    FROM _buildprof_repeated_cover AS c
  ),
  found AS (
    SELECT * FROM alone
    UNION ALL SELECT * FROM serial
    UNION ALL SELECT * FROM repeated
  )
SELECT
  row_number() OVER (ORDER BY f.dur DESC) AS id,
  f.rule,
  f.cause,
  f.pid,
  f.dur,
  1.0 * f.dur / r.wall AS share,
  f.count,
  f.cmd,
  CASE f.cause
    WHEN 'codegen_units' THEN 'codegen-units'
    WHEN 'one_job' THEN 'jobs'
    WHEN 'serial_by_default' THEN 'jobs'
  END AS option_key,
  iif(
    EXISTS (
      SELECT 1 FROM _buildprof_ancestors(f.pid)
      WHERE buildprof_executable(cmd) = 'cargo-fuzz'
    ),
    'cargo_fuzz',
    NULL
  ) AS via,
  (
    SELECT value FROM buildprof_process_options((
      SELECT pid FROM _buildprof_ancestors(f.pid)
      WHERE buildprof_executable(cmd) = 'cargo'
      ORDER BY depth LIMIT 1
    ))
    WHERE key = 'profile'
  ) AS profile
FROM found AS f, _buildprof_recording AS r, _buildprof_threshold AS t
WHERE f.dur > 0
  AND (
    f.rule = 'ran_alone'
    -- No need for the build to have run anything in parallel: a build that
    -- never did, because its driver ran one job at a time, is the worst case.
    OR (f.rule = 'serial_driver' AND f.dur >= t.serial * r.wall)
    OR (f.rule = 'repeated_command' AND f.dur >= t.repeated * r.wall)
  )
ORDER BY f.dur DESC;
