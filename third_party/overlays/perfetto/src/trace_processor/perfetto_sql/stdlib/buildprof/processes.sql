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

-- Every program a recorded process ran: one row per exec, lasting until the
-- next exec or the exit.
CREATE PERFETTO TABLE buildprof_segment(
  -- The slice the recorder wrote for this program.
  slice_id LONG,
  -- When the program started.
  ts TIMESTAMP,
  -- How long it ran.
  dur DURATION,
  -- The program's executable name.
  tool STRING,
  -- The process.
  pid LONG,
  -- The process that started it.
  ppid LONG,
  -- The nearest ancestor that exec'd, skipping forks that never did.
  build_ppid LONG,
  -- 0 for a fork that never exec'd.
  execed LONG,
  -- The command line, arguments joined by spaces.
  cmd STRING,
  -- The working directory.
  cwd STRING,
  -- The exit status, on the process's last program.
  exit_code LONG
) AS
SELECT
  s.id AS slice_id,
  s.ts,
  s.dur,
  s.name AS tool,
  extract_arg(s.arg_set_id, 'debug.pid') AS pid,
  extract_arg(s.arg_set_id, 'debug.ppid') AS ppid,
  -- The recorder keeps forks that never exec'd, because discarding them
  -- would be irreversible. They are not build actions though, so their
  -- children are parented to the nearest pid that did exec.
  extract_arg(s.arg_set_id, 'debug.build_ppid') AS build_ppid,
  extract_arg(s.arg_set_id, 'debug.execed') AS execed,
  extract_arg(s.arg_set_id, 'debug.cmd') AS cmd,
  extract_arg(s.arg_set_id, 'debug.cwd') AS cwd,
  extract_arg(s.arg_set_id, 'debug.exit_code') AS exit_code
FROM slice AS s
WHERE s.category = 'buildprof.process';

-- Each process's span across all the programs it ran.
--
-- ifnull: a trace recorded before fork-only processes were kept carries
-- neither annotation, and in those the recorder had already reparented onto
-- the nearest exec'd ancestor. Without the fallback every ppid goes NULL,
-- nothing has a parent, and every process looks like a leaf.
CREATE PERFETTO TABLE _buildprof_span AS
FROM (
  SELECT pid, ifnull(build_ppid, ppid) AS ppid, ifnull(execed, 1) AS execed,
    ts, ts + dur AS end_ts, slice_id
  FROM buildprof_segment
)
|> AGGREGATE MIN(ppid) AS ppid, MIN(execed) AS execed, MIN(ts) AS ts,
     MAX(end_ts) AS end_ts, MIN(slice_id) AS slice_id
   GROUP BY pid;

-- Every build action: one row per process that exec'd, spanning all the
-- programs it ran.
--
-- A process is named by its first program and finishes as its last: `sh -c`
-- often execs the compiler in place, leaving one pid with two identities.
CREATE PERFETTO TABLE buildprof_process(
  -- The process, so `id` and `pid` can both be used.
  id LONG,
  -- The process.
  pid LONG,
  -- The nearest ancestor that exec'd.
  ppid LONG,
  -- Always 1; kept so readers of older traces see the same columns.
  execed LONG,
  -- When the first program started.
  ts TIMESTAMP,
  -- Until the last program exited.
  dur DURATION,
  -- The slice of the first program.
  slice_id LONG,
  -- The first program's executable name.
  tool STRING,
  -- The first program's command line.
  cmd STRING,
  -- The first program's working directory.
  cwd STRING,
  -- The last program's executable name.
  final_tool STRING,
  -- The last program's command line.
  final_cmd STRING,
  -- The last program's exit status.
  exit_code LONG
) AS
SELECT
  span.pid AS id,
  span.pid,
  span.ppid,
  span.execed,
  span.ts,
  span.end_ts - span.ts AS dur,
  span.slice_id,
  first.tool,
  first.cmd,
  first.cwd,
  last.tool AS final_tool,
  last.cmd AS final_cmd,
  last.exit_code
FROM _buildprof_span AS span
JOIN buildprof_segment AS first ON first.slice_id = span.slice_id
JOIN buildprof_segment AS last
  ON last.pid = span.pid AND last.ts + last.dur = span.end_ts
-- Build actions only. The forks that never exec'd are still in
-- buildprof_segment for anyone who wants the raw process tree.
WHERE span.execed = 1
-- A pid's last program is the one ending last; two ending together would be
-- one pid running two programs at once, which exec does not allow.
GROUP BY span.pid;

-- Build actions that started nothing else.
CREATE PERFETTO TABLE buildprof_leaf AS
FROM (
  SELECT p.*,
    iif(parent.pid IS NULL, NULL, p.ppid) AS parent_id
  FROM buildprof_process AS p
  LEFT JOIN buildprof_process AS parent ON parent.pid = p.ppid
)
|> TREE ACCUMULATE UP COUNT(*) AS subtree
|> WHERE subtree = 1
|> SELECT * EXCEPT (parent_id, subtree);
