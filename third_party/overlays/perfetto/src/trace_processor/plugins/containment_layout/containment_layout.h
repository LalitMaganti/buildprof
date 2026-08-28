/*
 * Copyright (C) 2026 The Buildprof Authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#ifndef SRC_TRACE_PROCESSOR_PLUGINS_CONTAINMENT_LAYOUT_CONTAINMENT_LAYOUT_H_
#define SRC_TRACE_PROCESSOR_PLUGINS_CONTAINMENT_LAYOUT_CONTAINMENT_LAYOUT_H_

#include <cstdint>
#include <vector>

#include "src/trace_processor/perfetto_sql/engine/perfetto_sql_connection.h"
#include "src/trace_processor/sqlite/bindings/sqlite_module.h"
#include "src/trace_processor/sqlite/module_state_manager.h"

namespace perfetto::trace_processor::containment_layout {

// Lays out an arbitrary forest of time intervals so that containment is
// preserved and vertical space is minimised.
//
// ```
//   select id, layout_depth, depth
//   from __intrinsic_containment_layout((
//     select id, parent_id, ts, dur from my_actions
//   ))
// ```
//
// Missing parents define roots. Subtrees are packed tallest-first into the
// lowest rows that remain free across their time range. `depth` is relative to
// the root, `layout_depth` is the rendered row, and `subtree_height` is the
// number of rows occupied by the subtree.
struct ContainmentLayout : sqlite::Module<ContainmentLayout> {
  struct Result {
    int64_t id;
    uint32_t depth;
    uint32_t layout_depth;
    // Rows spanned by this row's whole subtree, itself included. 1 for a leaf.
    // Lets a renderer draw a box enclosing everything a node contains.
    uint32_t subtree_height;
  };
  struct State {
    std::vector<Result> results;
  };
  struct Context : sqlite::ModuleStateManager<ContainmentLayout> {
    explicit Context(PerfettoSqlConnection* _connection)
        : sqlite::ModuleStateManager<ContainmentLayout>(owned_committed_store_),
          connection(_connection) {}
    PerfettoSqlConnection* connection;

   private:
    sqlite::CommittedStateManager owned_committed_store_;
  };
  struct Vtab : sqlite::Module<ContainmentLayout>::Vtab {
    sqlite::ModuleStateManager<ContainmentLayout>::PerVtabState* state;
  };
  struct Cursor : sqlite::Module<ContainmentLayout>::Cursor {
    const std::vector<Result>* results = nullptr;
    uint32_t index = 0;
  };

  // Inputs are parallel arrays; duration -1 extends to INT64_MAX.
  static std::vector<Result> ComputeLayout(
      const std::vector<int64_t>& ids,
      const std::vector<int64_t>& parent_ids,
      const std::vector<bool>& has_parent,
      const std::vector<int64_t>& ts,
      const std::vector<int64_t>& dur);

  static constexpr auto kType = kCreateOnly;
  static constexpr bool kSupportsWrites = false;
  static constexpr bool kDoesOverloadFunctions = false;

  static int Create(sqlite3*,
                    void*,
                    int,
                    const char* const*,
                    sqlite3_vtab**,
                    char**);
  static int Destroy(sqlite3_vtab*);

  static int Connect(sqlite3*,
                     void*,
                     int,
                     const char* const*,
                     sqlite3_vtab**,
                     char**);
  static int Disconnect(sqlite3_vtab*);

  static int BestIndex(sqlite3_vtab*, sqlite3_index_info*);

  static int Open(sqlite3_vtab*, sqlite3_vtab_cursor**);
  static int Close(sqlite3_vtab_cursor*);

  static int Filter(sqlite3_vtab_cursor*,
                    int,
                    const char*,
                    int,
                    sqlite3_value**);
  static int Next(sqlite3_vtab_cursor*);
  static int Eof(sqlite3_vtab_cursor*);
  static int Column(sqlite3_vtab_cursor*, sqlite3_context*, int);
  static int Rowid(sqlite3_vtab_cursor*, sqlite_int64*);

  static int Begin(sqlite3_vtab*) { return SQLITE_OK; }
  static int Sync(sqlite3_vtab*) { return SQLITE_OK; }
  static int Commit(sqlite3_vtab*) { return SQLITE_OK; }
  static int Rollback(sqlite3_vtab*) { return SQLITE_OK; }
  static int Savepoint(sqlite3_vtab* t, int r) {
    ContainmentLayout::Vtab* vtab = GetVtab(t);
    sqlite::ModuleStateManager<ContainmentLayout>::OnSavepoint(vtab->state, r);
    return SQLITE_OK;
  }
  static int Release(sqlite3_vtab* t, int r) {
    ContainmentLayout::Vtab* vtab = GetVtab(t);
    sqlite::ModuleStateManager<ContainmentLayout>::OnRelease(vtab->state, r);
    return SQLITE_OK;
  }
  static int RollbackTo(sqlite3_vtab* t, int r) {
    ContainmentLayout::Vtab* vtab = GetVtab(t);
    sqlite::ModuleStateManager<ContainmentLayout>::OnRollbackTo(vtab->state, r);
    return SQLITE_OK;
  }

  // Depends on the functions above.
  static constexpr sqlite3_module kModule = CreateModule();
};

// Registers the ContainmentLayout plugin with the global plugin set.
// Idempotent; only the first call has an effect. Must run before the first
// GetPluginSet() call (i.e. before constructing TraceProcessorImpl).
void RegisterPlugin();

}  // namespace perfetto::trace_processor::containment_layout

#endif  // SRC_TRACE_PROCESSOR_PLUGINS_CONTAINMENT_LAYOUT_CONTAINMENT_LAYOUT_H_
