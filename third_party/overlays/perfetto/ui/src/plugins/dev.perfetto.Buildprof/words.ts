// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import m from "mithril";
import WORDS_JSON from "../../../../src/trace_processor/perfetto_sql/stdlib/buildprof/words.json?raw";

/**
 * The words for what the buildprof SQL package names by key, shared with
 * every other front end so they all say the same thing.
 */
const WORDS: unknown = JSON.parse(WORDS_JSON);

/** The template at a dotted `path`, such as `insight.ran_alone.title`. */
export function template(path: string): string | undefined {
  let node: unknown = WORDS;
  for (const key of path.split(".")) {
    if (typeof node !== "object" || node === null) return undefined;
    node = (node as Record<string, unknown>)[key];
  }
  return typeof node === "string" ? node : undefined;
}

/**
 * A template with its `{variables}` filled in. A template that does not exist
 * comes back as its path, so a missing word shows rather than hides.
 */
export function say(
  path: string,
  vars: Record<string, string | number> = {},
): string {
  const found = template(path);
  if (found === undefined) return path;
  return found.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in vars ? String(vars[name]) : whole,
  );
}

/**
 * Words with their code: a fenced block (three backticks on lines of their
 * own) as a code block, and backtick spans as inline code.
 */
export function withCode(text: string): m.Children {
  return text.split(/\n?```\n?/).map((part, i) =>
    i % 2 === 1
      ? m("pre", { style: { margin: "4px 0" } }, m("code", part))
      : part
          .split("`")
          .map((span, j) => (j % 2 === 1 ? m("code", span) : span)),
  );
}
