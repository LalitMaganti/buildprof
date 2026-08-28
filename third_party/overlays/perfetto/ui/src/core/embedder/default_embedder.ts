// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import type {Embedder} from './embedder';
import {BUILDPROF_HERO} from './buildprof_brand';
import {BuildprofHomePage} from './buildprof_home_page';
import {showBuildprofHelp} from './buildprof_help';

/** Buildprof's self-hosted Perfetto deployment. */
export class DefaultEmbedder implements Embedder {
  readonly appTitle = 'Buildprof';
  readonly showHelp = showBuildprofHelp;
  readonly analyticsId = undefined;
  readonly extensionServer = undefined;
  readonly brandingBadge = undefined;
  // Enabled plugins provide commands, search, notes, SQL, and Buildprof UI.
  readonly defaultPlugins: ReadonlyArray<string> = [
    'dev.perfetto.Timeline',
    'dev.perfetto.Buildprof',
    'dev.perfetto.CoreCommands',
    'dev.perfetto.SearchUtils',
    'dev.perfetto.Notes',
    'dev.perfetto.QueryPage',
    'dev.perfetto.SqlModules',
  ];
  readonly homePage = BuildprofHomePage;
  readonly brandLogo = {src: BUILDPROF_HERO, alt: 'buildprof'};
  readonly navigationMode = 'topbar' as const;
  readonly documentationUrl = 'https://github.com/lalitmaganti/buildprof#readme';
  readonly reportBugUrl = 'https://github.com/lalitmaganti/buildprof/issues/new';
  readonly showConvertToJson = false;
  readonly showMetatrace = false;
}
