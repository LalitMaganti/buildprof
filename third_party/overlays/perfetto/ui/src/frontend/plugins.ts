// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import type {PerfettoPlugin, PerfettoPluginStatic} from '../public/plugin';
import BuildprofPlugin from '../plugins/dev.perfetto.Buildprof';
import QueryPagePlugin from '../plugins/dev.perfetto.QueryPage';
import SqlModulesPlugin from '../plugins/dev.perfetto.SqlModules';
import TimelinePlugin from '../core_plugins/dev.perfetto.Timeline';
import CoreCommandsPlugin from '../core_plugins/dev.perfetto.CoreCommands';
import SearchUtilsPlugin from '../core_plugins/dev.perfetto.SearchUtils';
import NotesPlugin from '../core_plugins/dev.perfetto.Notes';

// Only plugins in this explicit list are registered and bundled.
// QueryPage depends on CoreCommands and SqlModules.
export const plugins: PerfettoPluginStatic<PerfettoPlugin>[] = [
  BuildprofPlugin,
  QueryPagePlugin,
  SqlModulesPlugin,
];

// CoreCommands, SearchUtils, and Notes support the standard navigation UI.
export const corePlugins: PerfettoPluginStatic<PerfettoPlugin>[] = [
  TimelinePlugin,
  CoreCommandsPlugin,
  SearchUtilsPlugin,
  NotesPlugin,
];
