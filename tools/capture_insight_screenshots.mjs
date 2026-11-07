#!/usr/bin/env node

// Captures the screenshots in docs/insights.md: the Insights tab, and each
// kind of insight as the process details show it. Serve a built UI (for
// example `python3 -m http.server` in its dist directory) and point
// BUILDPROF_UI_URL at it; the recordings come from BUILDPROF_INSIGHT_TRACES.

import {mkdir} from "node:fs/promises";
import path from "node:path";
import {chromium} from "../third_party/src/perfetto/ui/node_modules/.pnpm/playwright@1.58.2/node_modules/playwright/index.mjs";

const root = path.resolve(import.meta.dirname, "..");
const output = path.join(root, "docs", "assets", "insights");
const ui = process.env.BUILDPROF_UI_URL ?? "http://127.0.0.1:10077/";
const traces =
  process.env.BUILDPROF_INSIGHT_TRACES ?? path.join(root, "target", "uitraces");

// One recording per kind of insight, named as docs/insights.md names its
// sections, and which card in the Insights tab shows it.
const shots = [
  ["ran-alone-codegen-units", "ruff-fuzz-build", 0],
  ["ran-alone-unoptimized-build-script", "monero-build", 0],
  ["ran-alone-none", "helix-noop-main", 0],
  ["serial-driver-serial-by-default", "git-make-sparse", 0],
  ["serial-driver-one-job", "cargo-j1", 0],
  ["serial-driver-none", "node-noop-main", 0],
  ["repeated-command-none", "ruff-tests", 0],
];

async function open(browser, recording) {
  const page = await browser.newPage({
    viewport: {width: 1600, height: 1000},
    deviceScaleFactor: 2,
  });
  await page.goto(ui, {waitUntil: "networkidle"});
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", {name: "Open a recording"}).click();
  await (await chooser).setFiles(path.join(traces, `${recording}.buildprof`));
  await page.locator(".bt-insights__tile").first().waitFor({timeout: 120_000});
  await page.waitForTimeout(1_000);
  return page;
}

await mkdir(output, {recursive: true});
const browser = await chromium.launch({headless: true});

try {
  // The whole window with the Insights tab open, for the top of the page.
  const overview = await open(browser, "helix-noop-main");
  await overview.screenshot({path: path.join(output, "overview.png")});
  await overview.close();

  for (const [name, recording, card] of shots) {
    const page = await open(browser, recording);
    await page.locator(".bt-insights__tile").nth(card).click();
    const section = page
      .locator(".pf-section", {has: page.getByText("What we saw")})
      .first();
    await section.waitFor({timeout: 30_000});
    await page.waitForTimeout(500);
    await section.screenshot({path: path.join(output, `${name}.png`)});
    console.log(`==> ${name}.png from ${recording}`);
    if (name === "ran-alone-codegen-units") {
      // The setting the insight blames, highlighted where the options list it.
      const options = page
        .locator(".pf-section", {has: page.getByText("How rustc was run")})
        .first();
      await options.scrollIntoViewIfNeeded();
      await options.screenshot({path: path.join(output, "highlighted-option.png")});
      console.log("==> highlighted-option.png");
    }
    await page.close();
  }
} finally {
  await browser.close();
}
