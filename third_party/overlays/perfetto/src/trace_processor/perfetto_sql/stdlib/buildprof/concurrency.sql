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

INCLUDE PERFETTO MODULE buildprof.processes;

-- When each parent had at least one child running, split wherever a child
-- starts or ends.
CREATE PERFETTO TABLE _buildprof_child_cover AS
FROM (
  SELECT c.ppid, c.ts, c.dur
  FROM buildprof_process AS c
  JOIN buildprof_process AS parent ON parent.pid = c.ppid
)
|> INTERVAL FLATTEN PER ppid AGGREGATE COUNT(*) AS running;

-- How many processes were doing work at each point in the build, split
-- wherever a process starts or ends.
--
-- A process counts while it runs and none of its children do: a `make`
-- waiting on a compile is not doing the work, the compile is. So every process
-- adds one, and every parent takes one away again while it has a child
-- running. Stretches where nothing ran have a count of 0.
CREATE PERFETTO TABLE _buildprof_occupancy AS
FROM (
  SELECT ts, dur, 1 AS weight FROM buildprof_process
  UNION ALL
  SELECT ts, dur, -1 AS weight FROM _buildprof_child_cover
)
|> INTERVAL FLATTEN AGGREGATE SUM(weight) AS value
|> INTERVAL FILL GAPS WITH (
     SELECT min(ts) AS ts, max(ts + dur) - min(ts) AS dur, 0 AS value
     FROM buildprof_process
   );

-- Neighbouring stretches with the same count, numbered so they can be merged.
CREATE PERFETTO TABLE _buildprof_occupancy_runs AS
FROM (
  SELECT ts, dur, value,
    sum(starts) OVER (ORDER BY ts) AS run
  FROM (
    SELECT ts, dur, value,
      iif(value = lag(value) OVER (ORDER BY ts), 0, 1) AS starts
    FROM _buildprof_occupancy
    WHERE dur > 0
  )
)
|> AGGREGATE MIN(ts) AS ts, SUM(dur) AS dur GROUP BY run, value;

-- The build's concurrency as phases: each row is a stretch during which the
-- same number of processes were doing work.
CREATE PERFETTO TABLE buildprof_concurrency(
  -- Unique id.
  id LONG,
  -- When the phase started.
  ts TIMESTAMP,
  -- How long it lasted.
  dur DURATION,
  -- How many processes were doing work.
  value LONG
) AS
FROM _buildprof_occupancy_runs
|> SELECT run AS id, ts, dur, value
|> ORDER BY ts;

CREATE PERFETTO TABLE _buildprof_child_overlap AS
FROM _buildprof_child_cover
|> AGGREGATE SUM(dur) AS covered, MAX(running) AS peak GROUP BY ppid AS pid;

CREATE PERFETTO TABLE _buildprof_child_totals AS
FROM (
  SELECT c.ppid, c.dur
  FROM buildprof_process AS c
  JOIN buildprof_process AS parent ON parent.pid = c.ppid
)
|> AGGREGATE COUNT(*) AS children, SUM(dur) AS busy GROUP BY ppid AS pid;

-- How the children of each process overlapped: the measured parallelism a
-- `make -j8` actually reached, next to the 8 it was given.
CREATE PERFETTO TABLE buildprof_child_parallelism(
  -- The parent process.
  pid LONG,
  -- How many children it started.
  children LONG,
  -- The children's run times added up.
  busy DURATION,
  -- How long at least one child was running.
  covered DURATION,
  -- The most children running at once.
  peak LONG
) AS
SELECT t.pid, t.children, t.busy, o.covered, o.peak
FROM _buildprof_child_totals AS t
JOIN _buildprof_child_overlap AS o USING (pid);
