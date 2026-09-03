// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import m from "mithril";
import type { App } from "../../public/app";
import type { PerfettoPlugin } from "../../public/plugin";
import type { Trace } from "../../public/trace";
import {
  LONG,
  NUM,
  NUM_NULL,
  STR,
  STR_NULL,
} from "../../trace_processor/query_result";
import { escapeSearchQuery } from "../../trace_processor/query_utils";
import { showModal } from "../../widgets/modal";
import { TrackNode } from "../../public/workspace";
import { SourceDataset } from "../../trace_processor/dataset";
import { createPerfettoTable } from "../../trace_processor/sql_utils";
import { SliceTrack } from "../../components/tracks/slice_track";
import { checkerboard } from "../../components/checkerboard";
import { DurationWidget } from "../../components/widgets/duration";
import { Timestamp } from "../../components/widgets/timestamp";
import { renderArguments } from "../../components/details/args";
import {
  type ArgsDict,
  getArgs,
} from "../../components/sql_utils/args";
import { asArgSetId } from "../../components/sql_utils/core_types";
import { makeColorScheme } from "../../components/colorizer";
import { HSLColor } from "../../base/color";
import type { ColorScheme } from "../../base/color_scheme";
import type { TrackEventDetailsPanel } from "../../public/details_panel";
import type {
  TrackMouseEvent,
  TrackRenderContext,
  TrackRenderer,
} from "../../public/track";
import type { AreaSelection } from "../../public/selection";
import { DetailsShell } from "../../widgets/details_shell";
import { GridLayout, GridLayoutColumn } from "../../widgets/grid_layout";
import { Section } from "../../widgets/section";
import { Tree, TreeNode } from "../../widgets/tree";
import { Button, ButtonGroup } from "../../widgets/button";
import { Switch } from "../../widgets/switch";
import { Anchor } from "../../widgets/anchor";
import { Time } from "../../base/time";
import { Tooltip } from "../../widgets/tooltip";
import { Stack, StackAuto } from "../../widgets/stack";
import { RelatedEventsOverlay } from "../../components/related_events/related_events_overlay";
import type { ArrowConnection } from "../../components/related_events/arrow_visualiser";
import { DataGrid } from "../../components/widgets/datagrid/datagrid";
import { SQLDataSource } from "../../components/widgets/datagrid/sql_data_source";
import type { ColumnSchema } from "../../components/widgets/datagrid/datagrid_schema";
import type { Pivot } from "../../components/widgets/datagrid/model";
import { showBuildprofHelp } from "../../core/embedder/buildprof_help";

const SHARE_SERVICE = "https://buildprofusercontent.lalitm.com/v1/traces";

function chooseAndOpenTrace(app: App): void {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".buildprof,.pftrace,.perfetto-trace,.trace";
  input.onchange = () => {
    const file = input.files?.[0];
    if (file !== undefined) void app.openTraceFromFile(file);
  };
  input.click();
}

function rejectNonBuildprof(trace: Trace): void {
  trace.initialPage.suggest("/", 1_000);
  const listener = trace.onTraceReady.addListener(() => {
    listener[Symbol.dispose]();
    void showModal({
      key: "buildprof-invalid-recording",
      title: "Not a Buildprof recording",
      icon: "error",
      content: m(
        "p",
        "This file does not contain Buildprof process data. Choose a .buildprof file produced by the buildprof command.",
      ),
      buttons: [{ text: "Back to Buildprof", primary: true }],
    });
  });
}

async function isBuildprof(trace: Trace): Promise<boolean> {
  const result = await trace.engine.query(`
    select count(*) as cnt
    from slice
    where category = 'buildprof.process'
      and extract_arg(arg_set_id, 'debug.pid') is not null
      and extract_arg(arg_set_id, 'debug.cmd') is not null
  `);
  return result.firstRow({ cnt: NUM }).cnt > 0;
}

function showError(error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  void showModal({
    title: "Could not share trace",
    icon: "error",
    content: m("p", message),
    buttons: [{ text: "Close", primary: true }],
  });
}

async function uploadTrace(trace: Trace): Promise<void> {
  void showModal({
    key: "buildprof-uploading",
    title: "Uploading trace",
    icon: "upload",
    content: m(
      "p",
      "Sending the trace through the Buildprof sharing service…",
    ),
  });

  try {
    const traceFile = await trace.getTraceFile();
    const response = await fetch(SHARE_SERVICE, {
      method: "POST",
      headers: { "Content-Type": "application/octet-stream" },
      body: traceFile,
    });
    if (!response.ok) {
      throw new Error(`The sharing service returned HTTP ${response.status}`);
    }

    const url = (await response.text()).trim();
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" || parsed.hostname !== "0x0.st") {
      throw new Error("0x0.st returned an invalid sharing URL");
    }

    const token = response.headers.get("X-Token");
    const deleteCommand =
      token === null
        ? undefined
        : `curl -X POST -F 'token=${token}' -F 'delete=' '${url}'`;

    void showModal({
      title: "Trace uploaded",
      icon: "check_circle",
      content: m(
        "div",
        m("p", "Anyone with this URL can download the trace:"),
        m("p", m("code", url)),
        token === null
          ? m(
              "p",
              "0x0.st did not return a management token, so Buildprof cannot provide a deletion command for this upload.",
            )
          : m(
              "div",
              m(
                "p",
                "Keep this deletion command private. It contains the management token required to remove the file:",
              ),
              m("p", m("code", deleteCommand)),
            ),
      ),
      buttons: [
        {
          text:
            deleteCommand === undefined
              ? "Copy sharing URL"
              : "Copy URL and deletion command",
          primary: true,
          action: () =>
            void navigator.clipboard.writeText(
              deleteCommand === undefined
                ? url
                : `Sharing URL: ${url}\nDeletion command: ${deleteCommand}`,
            ),
        },
        { text: "Close" },
      ],
    });
  } catch (error) {
    showError(error);
  }
}

function confirmShare(trace: Trace): void {
  void showModal({
    title: "Share trace?",
    icon: "share",
    content: m(
      "div",
      m("p", "If you continue:"),
      m(
        "ul",
        m(
          "li",
          "Your trace is relayed by the Buildprof sharing service to the third-party storage service 0x0.st.",
        ),
        m("li", "The Buildprof sharing service does not retain a copy."),
        m(
          "li",
          "Anyone with the generated, hard-to-guess URL can download the file.",
        ),
        m(
          "li",
          "0x0.st retains files according to its policy, normally from 30 days up to one year.",
        ),
        m(
          "li",
          "After upload, you will receive a private deletion command containing the 0x0.st management token.",
        ),
      ),
      m(
        "p",
        m(
          "strong",
          "Build traces can contain command names, source paths, environment details, and other sensitive information.",
        ),
      ),
    ),
    buttons: [
      { text: "Cancel" },
      {
        text: "Upload trace",
        primary: true,
        action: () => void uploadTrace(trace),
      },
    ],
  });
}

export default class BuildprofPlugin implements PerfettoPlugin {
  static readonly id = "dev.perfetto.Buildprof";

  static onActivate(app: App): void {
    // Buildprof owns the sidebar entry; CoreCommands retains the shortcut.
    app.sidebar.addMenuItem({
      section: "trace_files",
      sortOrder: 1,
      topbar: true,
      text: "Open trace file",
      icon: "folder_open",
      action: () => chooseAndOpenTrace(app),
    });
    app.sidebar.addMenuItem({
      section: "support",
      sortOrder: 10,
      topbar: true,
      topbarPosition: "right",
      text: "How to navigate",
      icon: "help_outline",
      cssClass: "pf-topbar-icon-only",
      tooltip: "How to navigate",
      action: showBuildprofHelp,
    });
    app.sidebar.addMenuItem({
      section: "support",
      sortOrder: 20,
      topbar: true,
      topbarPosition: "right",
      text: "Report a bug",
      icon: "bug_report",
      cssClass: "pf-topbar-icon-only",
      tooltip: "Report a bug",
      href: "https://github.com/lalitmaganti/buildprof/issues/new",
    });
    app.sidebar.addMenuItem({
      section: "support",
      sortOrder: 30,
      topbar: true,
      topbarPosition: "right",
      text: "GitHub",
      icon: "code",
      cssClass: "pf-topbar-icon-only",
      tooltip: "Buildprof on GitHub",
      href: "https://github.com/lalitmaganti/buildprof",
    });
  }

  async onTraceLoad(trace: Trace): Promise<void> {
    if (!(await isBuildprof(trace))) {
      rejectNonBuildprof(trace);
      return;
    }

    if (trace.traceInfo.downloadable) {
      trace.sidebar.addMenuItem({
        section: "current_trace",
        sortOrder: 20,
        topbar: true,
        text: "Share trace",
        icon: "share",
        action: () => confirmShare(trace),
      });
    }

    edgesReady = undefined;
    if ((await materialise(trace)) === 0) return;
    const dependencyArrows = new DependencyArrows(trace);
    const compilerTracks = new CompilerTracks(trace);
    await compilerTracks.register();
    const detailsPanels = new ProcessDetailsPanels(
      trace,
      dependencyArrows,
      compilerTracks,
    );
    await buildConcurrencyTrack(trace);
    await buildProcessTreeTrack(trace, detailsPanels);
    registerBuildprofSearch(trace);
    registerAggregationTab(trace);

    // Draw the selected action's immediate producers and consumers over the
    // timeline. One hop only: the transitive closure of a build graph is the
    // whole build, which is not a picture of anything.
    trace.tracks.registerOverlay(
      new RelatedEventsOverlay(trace, () => dependencyArrows.connections()),
    );
  }
}

// Occupancy counts leaf processes, excluding long-lived orchestrators.

// Labels combine the tool with an argv-derived action or artifact.

function tokenAfter(col: string, flag: string): string {
  const rest = `substr(${col}, instr(${col}, ' ${flag} ') + ${flag.length + 2})`;
  return `case when instr(${col}, ' ${flag} ') > 0 then
      substr(${rest}, 1,
        case when instr(${rest}, ' ') = 0 then length(${rest})
             else instr(${rest}, ' ') - 1 end)
    end`;
}

/**
 * The expression is unrolled because SQLite expressions have no loop.
 */
function tokenAt(col: string, n: number): string {
  let rest = col;
  for (let i = 1; i < n; i++) {
    rest = `case when instr(${rest}, ' ') = 0 then ''
                 else substr(${rest}, instr(${rest}, ' ') + 1) end`;
  }
  return `case when instr(${rest}, ' ') = 0 then ${rest}
               else substr(${rest}, 1, instr(${rest}, ' ') - 1) end`;
}

// Strip the basename by trimming every trailing non-slash character.
function dirnameSql(expr: string): string {
  return `case when instr(${expr}, '/') > 0
     then rtrim(${expr}, replace(${expr}, '/', ''))
     else '' end`;
}

function extensionSql(expr: string): string {
  const base = basenameSql(expr);
  return `case when instr(${base}, '.') > 0
     then replace(${base}, rtrim(${base}, replace(${base}, '.', '')), '')
     else '' end`;
}

function basenameSql(expr: string): string {
  return `case when instr(${expr}, '/') > 0
     then replace(${expr}, rtrim(${expr}, replace(${expr}, '/', '')), '')
     else ${expr} end`;
}

/**
 * Tools that only ever stand in front of the real work.
 *
 * Ninja and cargo both drive compiles through `/bin/sh -c`, and the shell
 * execs the compiler in place when the command is simple enough that it does
 * not need to fork. That leaves one pid holding two identities.
 */
const SHELL_TOOLS = ["sh", "bash", "dash", "zsh", "env"];

/**
 * SQL expression naming a process by tool and its primary action or artifact.
 */
function sliceLabelSql(name: string, cmd: string, cwd: string): string {
  const crate = tokenAfter(cmd, "--crate-name");
  const out = tokenAfter(cmd, "-o");
  const dir = tokenAfter(cmd, "-C");
  const pkg = tokenAfter(cmd, "-p");
  const shells = SHELL_TOOLS.map((tool) => `'${tool}'`).join(", ");
  const relative = (expr: string) => `case
      when ${expr} like ${cwd} || '/%' then substr(${expr}, length(${cwd}) + 2)
      else ${expr}
    end`;
  // Temporary output names are unstable across builds.
  const realOut = `case when ${out} not like '/tmp/%' then ${out} end`;
  const a2 = tokenAt(cmd, 2);
  const a3 = tokenAt(cmd, 3);
  const usable = (tok: string) =>
    `${tok} != '' and substr(${tok}, 1, 1) != '-' and ${tok} not like '/tmp/%'`;
  const firstArg = `case
      when ${usable(a2)} then ${a2}
      when ${usable(a3)} then ${a3}
    end`;
  // Keep raw argv whitespace and long paths out of timeline labels.
  const clean = (expr: string) =>
    `substr(replace(replace(replace(${expr}, char(10), ' '), char(13), ' '), char(9), ' '), 1, 80)`;
  const label = `case
    when ${name} in (${shells}) then ${name}
    when ${crate} is not null and ${crate} != '___'
      then ${name} || ' ' || ${crate}
    when ${name} = 'compile' and ${pkg} is not null
      then ${name} || ' ' || ${pkg}
    when ${realOut} is not null
      then ${name} || ' ' || (${relative(realOut)})
    when ${dir} is not null
      then ${name} || ' ' || ${basenameSql(dir)}
    when (${firstArg}) is not null
      then ${name} || ' ' || (${relative(`(${firstArg})`)})
    else ${name}
  end`;
  return clean(label);
}

/**
 * Open a DataGrid in its own tab over a subquery.
 *
 * Large result sets use the grid's sorting, filtering and virtualisation.
 */
interface GridPreset {
  readonly label: string;
  readonly pivot?: Pivot;
}

function openGridTab(
  trace: Trace,
  uri: string,
  title: string,
  description: string,
  subquery: string,
  schema: ColumnSchema,
  columns: ReadonlyArray<{ id: string; field: string; sort?: "ASC" | "DESC" }>,
  presets: ReadonlyArray<GridPreset> = [],
): void {
  const dataSource = new SQLDataSource({
    engine: trace.engine,
    tableOrSubquery: subquery,
  });

  // Manual pivot changes clear the active preset label.
  let pivot: Pivot | undefined = presets[0]?.pivot;
  let activeLabel: string | undefined = presets[0]?.label;

  trace.tabs.registerTab({
    uri,
    isEphemeral: true,
    content: {
      getTitle: () => title,
      render: () =>
        m(
          DetailsShell,
          { title, description, fillHeight: true },
          m(DataGrid, {
            schema,
            data: dataSource,
            fillHeight: true,
            initialColumns: [...columns],
            showExportButton: true,
            ...(presets.length === 0
              ? {}
              : {
                  pivot,
                  onPivotChanged: (next?: Pivot) => {
                    pivot = next;
                    activeLabel = undefined;
                  },
                  toolbarItemsLeft: [
                    m(
                      "div",
                      {
                        style: {
                          display: "flex",
                          alignItems: "center",
                          gap: "10px",
                          padding: "4px 0 4px 8px",
                        },
                      },
                      m(
                        "span",
                        {
                          style: {
                            opacity: "0.55",
                            fontSize: "11px",
                            letterSpacing: "0.06em",
                            textTransform: "uppercase",
                            whiteSpace: "nowrap",
                          },
                        },
                        "Group by",
                      ),
                      m(
                        ButtonGroup,
                        ...presets.map((preset) =>
                          m(Button, {
                            label: preset.label,
                            active: activeLabel === preset.label,
                            onclick: () => {
                              pivot = preset.pivot;
                              activeLabel = preset.label;
                            },
                          }),
                        ),
                      ),
                    ),
                  ],
                }),
          }),
        ),
    },
  });
  trace.tabs.showTab(uri);
}

// Area aggregation uses leaf processes to avoid double-counting descendants.

type AggDimension = "tool" | "dir" | "name";

const AGG_DIMENSIONS: ReadonlyArray<{
  readonly key: AggDimension;
  readonly label: string;
}> = [
  { key: "tool", label: "Tool" },
  { key: "dir", label: "Directory" },
  { key: "name", label: "Action" },
];

const DURATION_CELL = (v: unknown) =>
  typeof v === "bigint" ? formatDuration(v) : String(v);

/**
 * A cell that selects the corresponding slice and scrolls to it.
 *
 * Group rows have no single target and render an empty cell.
 */
function makeJumpColumn(trace: Trace): ColumnSchema[string] {
  return {
    title: "Jump",
    columnType: "identifier",
    cellRenderer: (v: unknown) => {
      if (typeof v !== "bigint" && typeof v !== "number") return "";
      const sliceId = Number(v);
      return m(Anchor, {
        icon: "arrow_forward",
        title: "Select this process on the timeline",
        onclick: () => {
          void trace.selection
            .selectTrackEvent(PROCESS_TRACK_URI, sliceId)
            .then(() => trace.selection.scrollToSelection("focus"));
        },
      });
    },
  };
}

/** Render a non-heading tree label. */
function plainLabel(label: m.Children): m.Children {
  return m("span", { style: { fontWeight: "normal" } }, label);
}

const FLAT_PIVOT: Pivot = { groupBy: [], aggregates: [] };

function makeFilePivot(...fields: ReadonlyArray<string>): Pivot {
  return {
    groupBy: fields.map((field) => ({ id: field, field })),
    aggregates: [
      { id: "opens", function: "SUM", field: "opens", sort: "DESC" },
      { id: "reads", function: "SUM", field: "reads" },
      { id: "writes", function: "SUM", field: "writes" },
    ],
    groupDisplay: "tree",
  };
}

function makeLeafSchema(trace: Trace): ColumnSchema {
  return {
    slice_id: makeJumpColumn(trace),
    tool: { title: "Tool", columnType: "text" },
    dir: { title: "Directory", columnType: "text" },
    name: { title: "Action", columnType: "text" },
    pid: { title: "PID", columnType: "quantitative" },
    dur: {
      title: "Duration",
      columnType: "quantitative",
      cellRenderer: DURATION_CELL,
    },
  };
}

/**
 * Group by one column, measured by count and duration.
 *
 * Groups remain expandable down to individual actions.
 */
function makeBuildPivot(...fields: ReadonlyArray<string>): Pivot {
  return {
    groupBy: fields.map((field) => ({ id: field, field })),
    aggregates: [
      { id: "count", function: "COUNT" },
      { id: "total", function: "SUM", field: "dur", sort: "DESC" },
      { id: "longest", function: "MAX", field: "dur" },
    ],
    groupDisplay: "tree",
  };
}

class BuildAggregation {
  private source?: SQLDataSource;
  private sourceKey = "";
  private pivot: Pivot = makeBuildPivot("tool");

  constructor(private readonly trace: Trace) {}

  render(sel: AreaSelection): m.Children {
    // Only process-tree selections contribute to this aggregation.
    if (!sel.trackUris.includes(PROCESS_TRACK_URI)) return undefined;

    // Reuse the data source while the selected time range is unchanged.
    const key = `${sel.start}-${sel.end}`;
    if (key !== this.sourceKey) {
      this.sourceKey = key;
      this.source = new SQLDataSource({
        engine: this.trace.engine,
        tableOrSubquery: `(
          select tool, dir, name, pid, dur, slice_id
          from ${T.leaf}
          where ts < ${sel.end} and ts + dur > ${sel.start}
        )`,
      });
    }
    if (this.source === undefined) return undefined;

    return m(
      Stack,
      { fillHeight: true, spacing: "none" },
      m(
        StackAuto,
        m(DataGrid, {
          schema: makeLeafSchema(this.trace),
          data: this.source,
          fillHeight: true,
          showExportButton: true,
          toolbarItemsLeft: m(
            ButtonGroup,
            ...AGG_DIMENSIONS.map((d) =>
              m(Button, {
                label: d.label,
                compact: true,
                active: this.pivot.groupBy[0]?.field === d.key,
                onclick: () => {
                  this.pivot = makeBuildPivot(d.key);
                },
              }),
            ),
          ),
          pivot: this.pivot,
          onPivotChanged: (pivot?: Pivot) => {
            if (pivot !== undefined) this.pivot = pivot;
          },
        }),
      ),
    );
  }
}

function registerAggregationTab(trace: Trace): void {
  const agg = new BuildAggregation(trace);
  trace.selection.registerAreaSelectionTab({
    id: "buildprof.aggregation",
    name: "Build aggregation",
    priority: 100,
    render: (sel: AreaSelection) => {
      const content = agg.render(sel);
      return content === undefined ? undefined : { isLoading: false, content };
    },
  });
}

// Downstream queries use typed, materialised columns.

const T = {
  seg: "__bt_segment",
  open: "__bt_open",
  life: "__bt_life",
  leaf: "__bt_leaf",
  layout: "__bt_layout",
  occupancy: "__bt_occupancy",
  concurrency: "__bt_concurrency",
  tree: "__bt_tree",
  rename: "__bt_rename",
  /**
   * Producer/consumer edges between processes.
   *
   * One row per (writer, reader) pair with the number of files they share.
   * Built lazily because it joins every file open in the trace.
   */
  edge: "__bt_edge",
} as const;

/**
 * What counts as something one action hands to another.
 *
 * Restricting edges to recognised artifacts avoids dependencies through
 * shared bookkeeping paths.
 */
const ARTIFACT_EXTENSIONS = [
  "o",
  "obj",
  "a",
  "lib",
  "so",
  "dylib",
  "dll",
  "rlib",
  "rmeta",
  "bc",
  "ll",
  "s",
  "asm",
  "pch",
  "gch",
  "h",
  "hh",
  "hpp",
  "hxx",
  "inc",
  "c",
  "cc",
  "cpp",
  "cxx",
  "rs",
  "go",
  "zig",
  "ts",
  "js",
  "proto",
];

const PROCESS_TRACK_URI = "buildprof.processes";

let edgesReady: Promise<void> | undefined;

function ensureEdges(trace: Trace): Promise<void> {
  const engine = trace.engine;
  if (edgesReady === undefined) {
    const artifactExts = ARTIFACT_EXTENSIONS.map((ext) => `'${ext}'`).join(
      ", ",
    );
    edgesReady = createPerfettoTable({
      engine,
      name: T.edge,
      as: `
        with
          -- Attribute writes through one rename hop to the destination.
          moved as (
            select distinct o.pid as pid,
                   coalesce(rn.to_path, o.path) as path,
                   coalesce(
                     lower(regexp_extract(rn.to_path, '\\.([^./]+)$')),
                     o.ext
                   ) as ext
            from ${T.open} o
            left join ${T.rename} rn on rn.from_path = o.path
            where o.is_write = 1
          ),
          w as (select pid, path from moved where ext in (${artifactExts})),
          r as (select distinct pid, path from ${T.open}
                 where is_write = 0 and ext in (${artifactExts}))
        select w.pid as src, r.pid as dst, count(*) as files,
               min(w.path) as sample
        from w join r on r.path = w.path and r.pid != w.pid
        group by w.pid, r.pid
      `,
    }).then(async () => {
      // Support point lookups from the panel and overlay.
      await Promise.all([
        engine.query(`create perfetto index ${T.edge}_src on ${T.edge}(src)`),
        engine.query(`create perfetto index ${T.edge}_dst on ${T.edge}(dst)`),
      ]);
    });
  }
  return edgesReady;
}

async function materialise(trace: Trace): Promise<number> {
  const e = trace.engine;

  await createPerfettoTable({
    engine: e,
    name: T.seg,
    as: `
      select
        s.id   as slice_id,
        s.ts   as ts,
        s.dur  as dur,
        s.name as tool,
        extract_arg(s.arg_set_id, 'debug.pid')       as pid,
        extract_arg(s.arg_set_id, 'debug.ppid')      as ppid,
        -- The recorder keeps forks that never exec'd, because discarding them
        -- would be irreversible. They are not build actions though, so the
        -- timeline hides them and parents their children to the nearest pid
        -- that did exec.
        extract_arg(s.arg_set_id, 'debug.build_ppid') as build_ppid,
        extract_arg(s.arg_set_id, 'debug.execed')     as execed,
        extract_arg(s.arg_set_id, 'debug.cmd')       as cmd,
        extract_arg(s.arg_set_id, 'debug.cwd')       as cwd,
        extract_arg(s.arg_set_id, 'debug.exit_code') as exit_code
      from slice s
      where s.category = 'buildprof.process'
    `,
  });

  await createPerfettoTable({
    engine: e,
    name: T.open,
    as: `
      select
        extract_arg(s.arg_set_id, 'debug.owner_pid') as pid,
        extract_arg(s.arg_set_id, 'debug.path')      as path,
        extract_arg(s.arg_set_id, 'debug.flags')     as flags,
        -- Materialise dependency predicates once for subsequent queries.
        lower(regexp_extract(
          extract_arg(s.arg_set_id, 'debug.path'), '\\.([^./]+)$')) as ext,
        iif((extract_arg(s.arg_set_id, 'debug.flags') & ${O_ACCMODE}) != 0
            or (extract_arg(s.arg_set_id, 'debug.flags') & ${O_CREAT}) != 0,
            1, 0) as is_write
      from slice s
      where s.category = 'buildprof.file'
    `,
  });

  // Process identity comes from the first exec; exit status from the last.
  await createPerfettoTable({
    engine: e,
    name: T.life,
    as: `
      with
        span as (
          -- ifnull: a trace recorded before fork-only processes were kept
          -- carries neither annotation, and in those the recorder had already
          -- reparented onto the nearest exec'd ancestor. Without the fallback
          -- every ppid goes NULL, nothing has a parent, and every process
          -- looks like a leaf -- which silently wrecks the occupancy track.
          select pid, min(ifnull(build_ppid, ppid)) as ppid,
                 min(ifnull(execed, 1)) as execed,
                 min(ts) as ts,
                 max(ts + dur) - min(ts) as dur,
                 -- The slice a grid row jumps to: the process's first exec.
                 min(slice_id) as slice_id
          from ${T.seg} group by pid
        ),
        launched as (
          select pid, tool, cmd, cwd,
                 row_number() over (partition by pid order by ts asc) as rn
          from ${T.seg}
        ),
        final as (
          select pid, exit_code,
                 row_number() over (partition by pid order by ts desc) as rn
          from ${T.seg}
        )
      select
        span.pid as id,
        span.pid as pid,
        span.ppid as ppid,
        span.execed as execed,
        span.ts as ts,
        span.dur as dur,
        span.slice_id as slice_id,
        launched.tool as tool,
        final.exit_code as exit_code,
        ${sliceLabelSql("launched.tool", "launched.cmd", "launched.cwd")} as name,
        ${basenameSql("launched.cwd")} as dir
      from span
      join launched on launched.pid = span.pid and launched.rn = 1
      join final on final.pid = span.pid and final.rn = 1
      -- Build actions only. The forks that never exec'd are still in
      -- __bt_segment for anyone who wants the raw process tree; this table is
      -- the build-shaped view everything else is built on.
      --
      -- ifnull: traces recorded before the recorder kept fork-only processes
      -- have no such annotation, and everything in them did exec.
      where span.execed = 1
    `,
  });

  await createPerfettoTable({
    engine: e,
    name: T.rename,
    as: `
      select
        extract_arg(s.arg_set_id, 'debug.from') as from_path,
        extract_arg(s.arg_set_id, 'debug.to')   as to_path
      from slice s
      where s.category = 'buildprof.rename'
    `,
  });

  // Index keys used by details, overlay, and dependency queries.
  for (const [table, column] of [
    [T.open, "pid"],
    [T.open, "path"],
    [T.life, "pid"],
    [T.life, "ppid"],
    [T.life, "slice_id"],
    [T.rename, "from_path"],
  ] as const) {
    await e.query(
      `create perfetto index ${table}_${column} on ${table}(${column})`,
    );
  }

  await createPerfettoTable({
    engine: e,
    name: T.leaf,
    as: `
      select l.*
      from ${T.life} l
      left join (select distinct ppid as parent from ${T.life}) k
        on k.parent = l.pid
      where k.parent is null
    `,
  });

  // Materialise one containment row per process.
  await e.query(`
    create virtual table __bt_layout_v using __intrinsic_containment_layout((
      select pid as id, ppid as parent_id, ts, dur from ${T.life}
    ))
  `);
  await createPerfettoTable({
    engine: e,
    name: T.layout,
    as: `select id, layout_depth, subtree_height from __bt_layout_v`,
  });

  const count = await e.query(`select count(*) as cnt from ${T.life}`);
  return count.firstRow({ cnt: NUM }).cnt;
}

const IDLE_LEAF_COUNT = 0;
const SINGLE_LEAF_COUNT = 1;

interface ConcurrencyColorBand {
  readonly maximumLeafCount: number;
  readonly color: ColorScheme;
}

function buildprofColor(hex: string): ColorScheme {
  const base = new HSLColor(hex).saturate(10);
  return makeColorScheme(base, base.darken(10));
}

// Build tools benefit from a stable, recognisable palette more than from the
// full hue range used by Perfetto's generic slice colorizer. Favour blues,
// greens, and teals, with restrained warm and violet accents for separation.
const SHELL_COLOR = buildprofColor("#8276a8");
const C_COMPILER_COLOR = buildprofColor("#4f8fc9");
const RUST_COMPILER_COLOR = buildprofColor("#63a889");
const LINKER_COLOR = buildprofColor("#4e9e9a");
const BUILD_SYSTEM_COLOR = buildprofColor("#b28a52");
const SCRIPT_COLOR = buildprofColor("#68a6b8");
const TOOL_COLORS: ReadonlyArray<ColorScheme> = [
  buildprofColor("#568fc2"),
  buildprofColor("#5ca58d"),
  buildprofColor("#559fa6"),
  buildprofColor("#7196bd"),
  buildprofColor("#74a47f"),
  buildprofColor("#8a7db3"),
  buildprofColor("#57a0b5"),
  buildprofColor("#809b70"),
  buildprofColor("#b38b55"),
  buildprofColor("#a6755c"),
  buildprofColor("#6f82b5"),
  buildprofColor("#9b9061"),
];

function stableColorIndex(name: string): number {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = Math.imul(hash, 31) + name.charCodeAt(i);
  }
  return (hash >>> 0) % TOOL_COLORS.length;
}

function buildprofSliceColor(name: string): ColorScheme {
  const tool = (name.split("/").pop() ?? name).toLowerCase();
  if (/^(ba|da|z|fi)?sh$/.test(tool)) return SHELL_COLOR;
  if (/^rustc(?:[-.][\d.]+)?$/.test(tool)) return RUST_COMPILER_COLOR;
  if (/^(?:clang|gcc|g\+\+|cc|c\+\+)(?:[-.][\d.]+)?$/.test(tool)) {
    return C_COMPILER_COLOR;
  }
  if (/^(?:ld|lld|mold|ar|ranlib)(?:[-.][\d.]+)?$/.test(tool)) {
    return LINKER_COLOR;
  }
  if (/^(?:make|ninja|cmake|cargo|bazel|meson)$/.test(tool)) {
    return BUILD_SYSTEM_COLOR;
  }
  if (/^(?:python\d*|node|perl|ruby)$/.test(tool)) return SCRIPT_COLOR;
  return TOOL_COLORS[stableColorIndex(tool)];
}

const CONCURRENCY_COLOR_BANDS: ReadonlyArray<ConcurrencyColorBand> = [
  {
    maximumLeafCount: IDLE_LEAF_COUNT,
    color: makeColorScheme(new HSLColor("#b0b0b0")),
  },
  {
    maximumLeafCount: SINGLE_LEAF_COUNT,
    color: makeColorScheme(new HSLColor("#f2c66d")),
  },
  { maximumLeafCount: 2, color: makeColorScheme(new HSLColor("#b7d7f0")) },
  { maximumLeafCount: 4, color: makeColorScheme(new HSLColor("#91c4e8")) },
  { maximumLeafCount: 8, color: makeColorScheme(new HSLColor("#6fadd8")) },
  { maximumLeafCount: 16, color: makeColorScheme(new HSLColor("#5796c4")) },
];

const HIGH_CONCURRENCY_COLOR = makeColorScheme(new HSLColor("#417eaa"));

function concurrencyColor(leafCount: number): ColorScheme {
  for (const band of CONCURRENCY_COLOR_BANDS) {
    if (leafCount <= band.maximumLeafCount) return band.color;
  }
  return HIGH_CONCURRENCY_COLOR;
}

async function buildConcurrencyTrack(trace: Trace): Promise<void> {
  const e = trace.engine;

  await createPerfettoTable({
    engine: e,
    name: T.occupancy,
    as: `
      with
        child_intervals as (
          select ppid, ts, ts + dur as end_ts,
                 max(ts + dur) over (
                   partition by ppid order by ts, pid
                   rows between unbounded preceding and 1 preceding
                 ) as previous_end
          from ${T.life}
          where ppid != 0
        ),
        marked_child_intervals as (
          select *,
                 iif(previous_end is null or ts > previous_end, 1, 0)
                   as starts_group
          from child_intervals
        ),
        grouped_child_intervals as (
          select *,
                 sum(starts_group) over (
                   partition by ppid order by ts, end_ts
                 ) as group_id
          from marked_child_intervals
        ),
        -- A parent is removed from the leaf count once, even while several of
        -- its children overlap. Merge those child lifetimes before emitting
        -- the parent's counter deltas.
        parent_covered as (
          select ppid, min(ts) as ts, max(end_ts) as end_ts
          from grouped_child_intervals
          group by ppid, group_id
        ),
        events as (
          select ts, 1 as delta from ${T.life}
          union all select ts + dur, -1 from ${T.life}
          union all select ts, -1 from parent_covered
          union all select end_ts, 1 from parent_covered
        ),
        deltas as (
          select ts, sum(delta) as delta from events group by ts
        )
      select row_number() over (order by ts) as id, ts,
             sum(delta) over (order by ts) as value
      from deltas
    `,
  });

  const peakRow = await e.query(
    `select ifnull(max(value), 0) as peak from ${T.occupancy}`,
  );
  const peak = peakRow.firstRow({ peak: NUM }).peak;

  // Turn every counter step into an exact phase lasting until the next change.
  await createPerfettoTable({
    engine: e,
    name: T.concurrency,
    as: `
      with steps as (
        select id, ts, value, lead(ts) over (order by ts) as next_ts
        from ${T.occupancy}
      )
      select
        id,
        ts,
        next_ts - ts as dur,
        value,
        case
          when value = ${IDLE_LEAF_COUNT} then 'Idle'
          when value = ${SINGLE_LEAF_COUNT} then '1 active leaf process'
          else value || ' active leaf processes'
        end as name
      from steps
      where next_ts is not null and next_ts > ts
    `,
  });

  const uri = "buildprof.concurrency";
  trace.tracks.registerTrack({
    uri,
    renderer: SliceTrack.create({
      trace,
      uri,
      dataset: new SourceDataset({
        schema: { id: NUM, ts: LONG, dur: LONG, name: STR, value: NUM },
        src: T.concurrency,
      }),
      colorizer: (row) => concurrencyColor(row.value),
      detailsPanel: (row) => new ConcurrencyDetailsPanel(trace, row.value),
    }),
  });
  trace.defaultWorkspace.addChildLast(
    new TrackNode({
      uri,
      name: `Build concurrency \u00b7 peak ${peak}`,
    }),
  );
}

class ConcurrencyDetailsPanel implements TrackEventDetailsPanel {
  private active: Array<{
    name: string;
    pid: number;
    sliceId: number;
    overlapNs: bigint;
  }> = [];
  private phaseDuration = 0n;

  constructor(
    private readonly trace: Trace,
    private readonly leafCount: number,
  ) {}

  async load(selection: { ts: bigint; dur?: bigint }): Promise<void> {
    const start = selection.ts;
    const duration = selection.dur ?? 0n;
    this.phaseDuration = duration;
    const rows = await this.trace.engine.query(`
      select p.name, p.pid, p.slice_id, ${duration} as overlap
      from ${T.life} p
      where p.ts <= ${start} and p.ts + p.dur > ${start}
        and not exists (
          select 1 from ${T.life} child
          where child.ppid = p.pid
            and child.ts <= ${start}
            and child.ts + child.dur > ${start}
        )
      order by p.dur desc
      limit ${CHILD_ROW_LIMIT}
    `);
    this.active = [];
    const it = rows.iter({ name: STR, pid: NUM, slice_id: NUM, overlap: LONG });
    for (; it.valid(); it.next()) {
      this.active.push({
        name: it.name,
        pid: it.pid,
        sliceId: it.slice_id,
        overlapNs: it.overlap,
      });
    }
  }

  render(): m.Children {
    const description =
      this.leafCount === IDLE_LEAF_COUNT
        ? "idle"
        : `${this.leafCount} active leaf ${this.leafCount === SINGLE_LEAF_COUNT ? "process" : "processes"}`;
    return m(
      DetailsShell,
      { title: "Build concurrency", description },
      m(
        Section,
        { title: "Phase" },
        m(
          Tree,
          m(TreeNode, {
            left: "Active leaf processes",
            right: `${this.leafCount}`,
          }),
          m(TreeNode, {
            left: "Duration",
            right: formatDuration(this.phaseDuration),
          }),
          ...this.active.map((leaf) =>
            m(TreeNode, {
              left: plainLabel(
                m(
                  Anchor,
                  {
                    icon: "arrow_forward",
                    title: leaf.name,
                    onclick: () => {
                      void this.trace.selection
                        .selectTrackEvent(PROCESS_TRACK_URI, leaf.sliceId)
                        .then(() =>
                          this.trace.selection.scrollToSelection("focus"),
                        );
                    },
                  },
                  `${elidePathHead(leaf.name)} [${leaf.pid}]`,
                ),
              ),
              right: formatDuration(leaf.overlapNs),
            }),
          ),
        ),
      ),
    );
  }
}

interface CompilerInvocation {
  readonly sourcePid: number;
  readonly pid: number;
  readonly backend: string;
  readonly name: string;
  readonly tracks: ReadonlyArray<{ id: number; threadId: number }>;
  readonly uri: string;
}

interface CompilerEventRow {
  readonly id: number;
  readonly ts: bigint;
  readonly dur: bigint;
  readonly name: string;
  readonly backend: string;
  readonly category: string;
  readonly arg_set_id: number;
}

class CompilerEventDetailsPanel implements TrackEventDetailsPanel {
  private args?: ArgsDict;
  private loaded = false;

  constructor(
    private readonly trace: Trace,
    private readonly row: CompilerEventRow,
  ) {}

  async load(): Promise<void> {
    this.args = await getArgs(
      this.trace.engine,
      asArgSetId(this.row.arg_set_id),
    );
    this.loaded = true;
  }

  render(): m.Children {
    if (!this.loaded) return m("h2", "Loading");
    return m(
      DetailsShell,
      { title: "Compiler event" },
      m(GridLayout, [
        m(
          Section,
          { title: "Details" },
          m(Tree, [
            m(TreeNode, { left: "Name", right: this.row.name }),
            m(TreeNode, { left: "Backend", right: this.row.backend }),
            this.row.category !== "" &&
              m(TreeNode, { left: "Category", right: this.row.category }),
            m(TreeNode, {
              left: "Start time",
              right: m(Timestamp, {
                trace: this.trace,
                ts: Time.fromRaw(this.row.ts),
              }),
            }),
            m(TreeNode, {
              left: "Duration",
              right: m(DurationWidget, {
                trace: this.trace,
                dur: this.row.dur,
              }),
            }),
          ]),
        ),
        this.args !== undefined &&
          m(
            Section,
            { title: "Arguments" },
            m(Tree, renderArguments(this.trace, this.args)),
          ),
      ]),
    );
  }

  isLoading(): boolean {
    return !this.loaded;
  }
}

/** Materialise a compiler invocation only when its track becomes visible. */
class LazyCompilerTrack implements TrackRenderer {
  private delegate?: TrackRenderer;
  private loading?: Promise<TrackRenderer>;
  private error?: string;

  constructor(
    private readonly trace: Trace,
    private readonly create: () => Promise<TrackRenderer>,
  ) {}

  private ensureLoaded(): Promise<TrackRenderer> {
    if (this.delegate !== undefined) return Promise.resolve(this.delegate);
    if (this.loading !== undefined) return this.loading;
    this.loading = this.create().then(
      (delegate) => {
        this.delegate = delegate;
        this.trace.raf.scheduleFullRedraw();
        return delegate;
      },
      (error: unknown) => {
        this.error = error instanceof Error ? error.message : String(error);
        this.trace.raf.scheduleFullRedraw();
        throw error;
      },
    );
    return this.loading;
  }

  render(ctx: TrackRenderContext): void {
    if (this.delegate !== undefined) {
      this.delegate.render(ctx);
      return;
    }
    if (this.loading === undefined) void this.ensureLoaded();
    if (this.error === undefined) {
      checkerboard(ctx.ctx, this.getHeight(), 0, ctx.size.width);
    } else {
      ctx.ctx.fillStyle = ctx.colors.COLOR_TEXT_MUTED;
      ctx.ctx.fillText(`Could not load compiler events: ${this.error}`, 8, 24);
    }
  }

  getHeight(): number {
    return this.delegate?.getHeight?.() ?? 40;
  }

  getSliceVerticalBounds(depth: number) {
    return this.delegate?.getSliceVerticalBounds?.(depth);
  }

  getDataset() {
    return this.delegate?.getDataset?.();
  }

  async getSelectionDetails(eventId: number) {
    const delegate = await this.ensureLoaded();
    return delegate.getSelectionDetails?.(eventId);
  }

  detailsPanel(
    selection: Parameters<NonNullable<TrackRenderer["detailsPanel"]>>[0],
  ) {
    return this.delegate?.detailsPanel?.(selection);
  }

  renderTooltip() {
    return this.delegate?.renderTooltip?.();
  }

  getTrackShellButtons() {
    return this.delegate?.getTrackShellButtons?.();
  }

  onMouseMove(event: TrackMouseEvent): void {
    this.delegate?.onMouseMove?.(event);
  }

  onMouseClick(event: TrackMouseEvent): boolean {
    return this.delegate?.onMouseClick?.(event) ?? false;
  }

  onMouseDoubleClick(event: TrackMouseEvent): boolean {
    return this.delegate?.onMouseDoubleClick?.(event) ?? false;
  }

  onMouseOut(): void {
    this.delegate?.onMouseOut?.();
  }
}

/** Compiler tracks stay out of the overview until requested or pinned. */
class CompilerTracks {
  private readonly invocations = new Map<number, CompilerInvocation[]>();
  private readonly registered = new Set<string>();

  constructor(private readonly trace: Trace) {}

  async register(): Promise<void> {
    await this.trace.engine.query(
      "include perfetto module intervals.overlap",
    );
    const [tracksResult, processesResult] = await Promise.all([
      this.trace.engine.query(`
        select id as track_id, name
        from track
        where name glob '* compiler [[]pid *] · thread *'
      `),
      this.trace.engine.query(`select pid, name from ${T.life}`),
    ]);

    const processNames = new Map<number, string>();
    const processes = processesResult.iter({ pid: NUM, name: STR });
    for (; processes.valid(); processes.next()) {
      processNames.set(processes.pid, processes.name);
    }

    const grouped = new Map<
      string,
      {
        pid: number;
        backend: string;
        tracks: Array<{ id: number; threadId: number }>;
      }
    >();
    const tracks = tracksResult.iter({ track_id: NUM, name: STR });
    for (; tracks.valid(); tracks.next()) {
      const match = /^(.+) compiler \[pid (-?\d+)\] · thread (\d+)$/.exec(
        tracks.name,
      );
      if (match === null) continue;
      const backend = match[1];
      const pid = Number(match[2]);
      const threadId = Number(match[3]);
      const key = `${pid}:${backend}`;
      const invocation = grouped.get(key) ?? { pid, backend, tracks: [] };
      invocation.tracks.push({ id: tracks.track_id, threadId });
      grouped.set(key, invocation);
    }

    for (const groupedInvocation of grouped.values()) {
      const invocation: CompilerInvocation = {
        sourcePid: groupedInvocation.pid,
        pid: groupedInvocation.pid,
        backend: groupedInvocation.backend,
        name:
          processNames.get(groupedInvocation.pid) ??
          `${groupedInvocation.backend} compiler`,
        tracks: groupedInvocation.tracks,
        uri: `buildprof.compiler.${groupedInvocation.backend.toLowerCase()}.${groupedInvocation.pid}.${groupedInvocation.pid}`,
      };
      const invocations = this.invocations.get(invocation.pid) ?? [];
      invocations.push(invocation);
      this.invocations.set(invocation.pid, invocations);
    }
    if (this.invocations.size === 0) return;

    // Raw TrackEvent tracks are an implementation detail. Buildprof presents
    // the same slices through invocation-oriented tracks and workspaces.
    for (const node of [...this.trace.defaultWorkspace.flatTracks]) {
      if (
        [...this.invocations.values()]
          .flat()
          .some((invocation) =>
            node.name.startsWith(
              `${invocation.backend} compiler [pid ${invocation.sourcePid}]`,
            ),
          )
      ) {
        node.remove();
      }
    }

    const workspace =
      this.trace.workspaces.createEmptyWorkspace("Compiler details");
    for (const invocation of [...this.invocations.values()].flat()) {
      this.ensureTrack(invocation);
      workspace.addChildLast(this.node(invocation));
    }
  }

  has(pid: number): boolean {
    return (this.invocations.get(pid)?.length ?? 0) > 0;
  }

  show(pid: number): void {
    const invocations = this.invocations.get(pid);
    if (invocations === undefined) return;
    const workspace = this.trace.currentWorkspace;
    for (const invocation of invocations) {
      this.ensureTrack(invocation);
      let node = workspace.getTrackByUri(invocation.uri);
      if (node === undefined) {
        node = this.node(invocation);
        workspace.addChildLast(node);
      }
    }
    this.trace.raf.scheduleFullRedraw();
    this.trace.scrollTo({
      track: { uri: invocations[0].uri, expandGroup: true },
    });
  }

  private ensureTrack(invocation: CompilerInvocation): void {
    this.registerSummaryTrack(invocation);
    for (const track of invocation.tracks) {
      this.registerTrack(
        invocation,
        compilerThreadUri(invocation, track.threadId),
        compilerThreadEventsSql(invocation, track.id),
      );
    }
  }

  private registerSummaryTrack(invocation: CompilerInvocation): void {
    if (this.registered.has(invocation.uri)) return;
    this.registered.add(invocation.uri);
    const dataset = new SourceDataset({
      src: compilerSummarySql(invocation),
      schema: {
        id: NUM,
        ts: LONG,
        dur: LONG,
        name: STR,
        depth: NUM,
        value: NUM,
      },
    });
    this.trace.tracks.registerTrack({
      uri: invocation.uri,
      renderer: new LazyCompilerTrack(this.trace, () =>
        SliceTrack.createMaterialized({
          trace: this.trace,
          uri: invocation.uri,
          dataset,
          colorizer: (row) => concurrencyColor(row.value),
        }),
      ),
    });
  }

  private registerTrack(
    invocation: CompilerInvocation,
    uri: string,
    src: string,
  ): void {
    if (this.registered.has(uri)) return;
    this.registered.add(uri);
    const dataset = new SourceDataset({
      src,
      schema: {
        id: NUM,
        ts: LONG,
        dur: LONG,
        name: STR,
        depth: NUM,
        pid: NUM,
        backend: STR,
        category: STR,
        arg_set_id: NUM,
      },
    });
    this.trace.tracks.registerTrack({
      uri,
      renderer: new LazyCompilerTrack(this.trace, () =>
        SliceTrack.createMaterialized({
          trace: this.trace,
          uri,
          dataset,
          colorizer: (row) => buildprofSliceColor(row.name),
          detailsPanel: (row) =>
            new CompilerEventDetailsPanel(this.trace, row),
        }),
      ),
    });
  }

  private node(invocation: CompilerInvocation): TrackNode {
    const group = new TrackNode({
      uri: invocation.uri,
      name: `${invocation.name} · ${invocation.backend} · pid ${invocation.pid}`,
      isSummary: true,
      collapsed: true,
      removable: true,
    });
    for (const track of invocation.tracks) {
      group.addChildLast(
        new TrackNode({
          uri: compilerThreadUri(invocation, track.threadId),
          name: `Thread ${track.threadId}`,
          subtitle: `${invocation.backend} · pid ${invocation.pid}`,
        }),
      );
    }
    return group;
  }
}

function compilerThreadUri(
  invocation: CompilerInvocation,
  threadId: number,
): string {
  return `${invocation.uri}.thread.${threadId}`;
}

/** Show how many compiler threads are doing work at each point in time. */
function compilerSummarySql(invocation: CompilerInvocation): string {
  const trackIds = invocation.tracks.map((track) => track.id).join(",");
  return `
    with counts as (
      select ts,
             value,
             lead(ts) over (order by ts) as next_ts
      from intervals_overlap_count!((
        select ts, max(dur, 1) as dur
        from slice
        where track_id in (${trackIds}) and depth = 0
      ), ts, dur)
    )
    select row_number() over (order by ts) as id,
           ts, next_ts - ts as dur,
           case value
             when 1 then '1 active compiler thread'
             else value || ' active compiler threads'
           end as name,
           0 as depth,
           value
    from counts
    where value > 0 and next_ts > ts
  `;
}

/** Preserve the compiler's native nesting within one source thread. */
function compilerThreadEventsSql(
  invocation: CompilerInvocation,
  trackId: number,
): string {
  const backend = invocation.backend.replaceAll("'", "''");
  return `
    select s.id, s.ts, max(s.dur, 1) as dur, s.name, s.arg_set_id,
           ${invocation.pid} as pid, '${backend}' as backend,
           extract_arg(s.arg_set_id, 'debug.compiler_category') as category,
           s.depth as depth
    from slice s
    where s.track_id = ${trackId}
      and s.category = 'buildprof.compiler'
  `;
}

/**
 * Every process, laid out with real containment.
 *
 * The dataset's `depth` column carries the containment layout.
 */
class ProcessDetailsPanels {
  private readonly panels = new Map<number, ProcessDetailsPanel>();

  constructor(
    private readonly trace: Trace,
    private readonly dependencyArrows: DependencyArrows,
    private readonly compilerTracks: CompilerTracks,
  ) {}

  get(pid: number): ProcessDetailsPanel {
    const cached = this.panels.get(pid);
    if (cached !== undefined) return cached;
    const panel = new ProcessDetailsPanel(
      this.trace,
      pid,
      this.dependencyArrows,
      this.compilerTracks,
    );
    this.panels.set(pid, panel);
    return panel;
  }
}

async function buildProcessTreeTrack(
  trace: Trace,
  detailsPanels: ProcessDetailsPanels,
): Promise<void> {
  await createPerfettoTable({
    engine: trace.engine,
    as: `
      -- The timeline has one slice per process; details retain exec segments.
      select
        life.slice_id       as id,
        life.ts             as ts,
        max(life.dur, 1)    as dur,
        life.name           as name,
        life.tool           as tool,
        life.pid            as pid,
        layout.layout_depth as depth
      from ${T.life} life
      join ${T.layout} layout on layout.id = life.pid
      -- SliceTrack resolves selections by id.
      order by life.slice_id
    `,
    name: T.tree,
  });

  const shape = await trace.engine.query(
    `select count(*) as cnt, max(depth) + 1 as rows from ${T.tree}`,
  );
  const { cnt } = shape.firstRow({ cnt: NUM, rows: NUM });

  const uri = PROCESS_TRACK_URI;
  const track = SliceTrack.create({
    trace,
    uri,
    dataset: new SourceDataset({
      schema: {
        id: NUM,
        ts: LONG,
        dur: LONG,
        name: STR,
        depth: NUM,
        pid: NUM,
        tool: STR,
      },
      src: T.tree,
    }),
    // Keep tools visually consistent across unique action labels.
    colorizer: (row) => buildprofSliceColor(row.tool),
    detailsPanel: (row) => detailsPanels.get(row.pid),
  });
  trace.tracks.registerTrack({ uri, renderer: track });
  trace.defaultWorkspace.addChildLast(
    new TrackNode({
      uri,
      name: `Process tree \u00b7 ${cnt} processes`,
    }),
  );
}

function registerBuildprofSearch(trace: Trace): void {
  trace.search.registerSearchProvider({
    name: "Buildprof process names",
    selectTracks: (tracks) =>
      tracks.filter((track) => track.uri === PROCESS_TRACK_URI),
    async getSearchFilter(searchTerm) {
      const query = escapeSearchQuery(searchTerm);
      return {
        where: `name GLOB ${query}`,
        columns: { name: STR_NULL },
      };
    },
  });

  trace.search.registerSearchProvider({
    name: "Visible Buildprof compiler phases",
    selectTracks: (tracks) => {
      const workspaceUris = new Set(
        trace.currentWorkspace.flatTracks
          .map((node) => node.uri)
          .filter((uri): uri is string => uri !== undefined),
      );
      for (const node of trace.currentWorkspace.pinnedTracks) {
        if (node.uri !== undefined) workspaceUris.add(node.uri);
      }
      return tracks.filter(
        (track) =>
          track.uri.startsWith("buildprof.compiler.") &&
          workspaceUris.has(track.uri),
      );
    },
    async getSearchFilter(searchTerm) {
      const query = escapeSearchQuery(searchTerm);
      return {
        where: `name GLOB ${query}`,
        columns: { name: STR_NULL },
      };
    },
  });
}

// Open flags meaning the process intended to modify the file. O_WRONLY(1) and
// O_RDWR(2) live in the low two bits; O_CREAT is 0o100.
const O_ACCMODE = 3;
const O_CREAT = 64;

const WRITE_INTENT_SQL = `((flags & ${O_ACCMODE}) != 0 or (flags & ${O_CREAT}) != 0)`;

// Files are shown per-path with a count rather than one row per open: a single
// compile opens the same header dozens of times.
const FILE_ROW_LIMIT = 12;

const DEP_ROW_LIMIT = 8;

// Bound inline children; the grid exposes the full set.
const CHILD_ROW_LIMIT = 10;

interface Segment {
  readonly pid: number;
  readonly ppid: number;
  readonly name: string;
  readonly cmd: string;
  readonly cwd: string;
  readonly tsNs: bigint;
  readonly durNs: bigint;
  readonly exitCode: number | null;
}

interface ProcInfo {
  readonly pid: number;
  readonly ppid: number;
  readonly name: string;
  readonly lifetimeNs: bigint;
  readonly exitCode: number | null;
  readonly segments: ReadonlyArray<Segment>;
}

interface DepUse {
  pid: number;
  name: string;
  sliceId: number;
  files: number;
  sample: string;
}

interface FileUse {
  readonly path: string;
  readonly writeIntent: boolean;
  readonly count: number;
}

function formatDuration(ns: bigint): string {
  const n = Number(ns);
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)} s`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)} ms`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(2)} µs`;
  return `${n} ns`;
}

/**
 * Reduce exec segments to processes.
 *
 * Lifetime spans every segment and exit status comes from the final segment.
 */
function toProcesses(segments: ReadonlyArray<Segment>): ProcInfo[] {
  const byPid = new Map<number, Segment[]>();
  for (const seg of segments) {
    const existing = byPid.get(seg.pid);
    if (existing === undefined) byPid.set(seg.pid, [seg]);
    else existing.push(seg);
  }
  const out: ProcInfo[] = [];
  for (const segs of byPid.values()) {
    const first = segs[0];
    const last = segs[segs.length - 1];
    let exitCode: number | null = null;
    for (const seg of segs) {
      if (seg.exitCode !== null) exitCode = seg.exitCode;
    }
    out.push({
      pid: first.pid,
      ppid: first.ppid,
      name: last.name,
      lifetimeNs: last.tsNs + last.durNs - first.tsNs,
      exitCode,
      segments: segs,
    });
  }
  return out.sort((a, b) => Number(b.lifetimeNs - a.lifetimeNs));
}
const PATH_CHARS = 52;

/**
 * Shorten a path from the front, keeping the tail.
 *
 * Preserve the filename and trailing directories.
 */
function elidePathHead(path: string): string {
  if (path.length <= PATH_CHARS) return path;
  return "\u2026" + path.slice(path.length - PATH_CHARS + 1);
}

/**
 * What a pid's exec segments actually were, in one or two sentences.
 *
 * Describes multiple exec segments sharing one pid.
 */
function explainExecs(proc: ProcInfo): string | undefined {
  if (proc.segments.length < 2) return undefined;

  const first = proc.segments[0];
  const last = proc.segments[proc.segments.length - 1];
  const from = basename(executableOf(first.cmd));
  const to = executableOf(last.cmd);

  if (proc.segments.length === 2) {
    const line =
      `${from} ran ${formatDuration(first.durNs)}, then replaced itself ` +
      `with ${to}.`;
    return from === basename(to)
      ? `${line} Both are called ${from}; the first one picks which binary to run.`
      : line;
  }

  let beforeLast = 0n;
  for (let i = 0; i < proc.segments.length - 1; i++) {
    beforeLast += proc.segments[i].durNs;
  }
  return (
    `${proc.segments.length} programs ran under this pid, ending with ${to}. ` +
    `${formatDuration(beforeLast)} passed before the last one started.`
  );
}

function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? path : path.slice(slash + 1);
}

const EXEC_HELP =
  "execve() swaps the program running under a pid: same process, new binary. " +
  "No child is created, so a handoff like this never shows up in the process " +
  "tree.";

/**
 * A command line rendered one argument per line.
 *
 * Long commands remain scannable when arguments are stacked.
 */
function commandLines(cmd: string): m.Children {
  const args = cmd.split(" ").filter((arg) => arg.length > 0);
  return m(
    "span",
    { style: { whiteSpace: "pre-wrap", wordBreak: "break-all" } },
    args.join("\n"),
  );
}

function executableOf(cmd: string): string {
  const space = cmd.indexOf(" ");
  return space === -1 ? cmd : cmd.slice(0, space);
}

/** Arrows are capped: a widely-read generated header has hundreds of readers. */
const ARROW_LIMIT = 24;

const PRODUCER_ARROW = "hsla(210, 90%, 55%, 0.9)";
const CONSUMER_ARROW = "hsla(28, 95%, 50%, 0.9)";

/**
 * Producer/consumer arrows for whatever is selected.
 *
 * The overlay's getter runs every frame, so it may only return the last
 * computed answer; the query behind it runs once per selection and asks for a
 * redraw when it lands.
 */
class DependencyArrows {
  private arrows: ArrowConnection[] = [];
  private key = "";
  private enabled = false;

  constructor(private readonly trace: Trace) {}

  isEnabled(): boolean {
    return this.enabled;
  }

  setEnabled(enabled: boolean): void {
    if (this.enabled === enabled) return;
    this.enabled = enabled;
    this.trace.raf.scheduleFullRedraw();
  }

  connections(): ArrowConnection[] {
    if (!this.enabled) return [];
    const sel = this.trace.selection.selection;
    if (sel.kind !== "track_event" || sel.trackUri !== PROCESS_TRACK_URI) {
      this.key = "";
      this.arrows = [];
      return this.arrows;
    }
    const key = `${sel.eventId}`;
    if (key !== this.key) {
      this.key = key;
      this.arrows = [];
      void this.refresh(sel.eventId, key);
    }
    return this.arrows;
  }

  private async refresh(sliceId: number, key: string): Promise<void> {
    await ensureEdges(this.trace);
    // Fetch both directions while preserving writer-to-reader arrow direction.
    const result = await this.trace.engine.query(`
      with me as (select pid from ${T.tree} where id = ${sliceId})
      select
        case when a.ts < b.ts + b.dur and b.ts < a.ts + a.dur
          then max(a.ts, b.ts)
          else a.ts + a.dur
        end as sts,
        a.depth as sdep,
        case when a.ts < b.ts + b.dur and b.ts < a.ts + a.dur
          then max(a.ts, b.ts)
          else b.ts
        end as ets,
        b.depth as edep,
        (e.src = (select pid from me)) as outgoing
      from ${T.edge} e
      join ${T.tree} a on a.pid = e.src
      join ${T.tree} b on b.pid = e.dst
      where e.src = (select pid from me) or e.dst = (select pid from me)
      order by e.files desc
      limit ${ARROW_LIMIT}
    `);
    if (this.key !== key) return;
    const next: ArrowConnection[] = [];
    const it = result.iter({
      sts: LONG,
      sdep: NUM,
      ets: LONG,
      edep: NUM,
      outgoing: NUM,
    });
    for (; it.valid(); it.next()) {
      next.push({
        start: {
          trackUri: PROCESS_TRACK_URI,
          ts: Time.fromRaw(it.sts),
          depth: it.sdep,
        },
        end: {
          trackUri: PROCESS_TRACK_URI,
          ts: Time.fromRaw(it.ets),
          depth: it.edep,
        },
        color: it.outgoing !== 0 ? CONSUMER_ARROW : PRODUCER_ARROW,
      });
    }
    this.arrows = next;
    m.redraw();
  }
}

class ProcessDetailsPanel implements TrackEventDetailsPanel {
  private proc?: ProcInfo;
  private parent?: ProcInfo;
  private children: Array<{
    pid: number;
    name: string;
    lifetimeNs: bigint;
    sliceId: number;
  }> = [];
  private parentSliceId?: number;
  private childCount = 0;
  private childTotal = 0n;
  private files: FileUse[] = [];
  private totalOpens = 0;
  private pathCount = 0;
  private readCount = 0;
  private producers: DepUse[] = [];
  private consumers: DepUse[] = [];
  private producerCount = 0;
  private consumerCount = 0;

  constructor(
    private readonly trace: Trace,
    private readonly pid: number,
    private readonly dependencyArrows: DependencyArrows,
    private readonly compilerTracks: CompilerTracks,
  ) {}

  /**
   * Load independent panel data concurrently, then resolve the parent.
   */
  async load(): Promise<void> {
    const e = this.trace.engine;
    const byPath = `
      select
        path,
        max((flags & ${O_ACCMODE}) != 0 or (flags & ${O_CREAT}) != 0)
          as write_intent,
        count(*) as cnt
      from ${T.open}
      where pid = ${this.pid}
      group by path
    `;

    const [segments, childTotals, childTop, fileTotals, fileRows] =
      await Promise.all([
        this.querySegments(`pid = ${this.pid}`),
        e.query(`
          select count(*) as cnt, ifnull(sum(dur), 0) as total
          from ${T.life} where ppid = ${this.pid}
        `),
        e.query(`
          select pid, name, dur, slice_id from ${T.life}
          where ppid = ${this.pid}
          order by dur desc limit ${CHILD_ROW_LIMIT}
        `),
        e.query(`
          select
            count(*) as paths,
            ifnull(sum(cnt), 0) as opens,
            ifnull(sum(case when write_intent = 0 then 1 else 0 end), 0)
              as reads
          from (${byPath})
        `),
        e.query(`
          select path, write_intent, cnt from (${byPath})
          order by write_intent desc, cnt desc, path
          limit ${FILE_ROW_LIMIT * 3}
        `),
      ]);

    const proc = toProcesses(segments)[0];
    if (proc === undefined) return;
    this.proc = proc;

    const ctot = childTotals.firstRow({ cnt: NUM, total: LONG });
    this.childCount = ctot.cnt;
    this.childTotal = ctot.total;

    this.children = [];
    const ct = childTop.iter({
      pid: NUM,
      name: STR,
      dur: LONG,
      slice_id: NUM,
    });
    for (; ct.valid(); ct.next()) {
      this.children.push({
        pid: ct.pid,
        name: ct.name,
        lifetimeNs: ct.dur,
        sliceId: ct.slice_id,
      });
    }

    const ftot = fileTotals.firstRow({ paths: NUM, opens: NUM, reads: NUM });
    this.pathCount = ftot.paths;
    this.totalOpens = ftot.opens;
    this.readCount = ftot.reads;

    this.files = [];
    const it = fileRows.iter({ path: STR, write_intent: NUM, cnt: NUM });
    for (; it.valid(); it.next()) {
      this.files.push({
        path: it.path,
        writeIntent: it.write_intent !== 0,
        count: it.cnt,
      });
    }

    await this.loadDependencies();

    if (proc.ppid !== 0) {
      this.parent = toProcesses(
        await this.querySegments(`pid = ${proc.ppid}`),
      )[0];
      const ps = await this.trace.engine.query(
        `select slice_id from ${T.life} where pid = ${proc.ppid}`,
      );
      if (ps.numRows() > 0) {
        this.parentSliceId = ps.firstRow({ slice_id: NUM }).slice_id;
      }
    }
  }

  private async querySegments(where: string): Promise<Segment[]> {
    const result = await this.trace.engine.query(`
      select pid, ppid, cmd, cwd, exit_code, tool as name, ts, dur
      from ${T.seg}
      where ${where}
      order by ts
    `);
    const out: Segment[] = [];
    const it = result.iter({
      pid: NUM,
      ppid: NUM,
      cmd: STR,
      cwd: STR,
      exit_code: NUM_NULL,
      name: STR,
      ts: LONG,
      dur: LONG,
    });
    for (; it.valid(); it.next()) {
      out.push({
        pid: it.pid,
        ppid: it.ppid,
        name: it.name,
        cmd: it.cmd,
        cwd: it.cwd,
        tsNs: it.ts,
        durNs: it.dur,
        exitCode: it.exit_code,
      });
    }
    return out;
  }

  /**
   * Who made what this action read, and who read what it made.
   */
  private async loadDependencies(): Promise<void> {
    await ensureEdges(this.trace);
    const side = (column: "src" | "dst") => `
      select l.pid as pid, l.name as name, l.slice_id as slice_id,
             e.files as files, e.sample as sample
      from ${T.edge} e
      join ${T.life} l on l.pid = e.${column === "src" ? "src" : "dst"}
      where e.${column === "src" ? "dst" : "src"} = ${this.pid}
      order by e.files desc
      limit ${DEP_ROW_LIMIT}
    `;
    const counts = `
      select
        (select count(*) from ${T.edge} where dst = ${this.pid}) as producers,
        (select count(*) from ${T.edge} where src = ${this.pid}) as consumers
    `;
    const [up, down, totals] = await Promise.all([
      this.trace.engine.query(side("src")),
      this.trace.engine.query(side("dst")),
      this.trace.engine.query(counts),
    ]);
    const collect = (result: typeof up): DepUse[] => {
      const rows: DepUse[] = [];
      const it = result.iter({
        pid: NUM,
        name: STR,
        slice_id: NUM,
        files: NUM,
        sample: STR,
      });
      for (; it.valid(); it.next()) {
        rows.push({
          pid: it.pid,
          name: it.name,
          sliceId: it.slice_id,
          files: it.files,
          sample: it.sample,
        });
      }
      return rows;
    };
    this.producers = collect(up);
    this.consumers = collect(down);
    const t = totals.firstRow({ producers: NUM, consumers: NUM });
    this.producerCount = t.producers;
    this.consumerCount = t.consumers;
  }

  private renderDeps(
    title: string,
    rows: DepUse[],
    total: number,
    open: () => void,
  ): m.Children {
    if (total === 0) {
      return m(TreeNode, { left: title, right: "nothing" });
    }
    return m(
      TreeNode,
      { left: title, right: `${total} ${total === 1 ? "action" : "actions"}` },
      ...rows.map((d) =>
        m(TreeNode, {
          left: plainLabel(
            this.processLink(
              `${elidePathHead(d.name)} [${d.pid}]`,
              d.sliceId,
              d.name,
            ),
          ),
          right: m(
            "span",
            { title: d.sample },
            basename(d.sample) + (d.files > 1 ? ` +${d.files - 1}` : ""),
          ),
        }),
      ),
      m(TreeNode, {
        left: m(Button, {
          label:
            total > rows.length
              ? `Show all ${total}\u2026`
              : "Open as a table\u2026",
          compact: true,
          onclick: open,
        }),
      }),
    );
  }

  private renderFiles(): m.Children {
    const writes = this.pathCount - this.readCount;
    const shown = this.files.slice(0, FILE_ROW_LIMIT);
    if (this.pathCount === 0) {
      return m(Tree, m(TreeNode, { left: "Opened", right: "nothing" }));
    }
    return m(
      Tree,
      m(TreeNode, { left: "Paths", right: `${this.pathCount}` }),
      m(TreeNode, { left: "Opens", right: `${this.totalOpens}` }),
      m(TreeNode, { left: "Read", right: `${this.readCount}` }),
      m(TreeNode, { left: "Written", right: `${writes}` }),
      m(
        TreeNode,
        { left: this.pathCount > shown.length ? "Most opened" : "Files" },
        ...shown.map((f) =>
          m(TreeNode, {
            left: plainLabel(
              m(
                "span",
                { title: f.path },
                f.writeIntent
                  ? m("span", { style: { opacity: "0.55" } }, "W ")
                  : undefined,
                elidePathHead(f.path),
              ),
            ),
            right: `${f.count}\u00d7`,
          }),
        ),
        m(TreeNode, {
          left: m(Button, {
            label:
              this.pathCount > shown.length
                ? `Show all ${this.pathCount} paths\u2026`
                : "Open as a table\u2026",
            compact: true,
            onclick: () => this.openFilesTab(),
          }),
        }),
      ),
    );
  }

  private processLink(
    label: string,
    sliceId: number | undefined,
    title?: string,
  ): m.Children {
    if (sliceId === undefined) return label;
    return m(
      Anchor,
      {
        icon: "arrow_forward",
        title,
        onclick: () => {
          void this.trace.selection
            .selectTrackEvent(PROCESS_TRACK_URI, sliceId)
            .then(() => this.trace.selection.scrollToSelection("focus"));
        },
      },
      label,
    );
  }

  private openChildrenTab(): void {
    openGridTab(
      this.trace,
      `buildprof.children.${this.pid}`,
      `Children of ${this.proc?.name ?? this.pid}`,
      `${this.childCount} processes spawned by ` +
        `${this.proc?.name ?? this.pid} [${this.pid}]`,
      `(select name, pid, dur, tool, dir, slice_id from ${T.life}
         where ppid = ${this.pid})`,
      {
        slice_id: makeJumpColumn(this.trace),
        name: { title: "Action", columnType: "text" },
        pid: { title: "PID", columnType: "quantitative" },
        dur: {
          title: "Lifetime",
          columnType: "quantitative",
          cellRenderer: DURATION_CELL,
        },
        tool: { title: "Tool", columnType: "text" },
        dir: { title: "Directory", columnType: "text" },
      },
      [
        { id: "slice_id", field: "slice_id" },
        { id: "tool", field: "tool" },
        { id: "name", field: "name" },
        { id: "dir", field: "dir" },
        { id: "pid", field: "pid" },
        { id: "dur", field: "dur", sort: "DESC" as const },
      ],
      [
        { label: "Tool", pivot: makeBuildPivot("tool") },
        { label: "Directory", pivot: makeBuildPivot("dir") },
        {
          label: "Tool \u203a Directory",
          pivot: makeBuildPivot("tool", "dir"),
        },
        { label: "Ungrouped", pivot: FLAT_PIVOT },
      ],
    );
  }

  private openDepsTab(direction: "producers" | "consumers"): void {
    const upstream = direction === "producers";
    const mine = upstream ? "dst" : "src";
    const other = upstream ? "src" : "dst";
    openGridTab(
      this.trace,
      `buildprof.deps.${direction}.${this.pid}`,
      `${upstream ? "Produced by" : "Consumed by"} ${this.proc?.name ?? this.pid}`,
      `${upstream ? this.producerCount : this.consumerCount} actions sharing ` +
        `files with ${this.proc?.name ?? this.pid} [${this.pid}]`,
      `(select l.name as name, l.pid as pid, l.tool as tool, l.dir as dir,
               l.dur as dur, l.slice_id as slice_id, e.files as files,
               e.sample as sample
          from ${T.edge} e
          join ${T.life} l on l.pid = e.${other}
         where e.${mine} = ${this.pid})`,
      {
        slice_id: makeJumpColumn(this.trace),
        name: { title: "Action", columnType: "text" },
        tool: { title: "Tool", columnType: "text" },
        dir: { title: "Directory", columnType: "text" },
        pid: { title: "PID", columnType: "quantitative" },
        sample: { title: "File", columnType: "text" },
        files: { title: "Files", columnType: "quantitative" },
        dur: {
          title: "Lifetime",
          columnType: "quantitative",
          cellRenderer: DURATION_CELL,
        },
      },
      [
        { id: "slice_id", field: "slice_id" },
        { id: "name", field: "name" },
        { id: "tool", field: "tool" },
        { id: "dir", field: "dir" },
        { id: "pid", field: "pid" },
        { id: "sample", field: "sample" },
        { id: "files", field: "files", sort: "DESC" as const },
        { id: "dur", field: "dur" },
      ],
      [
        { label: "Tool", pivot: makeBuildPivot("tool") },
        { label: "Directory", pivot: makeBuildPivot("dir") },
        { label: "Ungrouped", pivot: FLAT_PIVOT },
      ],
    );
  }

  private openFilesTab(): void {
    openGridTab(
      this.trace,
      `buildprof.files.${this.pid}`,
      `Files of ${this.proc?.name ?? this.pid}`,
      `${this.pathCount} paths, ${this.totalOpens} opens by ` +
        `${this.proc?.name ?? this.pid} [${this.pid}]`,
      `(select
          path,
          ${dirnameSql("path")} as dir,
          ${extensionSql("path")} as ext,
          count(*) as opens,
          sum(case when ${WRITE_INTENT_SQL} then 1 else 0 end) as writes,
          sum(case when ${WRITE_INTENT_SQL} then 0 else 1 end) as reads
        from ${T.open} where pid = ${this.pid} group by path)`,
      {
        dir: { title: "Directory", columnType: "text" },
        ext: { title: "Type", columnType: "text" },
        path: { title: "Path", columnType: "text" },
        opens: { title: "Opens", columnType: "quantitative" },
        reads: { title: "Reads", columnType: "quantitative" },
        writes: { title: "Writes", columnType: "quantitative" },
      },
      [
        { id: "dir", field: "dir" },
        { id: "ext", field: "ext" },
        { id: "path", field: "path" },
        { id: "opens", field: "opens", sort: "DESC" as const },
        { id: "reads", field: "reads" },
        { id: "writes", field: "writes" },
      ],
      [
        { label: "Path", pivot: makeFilePivot("path") },
        { label: "Directory", pivot: makeFilePivot("dir") },
        { label: "Type", pivot: makeFilePivot("ext") },
        { label: "Directory \u203a Type", pivot: makeFilePivot("dir", "ext") },
      ],
    );
  }

  render(): m.Children {
    const proc = this.proc;

    // Keep the panel structure stable while its values load.
    const pending = proc === undefined;
    const last = proc?.segments[proc.segments.length - 1];
    const blank = "";
    const execExplanation = proc ? explainExecs(proc) : undefined;

    return m(
      DetailsShell,
      {
        title: proc?.name ?? "Process",
        description: `pid ${this.pid}`,
        buttons: this.compilerTracks.has(this.pid)
          ? m(Button, {
              icon: "vertical_align_top",
              label: "Show compiler track",
              tooltip: "Add and view the detailed compiler track",
              onclick: () => this.compilerTracks.show(this.pid),
            })
          : undefined,
      },
      m(
        GridLayout,
        m(
          GridLayoutColumn,
          m(
            Section,
            { title: "Process tree" },
            m(
              Tree,
              m(TreeNode, {
                left: "Spawned by",
                right: this.parent
                  ? this.processLink(
                      `${elidePathHead(this.parent.name)} [${this.parent.pid}]`,
                      this.parentSliceId,
                      this.parent.name,
                    )
                  : pending
                    ? blank
                    : "the recording root",
              }),
              m(
                TreeNode,
                {
                  left: "Spawned",
                  right: pending
                    ? blank
                    : this.childCount === 0
                      ? "nothing"
                      : `${this.childCount} processes, ` +
                        `${formatDuration(this.childTotal)} total`,
                },
                ...this.children.map((c) =>
                  m(TreeNode, {
                    left: plainLabel(
                      this.processLink(
                        `${elidePathHead(c.name)} [${c.pid}]`,
                        c.sliceId,
                        c.name,
                      ),
                    ),
                    right: formatDuration(c.lifetimeNs),
                  }),
                ),
                ...(this.childCount === 0
                  ? []
                  : [
                      m(TreeNode, {
                        left: m(Button, {
                          label:
                            this.childCount > this.children.length
                              ? `Show all ${this.childCount} children\u2026`
                              : "Open as a table\u2026",
                          compact: true,
                          onclick: () => this.openChildrenTab(),
                        }),
                      }),
                    ]),
              ),
            ),
          ),
          m(
            Section,
            { title: "Process" },
            m(
              Tree,
              m(TreeNode, {
                left: "Lifetime",
                right: proc ? formatDuration(proc.lifetimeNs) : blank,
              }),
              m(TreeNode, {
                left: "Exit status",
                right: !proc
                  ? blank
                  : proc.exitCode === null
                    ? "unknown"
                    : `${proc.exitCode}`,
              }),
              m(TreeNode, { left: "PID", right: `${this.pid}` }),
              m(TreeNode, {
                left: "Parent PID",
                right: proc ? `${proc.ppid}` : blank,
              }),
              m(TreeNode, {
                left: "Working directory",
                right: last?.cwd ?? blank,
              }),
              ...(proc === undefined || execExplanation === undefined
                ? []
                : [
                    m(TreeNode, {
                      left: m(
                        "span",
                        {
                          style: {
                            display: "inline-flex",
                            alignItems: "center",
                            gap: "2px",
                          },
                        },
                        "Programs run",
                        m(
                          Tooltip,
                          {
                            trigger: m(Button, {
                              icon: "help_outline",
                              compact: true,
                            }),
                          },
                          m(
                            "div",
                            { style: { maxWidth: "360px" } },
                            m("p", EXEC_HELP),
                            m("p", execExplanation),
                          ),
                        ),
                      ),
                      right: `${proc.segments.length}`,
                    }),
                  ]),
              ...(proc?.segments ?? []).map((seg, i) =>
                m(
                  TreeNode,
                  {
                    left:
                      (proc?.segments.length ?? 1) === 1
                        ? "Program"
                        : i === (proc?.segments.length ?? 1) - 1
                          ? `Program ${i + 1} \u2014 ${basename(executableOf(seg.cmd))} (final)`
                          : `Program ${i + 1} \u2014 ${basename(executableOf(seg.cmd))} (replaced itself)`,
                    right: formatDuration(seg.durNs),
                  },
                  m(TreeNode, {
                    left: "Executable",
                    right: executableOf(seg.cmd),
                  }),
                  m(TreeNode, {
                    left: "Command",
                    right: commandLines(seg.cmd),
                  }),
                  m(TreeNode, { left: "Working directory", right: seg.cwd }),
                ),
              ),
            ),
          ),
        ),
        m(
          GridLayoutColumn,
          m(
            Section,
            {
              title: m(
                "div",
                {
                  style: {
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                  },
                },
                m("h1", "Dependencies"),
                m(Switch, {
                  label: "Show on timeline",
                  checked: this.dependencyArrows.isEnabled(),
                  onchange: (event: Event) => {
                    const input = event.target as HTMLInputElement;
                    this.dependencyArrows.setEnabled(input.checked);
                  },
                }),
              ),
            },
            m(
              Tree,
              this.renderDeps(
                "Produced by",
                this.producers,
                this.producerCount,
                () => this.openDepsTab("producers"),
              ),
              this.renderDeps(
                "Consumed by",
                this.consumers,
                this.consumerCount,
                () => this.openDepsTab("consumers"),
              ),
            ),
          ),
          m(
            Section,
            {
              title: "Files",
            },
            this.renderFiles(),
          ),
        ),
      ),
    );
  }
}
