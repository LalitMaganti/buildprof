// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import m from "mithril";
import type { Trace } from "../../public/trace";
import type { Overlay, TrackBounds } from "../../public/track";
import type { TimeScale } from "../../base/time_scale";
import { Time } from "../../base/time";
import {
  LONG,
  NUM,
  NUM_NULL,
  STR,
  STR_NULL,
} from "../../trace_processor/query_result";
import { Anchor } from "../../widgets/anchor";
import { Callout } from "../../widgets/callout";
import { Button } from "../../widgets/button";
import { Card } from "../../widgets/card";
import { Popup } from "../../widgets/popup";
import { Chip } from "../../widgets/chip";
import { Stack } from "../../widgets/stack";
import { GridLayout, GridLayoutColumn } from "../../widgets/grid_layout";
import { Intent } from "../../widgets/common";
import { Section } from "../../widgets/section";
import { Tree, TreeNode } from "../../widgets/tree";
import type { OptionDecorator } from "./options";
import { say, template, withCode } from "./words";

/** Wall time the way insights state it: 3m10s, 42 s, 850 ms. */
export function formatWallTime(ns: bigint): string {
  const n = Number(ns);
  if (n >= 60e9) {
    const minutes = Math.floor(n / 60e9);
    const seconds = Math.floor((n % 60e9) / 1e9);
    return `${minutes}m${String(seconds).padStart(2, "0")}s`;
  }
  // Non-breaking, so a time never wraps between its number and its unit.
  if (n >= 10e9) return `${Math.floor(n / 1e9)}\u00a0s`;
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}\u00a0s`;
  return `${Math.floor(n / 1e6)}\u00a0ms`;
}

function formatShare(share: number): string {
  return `${Math.round(share * 100)}%`;
}

export interface Insight {
  readonly id: number;
  readonly rule: string;
  readonly cause: string | null;
  readonly pid: number;
  readonly dur: bigint;
  readonly share: number;
  readonly count: number | null;
  readonly cmd: string;
  readonly optionKey: string | null;
  /** 'cargo_fuzz' when cargo-fuzz started this part of the build. */
  readonly via: string | null;
  /** The Cargo profile of the nearest cargo above the process. */
  readonly profile: string | null;
  /** The process as the timeline labels it. */
  readonly label: string;
  /** Where the process sits on the process tree track. */
  readonly sliceId: number;
  readonly ts: bigint;
  readonly sliceDur: bigint;
  readonly depth: number;
}

/** Only the costliest insights get an icon on the timeline. */
const ICON_LIMIT = 5;

/** A repeated command line, shortened for a title. */
function shortCommand(cmd: string): string {
  const args = cmd.split(" ").filter((arg) => arg.length > 0);
  const [exe, ...rest] = args;
  if (exe === undefined) return cmd;
  const short = [exe.slice(exe.lastIndexOf("/") + 1), ...rest].join(" ");
  return short.length > 60 ? `${short.slice(0, 59)}…` : short;
}

function executable(cmd: string): string {
  const space = cmd.indexOf(" ");
  const exe = space === -1 ? cmd : cmd.slice(0, space);
  return exe.slice(exe.lastIndexOf("/") + 1);
}

/**
 * The process as an insight names it. The timeline names a process by its
 * first program, but `env` or a shell often execs the real tool in place, and
 * the insight is about the tool.
 */
function processName(insight: Insight): string {
  const tool = executable(insight.cmd);
  if (insight.label.split(" ")[0] === tool) return insight.label;
  const subject = insight.cmd
    .split(" ")
    .slice(1)
    .find((arg) => arg.length > 0 && !arg.startsWith("-"));
  return subject === undefined ? tool : `${tool} ${subject}`;
}

export function insightTitle(insight: Insight): string {
  return say(`insight.${insight.rule}.title`, {
    process: insight.rule === "repeated_command" ? insight.label : processName(insight),
    command: shortCommand(insight.cmd),
    tool: executable(insight.cmd),
    count: insight.count ?? 0,
    dur: formatWallTime(insight.dur),
  });
}

/** Flags that set how many jobs a tool runs, which a suggestion replaces. */
const JOB_FLAGS = /^(?:-j\d*|--jobs(?:=\d+)?|--parallel(?:=\d+)?)$/;

/**
 * The process's own command, run with as many jobs as there are CPUs: what
 * the reader could type instead.
 */
function parallelCommand(insight: Insight): string {
  const [exe, ...args] = insight.cmd.split(" ").filter((a) => a.length > 0);
  if (exe === undefined) return insight.cmd;
  const tool = exe.slice(exe.lastIndexOf("/") + 1);
  const kept: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i] ?? "";
    if (JOB_FLAGS.test(arg)) {
      // A separate count belongs to the flag, as in `-j 1` or `--parallel 1`.
      if (!/\d/.test(arg) && /^\d+$/.test(args[i + 1] ?? "")) i++;
      continue;
    }
    kept.push(arg);
  }
  const jobs =
    tool === "ctest"
      ? ["--parallel", "$(nproc)"]
      : tool === "cargo" || tool === "ninja"
        ? []
        : ["-j$(nproc)"];
  return [tool, ...jobs, ...kept].join(" ");
}

/**
 * What happened, or what the reader could try: `part` is "what" or "try".
 * The most specific wording wins: one for how the build was started (such as
 * by cargo-fuzz), then one for the tool, then the cause's own.
 */
export function insightText(insight: Insight, part: "what" | "try"): string {
  const cause = insight.cause ?? "none";
  const tool = executable(insight.cmd);
  const base = `insight.${insight.rule}.${part}`;
  const path =
    [
      insight.via === null ? undefined : `${base}.${cause}_${insight.via}`,
      `${base}.${cause}_${tool}`,
    ].find((candidate) => candidate !== undefined && template(candidate) !== undefined) ??
    `${base}.${cause}`;
  return say(path, {
    tool,
    count: insight.count ?? 0,
    dur: formatWallTime(insight.dur),
    process: insight.label,
    profile: insight.profile ?? "dev",
    command: parallelCommand(insight),
  });
}

/**
 * Everything the SQL package found, joined to where each process sits on
 * the timeline.
 */
export async function loadInsights(
  trace: Trace,
  tables: { readonly life: string; readonly tree: string },
): Promise<Insight[]> {
  await trace.engine.query(`INCLUDE PERFETTO MODULE buildprof.insights`);
  const result = await trace.engine.query(`
    select
      i.id, i.rule, i.cause, i.pid, i.dur, i.share, i.count, i.cmd,
      i.option_key, i.via, i.profile,
      l.name as label, l.slice_id, t.ts, t.dur as slice_dur, t.depth
    from buildprof_insight i
    join ${tables.life} l on l.pid = i.pid
    join ${tables.tree} t on t.id = l.slice_id
    order by i.id
  `);
  const out: Insight[] = [];
  const it = result.iter({
    id: NUM,
    rule: STR,
    cause: STR_NULL,
    pid: NUM,
    dur: LONG,
    share: NUM,
    count: NUM_NULL,
    cmd: STR,
    option_key: STR_NULL,
    via: STR_NULL,
    profile: STR_NULL,
    label: STR,
    slice_id: NUM,
    ts: LONG,
    slice_dur: LONG,
    depth: NUM,
  });
  for (; it.valid(); it.next()) {
    out.push({
      id: it.id,
      rule: it.rule,
      cause: it.cause,
      pid: it.pid,
      dur: it.dur,
      share: it.share,
      count: it.count,
      cmd: it.cmd,
      optionKey: it.option_key,
      via: it.via,
      profile: it.profile,
      label: it.label,
      sliceId: it.slice_id,
      ts: it.ts,
      sliceDur: it.slice_dur,
      depth: it.depth,
    });
  }
  return out;
}

/**
 * The insights about one process, above its details. Each is one section in
 * two panes, as Trace Doctor pairs a problem with its fix: what happened on
 * the left, in the panel's usual label and value form, and what the reader
 * could try on the right.
 */
export function renderInsightSections(
  insights: ReadonlyArray<Insight>,
): m.Children {
  if (insights.length === 0) return undefined;
  return m(
    GridLayout,
    m(
      GridLayoutColumn,
      insights.map((insight) =>
        m(
          Section,
          { title: insightTitle(insight).replaceAll("`", "") },
          m(GridLayout, [
            m(
              GridLayoutColumn,
              m(Tree, [
                m(TreeNode, {
                  left: say("insight.kind_label"),
                  right: say(`insight.${insight.rule}.label_plain`),
                }),
                m(TreeNode, {
                  left: say("insight.time_label"),
                  right: say("insight.time", {
                    dur: formatWallTime(insight.dur),
                    share: formatShare(insight.share),
                  }),
                }),
                m(TreeNode, {
                  left: say("insight.what_title"),
                  right: withCode(insightText(insight, "what")),
                }),
              ]),
            ),
            m(
              GridLayoutColumn,
              m(
                Callout,
                { icon: "lightbulb", intent: Intent.Primary },
                m(Stack, { spacing: "small" }, [
                  m("strong", say("insight.try_title")),
                  m("span", withCode(insightText(insight, "try"))),
                  insight.optionKey !== null &&
                    m(
                      "span",
                      say("insight.blamed_option", {
                        option: say(`options.name.${insight.optionKey}`),
                      }),
                    ),
                  m(
                    Anchor,
                    {
                      href: learnMoreUrl(insight),
                      target: "_blank",
                      icon: "open_in_new",
                    },
                    say("insight.learn_more"),
                  ),
                ]),
              ),
            ),
          ]),
        ),
      ),
    ),
  );
}

/**
 * Where an insight is explained in full. The site redirects these to the docs,
 * so links in deployed UIs keep working when headings change.
 */
export function learnMoreUrl(insight: Insight): string {
  const slug = (key: string) => key.replaceAll("_", "-");
  return `https://buildprof.lalitm.com/insights/${slug(insight.rule)}/${slug(insight.cause ?? "none")}`;
}

const INSIGHTS_TAB_URI = "buildprof.insights";

/** How strongly to colour an insight: by how much of the build it took. */
function shareColor(share: number): string {
  if (share < 0.15) return "hsl(48, 90%, 50%)";
  if (share < 0.35) return "hsl(28, 90%, 52%)";
  return "hsl(0, 75%, 50%)";
}

/**
 * A tab of cards, one per insight, opened when the trace loads, as Trace
 * Doctor's drawer does. Clicking one selects its process, whose details say
 * what happened and what to try.
 */
export function registerInsightsTab(
  trace: Trace,
  insights: ReadonlyArray<Insight>,
  select: (insight: Insight) => void,
): void {
  if (insights.length === 0) return;
  trace.tabs.registerTab({
    uri: INSIGHTS_TAB_URI,
    closable: false,
    content: {
      getTitle: () => say("insight.tab", { count: insights.length }),
      render: () =>
        m(".bt-insights", [
          m(
            ".bt-insights__headline",
            insights.length === 1
              ? say("insight.headline.one")
              : say("insight.headline.many", { count: insights.length }),
          ),
          m(".bt-insights__hint", say("insight.hint")),
          m(
            ".bt-insights__tiles",
            insights.map((insight) =>
              m(
                Card,
                {
                  className: "bt-insights__tile",
                  interactive: true,
                  onclick: () => select(insight),
                  style: { borderTopColor: shareColor(insight.share) },
                },
                m(".bt-insights__head", [
                  m(
                    ".bt-insights__label",
                    say(`insight.${insight.rule}.label`),
                  ),
                  m(".bt-insights__cost", [
                    formatWallTime(insight.dur),
                    m(
                      ".bt-insights__bar",
                      {
                        title: say("insight.share", {
                          share: formatShare(insight.share),
                        }),
                      },
                      m(".bt-insights__bar-fill", {
                        style: {
                          width: formatShare(insight.share),
                          backgroundColor: shareColor(insight.share),
                        },
                      }),
                    ),
                  ]),
                ]),
                m(".bt-insights__title", withCode(insightTitle(insight))),
              ),
            ),
          ),
        ]),
    },
  });
  trace.tabs.addDefaultTab(INSIGHTS_TAB_URI);
  trace.tabs.showTab(INSIGHTS_TAB_URI);
}

/**
 * Highlights the settings these insights blame where the process's options
 * list them, with a small popup saying why.
 */
export function blamedOptions(
  insights: ReadonlyArray<Insight>,
): OptionDecorator {
  const blamed = new Map(
    insights.flatMap((insight) =>
      insight.optionKey === null ? [] : [[insight.optionKey, insight] as const],
    ),
  );
  return (key, value) => {
    const insight = blamed.get(key);
    if (insight === undefined) return undefined;
    return highlightedOption(value, insight);
  };
}

function highlightedOption(value: string, insight: Insight): m.Children {
  return m(Stack, { orientation: "horizontal", spacing: "small", inline: true }, [
    m(Chip, { label: value, intent: Intent.Warning, compact: true }),
    m(
      Popup,
      {
        trigger: m(Button, {
          icon: "help_outline",
          compact: true,
          title: say("options.why"),
        }),
      },
      m(Stack, { spacing: "small" }, [
        m("span", withCode(insightTitle(insight))),
        m(
          Anchor,
          { href: learnMoreUrl(insight), target: "_blank", icon: "open_in_new" },
          say("insight.learn_more"),
        ),
      ]),
    ),
  ]);
}

const ICON_SIZE = 14;
const ICON_COLOR = "hsl(38, 95%, 48%)";

/**
 * Warning icons at the right end of the costliest insights' slices.
 *
 * The icon keeps its size at any zoom, inside the slice when it fits and just
 * past its end when it does not.
 */
export class InsightIcons implements Overlay {
  private readonly marked: ReadonlyArray<Insight>;

  constructor(
    private readonly trace: Trace,
    private readonly trackUri: string,
    insights: ReadonlyArray<Insight>,
  ) {
    this.marked = insights.slice(0, ICON_LIMIT);
  }

  render(
    ctx: CanvasRenderingContext2D,
    timescale: TimeScale,
    _size: unknown,
    tracks: ReadonlyArray<TrackBounds>,
  ): void {
    if (this.marked.length === 0) return;
    const bounds = tracks.find((t) => t.node.uri === this.trackUri);
    if (bounds === undefined) return;
    const renderer = this.trace.tracks.getTrack(this.trackUri)?.renderer;

    const placed: Array<{ x: number; y: number }> = [];
    for (const insight of this.marked) {
      const row = renderer?.getSliceVerticalBounds?.(insight.depth);
      if (row === undefined) continue;
      const top = bounds.verticalBounds.top + row.top;
      const y = top + (row.bottom - row.top) / 2;
      const start = timescale.timeToPx(Time.fromRaw(insight.ts));
      const end = timescale.timeToPx(
        Time.fromRaw(insight.ts + insight.sliceDur),
      );
      const fits = end - start >= ICON_SIZE + 6;
      const x = fits ? end - ICON_SIZE / 2 - 3 : end + ICON_SIZE / 2 + 2;
      // Icons that would overlap draw once; the slice's details list them all.
      const near = placed.some(
        (p) => p.y === y && Math.abs(p.x - x) < ICON_SIZE,
      );
      if (!near) placed.push({ x, y });
    }

    ctx.save();
    ctx.font = `bold ${ICON_SIZE - 3}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    for (const { x, y } of placed) {
      const half = ICON_SIZE / 2;
      ctx.beginPath();
      ctx.moveTo(x, y - half);
      ctx.lineTo(x + half, y + half - 1);
      ctx.lineTo(x - half, y + half - 1);
      ctx.closePath();
      ctx.fillStyle = ICON_COLOR;
      ctx.fill();
      ctx.strokeStyle = "rgba(0, 0, 0, 0.45)";
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.fillStyle = "#1d1d1d";
      ctx.fillText("!", x, y + 1.5);
    }
    ctx.restore();
  }
}
