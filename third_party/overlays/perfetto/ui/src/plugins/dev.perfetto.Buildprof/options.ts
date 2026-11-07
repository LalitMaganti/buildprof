// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import m from "mithril";
import type { Trace } from "../../public/trace";
import { LONG, NUM, STR } from "../../trace_processor/query_result";
import { Section } from "../../widgets/section";
import { Tree, TreeNode } from "../../widgets/tree";
import { say, template } from "./words";

export interface ProcessOptions {
  readonly tool: string;
  readonly options: ReadonlyArray<{
    readonly key: string;
    readonly value: string;
    readonly isDefault: boolean;
  }>;
  readonly parallelism?: {
    readonly average: number;
    readonly peak: number;
  };
}

/** Tools whose children are their work, so measured parallelism means something. */
const DRIVERS = new Set(["cargo", "make", "ninja", "ctest", "pytest"]);

/** The settings the SQL package reads from a process's command line. */
export async function loadProcessOptions(
  trace: Trace,
  pid: number,
): Promise<ProcessOptions | undefined> {
  await trace.engine.query(`INCLUDE PERFETTO MODULE buildprof.options`);
  const [rows, parallelism] = await Promise.all([
    trace.engine.query(`
      select tool, key, value, is_default
      from buildprof_process_options(${pid})
      order by ord
    `),
    trace.engine.query(`
      select busy, covered, peak
      from buildprof_child_parallelism
      where pid = ${pid}
    `),
  ]);
  let tool: string | undefined;
  const options: Array<ProcessOptions["options"][number]> = [];
  const it = rows.iter({ tool: STR, key: STR, value: STR, is_default: NUM });
  for (; it.valid(); it.next()) {
    tool = it.tool;
    options.push({
      key: it.key,
      value: it.value,
      isDefault: it.is_default !== 0,
    });
  }
  if (tool === undefined) return undefined;
  let measured: ProcessOptions["parallelism"];
  if (DRIVERS.has(tool) && parallelism.numRows() > 0) {
    const row = parallelism.firstRow({ busy: LONG, covered: LONG, peak: NUM });
    if (row.covered > 0n) {
      measured = {
        average: Number(row.busy) / Number(row.covered),
        peak: row.peak,
      };
    }
  }
  return { tool, options, parallelism: measured };
}

function optionName(tool: string, key: string): string {
  const specific = template(`options.tool_name.${tool}.${key}`);
  return specific ?? say(`options.name.${key}`);
}

function optionValue(key: string, value: string, isDefault: boolean): string {
  const word = template(`options.value.${value}`) ?? value;
  const unit = template(`options.unit.${key}`);
  const shown = unit === undefined ? word : say(`options.unit.${key}`, { value: word });
  return isDefault ? say("options.default", { value: shown }) : shown;
}

/**
 * Lets the caller decorate an option's value, such as to highlight one; it
 * gets the setting's key and its value as shown, and returns undefined to
 * leave the value as it is.
 */
export type OptionDecorator = (
  key: string,
  shown: string,
) => m.Children | undefined;

/** The section naming the settings that decide how fast a tool ran. */
export function renderProcessOptions(
  view: ProcessOptions,
  decorate: OptionDecorator = () => undefined,
): m.Children {
  const toolWord = template(`options.tool.${view.tool}`) ?? view.tool;
  return m(
    Section,
    { title: say("options.title", { tool: toolWord }) },
    m(Tree, [
      ...view.options.map((option) => {
        const shown = optionValue(option.key, option.value, option.isDefault);
        return m(TreeNode, {
          left: optionName(view.tool, option.key),
          right: decorate(option.key, shown) ?? shown,
        });
      }),
      view.parallelism !== undefined &&
        m(TreeNode, {
          left: say("options.measured_title"),
          right: say("options.measured", {
            average: view.parallelism.average.toFixed(1),
            peak: view.parallelism.peak,
          }),
        }),
    ]),
  );
}
