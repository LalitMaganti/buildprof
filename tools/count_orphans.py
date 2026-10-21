#!/usr/bin/env python3
# Counts processes in a recording whose parent is not a recorded process.
import sys
from perfetto.trace_processor import TraceProcessor, TraceProcessorConfig

QUERY = """
with p as (
  select extract_arg(arg_set_id, 'debug.pid') as pid,
         min(ifnull(extract_arg(arg_set_id, 'debug.build_ppid'),
                    extract_arg(arg_set_id, 'debug.ppid'))) as ppid,
         min(ifnull(extract_arg(arg_set_id, 'debug.execed'), 1)) as execed
  from slice where category = 'buildprof.process'
  group by 1
)
select count(*) as processes,
       sum(execed = 0) as fork_only,
       sum(ppid != 0 and ppid not in (select pid from p)) as orphans,
       sum(ppid != 0 and ppid not in (select pid from p) and ppid = pid - 1) as minus_one
from p
"""

for path in sys.argv[1:]:
    tp = TraceProcessor(trace=path, config=TraceProcessorConfig(fetch_latest_trace_processor=True))
    row = next(iter(tp.query(QUERY)))
    print(f"{path}: processes={row.processes} fork_only={row.fork_only} orphans={row.orphans} parent_is_pid_minus_1={row.minus_one}")
    tp.close()
