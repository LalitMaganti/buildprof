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

#include "src/trace_processor/plugins/containment_layout/containment_layout.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "perfetto/base/logging.h"
#include "perfetto/ext/base/flat_hash_map.h"
#include "perfetto/ext/base/utils.h"
#include "src/trace_processor/core/plugin/plugin.h"
#include "src/trace_processor/perfetto_sql/engine/perfetto_sql_connection.h"
#include "src/trace_processor/sqlite/bindings/sqlite_result.h"
#include "src/trace_processor/sqlite/module_state_manager.h"
#include "src/trace_processor/sqlite/sql_source.h"
#include "src/trace_processor/sqlite/sqlite_utils.h"

namespace perfetto::trace_processor::containment_layout {
namespace {

constexpr char kSchema[] = R"(
  CREATE TABLE x(
    id BIGINT,
    layout_depth INTEGER,
    depth INTEGER,
    subtree_height INTEGER,
    PRIMARY KEY(id)
  ) WITHOUT ROWID
)";

enum ColumnIndex : size_t {
  kId = 0,
  kLayoutDepth,
  kDepth,
  kSubtreeHeight,
};

// A single layout row: the [start, end) ranges occupied on it, sorted by start
// and disjoint. Being disjoint their ends increase too, so the last range's
// end is the row's high-water mark.
using Row = std::vector<std::pair<int64_t, int64_t>>;

Row::const_iterator LowerBound(const Row& row, int64_t ts) {
  return std::lower_bound(row.begin(), row.end(), ts,
                          [](const std::pair<int64_t, int64_t>& r, int64_t t) {
                            return r.first < t;
                          });
}

bool RowIsFree(const Row& row, int64_t start, int64_t end) {
  if (row.empty() || start >= row.back().second) {
    return true;
  }
  auto it = LowerBound(row, start);
  if (it != row.end() && it->first < end) {
    return false;
  }
  if (it != row.begin() && std::prev(it)->second > start) {
    return false;
  }
  return true;
}

// Record [start, end) as occupied. Placements almost always arrive in time
// order, so this is an append; the rare out-of-order case shifts.
void RowMark(Row& row, int64_t start, int64_t end) {
  if (row.empty() || start >= row.back().first) {
    row.emplace_back(start, end);
  } else {
    row.insert(LowerBound(row, start), {start, end});
  }
}

}  // namespace

std::vector<ContainmentLayout::Result> ContainmentLayout::ComputeLayout(
    const std::vector<int64_t>& ids,
    const std::vector<int64_t>& parent_ids,
    const std::vector<bool>& has_parent,
    const std::vector<int64_t>& ts,
    const std::vector<int64_t>& dur) {
  size_t n = ids.size();
  if (n == 0) {
    return {};
  }

  // Index unordered input so parents can be resolved by id.
  base::FlatHashMap<int64_t, uint32_t> id_to_pos;
  for (size_t i = 0; i < n; ++i) {
    id_to_pos.Insert(ids[i], static_cast<uint32_t>(i));
  }

  // A row is a root if it has no parent, or names a parent outside the input.
  // Resolve each row's parent position once; kNoParent marks a root.
  constexpr uint32_t kNoParent = std::numeric_limits<uint32_t>::max();
  std::vector<uint32_t> parent_pos(n, kNoParent);
  for (size_t i = 0; i < n; ++i) {
    if (!has_parent[i]) {
      continue;
    }
    uint32_t* found = id_to_pos.Find(parent_ids[i]);
    if (found && *found != i) {
      parent_pos[i] = *found;
    }
  }

  // Resolve depth and root iteratively, breaking malformed parent cycles.
  constexpr uint32_t kUnresolved = std::numeric_limits<uint32_t>::max();
  std::vector<uint32_t> depth(n, kUnresolved);
  std::vector<uint32_t> root(n, kUnresolved);
  std::vector<uint32_t> stack;
  std::vector<bool> on_stack(n, false);
  for (size_t i = 0; i < n; ++i) {
    if (depth[i] != kUnresolved) {
      continue;
    }
    stack.clear();
    uint32_t cur = static_cast<uint32_t>(i);
    while (cur != kNoParent && depth[cur] == kUnresolved && !on_stack[cur]) {
      stack.push_back(cur);
      on_stack[cur] = true;
      cur = parent_pos[cur];
    }
    // Seed from the deepest already-resolved ancestor, or from a root.
    uint32_t base_depth;
    uint32_t base_root;
    if (cur == kNoParent || on_stack[cur]) {
      // Either we walked off the top, or we closed a cycle: the last row
      // pushed becomes the root of this chain.
      uint32_t top = stack.back();
      stack.pop_back();
      on_stack[top] = false;
      depth[top] = 0;
      root[top] = top;
      base_depth = 0;
      base_root = top;
    } else {
      base_depth = depth[cur];
      base_root = root[cur];
    }
    // Unwind: the stack holds the chain from |i| up to the root, so walking
    // it backwards assigns increasing depths.
    for (auto it = stack.rbegin(); it != stack.rend(); ++it) {
      base_depth++;
      depth[*it] = base_depth;
      root[*it] = base_root;
      on_stack[*it] = false;
    }
  }

  // Pack child subtrees beneath their parent as contiguous, tallest-first blocks.
  std::vector<std::vector<uint32_t>> children(n);
  std::vector<uint32_t> roots;
  for (size_t i = 0; i < n; ++i) {
    if (parent_pos[i] == kNoParent || depth[i] == 0) {
      roots.push_back(static_cast<uint32_t>(i));
    } else {
      children[parent_pos[i]].push_back(static_cast<uint32_t>(i));
    }
  }

  // Use iterative post-order to support deep process trees.
  std::vector<uint32_t> post;
  post.reserve(n);
  {
    std::vector<std::pair<uint32_t, size_t>> stack;
    for (uint32_t r : roots) {
      stack.emplace_back(r, 0);
      while (!stack.empty()) {
        uint32_t node = stack.back().first;
        size_t& next_child = stack.back().second;
        if (next_child < children[node].size()) {
          uint32_t c = children[node][next_child++];
          stack.emplace_back(c, 0);
          continue;
        }
        post.push_back(node);
        stack.pop_back();
      }
    }
  }

  std::vector<int64_t> sub_start(n, 0);
  std::vector<int64_t> sub_end(n, 0);
  std::vector<uint32_t> height(n, 1);
  std::vector<uint32_t> child_offset(n, 0);

  auto place_block = [](std::vector<Row>& occupied, uint32_t h, int64_t start,
                        int64_t end) {
    uint32_t at = 0;
    uint32_t run = 0;
    for (uint32_t r = 0;; ++r) {
      if (r >= occupied.size() || RowIsFree(occupied[r], start, end)) {
        if (run == 0) {
          at = r;
        }
        if (++run == h) {
          break;
        }
      } else {
        run = 0;
      }
    }
    uint32_t top = at + h - 1;
    if (occupied.size() <= top) {
      occupied.resize(top + 1);
    }
    for (uint32_t r = at; r <= top; ++r) {
      RowMark(occupied[r], start, end);
    }
    return at;
  };

  auto order_siblings = [&](std::vector<uint32_t>& v) {
    std::sort(v.begin(), v.end(), [&](uint32_t a, uint32_t b) {
      if (height[a] != height[b]) {
        return height[a] > height[b];
      }
      if (sub_start[a] != sub_start[b]) {
        return sub_start[a] < sub_start[b];
      }
      return ids[a] < ids[b];
    });
  };

  for (uint32_t node : post) {
    int64_t start = ts[node];
    int64_t end = dur[node] == -1 ? std::numeric_limits<int64_t>::max()
                                  : start + std::max<int64_t>(dur[node], 0);
    auto& kids = children[node];
    for (uint32_t c : kids) {
      start = std::min(start, sub_start[c]);
      end = std::max(end, sub_end[c]);
    }
    sub_start[node] = start;
    sub_end[node] = end;

    order_siblings(kids);
    std::vector<Row> occupied;
    uint32_t used = 0;
    for (uint32_t c : kids) {
      uint32_t at = place_block(occupied, height[c], sub_start[c], sub_end[c]);
      child_offset[c] = at;
      used = std::max(used, at + height[c]);
    }
    height[node] = 1 + used;
  }

  // Step 4: place the roots against each other, then push absolute rows down
  // the tree. A child sits one row below its parent's header, offset by the
  // position its block was given within the parent.
  std::vector<uint32_t> layout(n, 0);
  {
    order_siblings(roots);
    std::vector<Row> occupied;
    for (uint32_t r : roots) {
      layout[r] = place_block(occupied, height[r], sub_start[r], sub_end[r]);
    }
    std::vector<uint32_t> stack(roots.begin(), roots.end());
    while (!stack.empty()) {
      uint32_t node = stack.back();
      stack.pop_back();
      for (uint32_t c : children[node]) {
        layout[c] = layout[node] + 1 + child_offset[c];
        stack.push_back(c);
      }
    }
  }

  // Step 5: emit.
  std::vector<Result> out;
  out.reserve(n);
  for (size_t i = 0; i < n; ++i) {
    out.push_back(Result{ids[i], depth[i], layout[i], height[i]});
  }
  return out;
}

int ContainmentLayout::Create(sqlite3* db,
                              void* raw_ctx,
                              int argc,
                              const char* const* argv,
                              sqlite3_vtab** vtab,
                              char** zErr) {
  if (argc != 4) {
    *zErr = sqlite3_mprintf(
        "containment_layout: expected a single subquery argument");
    return SQLITE_ERROR;
  }
  if (int ret = sqlite3_declare_vtab(db, kSchema); ret != SQLITE_OK) {
    return ret;
  }

  auto* ctx = GetContext(raw_ctx);
  auto state = std::make_unique<State>();

  std::string sql = "SELECT * FROM ";
  sql.append(argv[3]);
  auto res = ctx->connection->ExecuteUntilLastStatement(
      SqlSource::FromTraceProcessorImplementation(std::move(sql)));
  if (!res.ok()) {
    *zErr = sqlite3_mprintf("%s", res.status().c_message());
    return SQLITE_ERROR;
  }

  std::vector<int64_t> ids;
  std::vector<int64_t> parent_ids;
  std::vector<bool> has_parent;
  std::vector<int64_t> ts;
  std::vector<int64_t> dur;
  sqlite3_stmt* stmt = res->stmt.sqlite_stmt();
  if (sqlite3_column_count(stmt) < 4) {
    *zErr = sqlite3_mprintf(
        "containment_layout: subquery must select id, parent_id, ts, dur");
    return SQLITE_ERROR;
  }
  do {
    ids.push_back(sqlite3_column_int64(stmt, 0));
    bool null_parent = sqlite3_column_type(stmt, 1) == SQLITE_NULL;
    has_parent.push_back(!null_parent);
    parent_ids.push_back(null_parent ? 0 : sqlite3_column_int64(stmt, 1));
    ts.push_back(sqlite3_column_int64(stmt, 2));
    dur.push_back(sqlite3_column_type(stmt, 3) == SQLITE_NULL
                      ? -1
                      : sqlite3_column_int64(stmt, 3));
  } while (res->stmt.Step());
  if (!res->stmt.status().ok()) {
    *zErr = sqlite3_mprintf("%s", res->stmt.status().c_message());
    return SQLITE_ERROR;
  }

  state->results = ComputeLayout(ids, parent_ids, has_parent, ts, dur);

  std::unique_ptr<Vtab> vtab_res = std::make_unique<Vtab>();
  vtab_res->state = ctx->OnCreate(argc, argv, std::move(state));
  *vtab = vtab_res.release();
  return SQLITE_OK;
}

int ContainmentLayout::Destroy(sqlite3_vtab* vtab) {
  std::unique_ptr<Vtab> tab(GetVtab(vtab));
  sqlite::ModuleStateManager<ContainmentLayout>::OnDestroy(tab->state);
  return SQLITE_OK;
}

int ContainmentLayout::Connect(sqlite3* db,
                               void* raw_ctx,
                               int argc,
                               const char* const* argv,
                               sqlite3_vtab** vtab,
                               char** zErr) {
  PERFETTO_CHECK(argc == 4);
  if (int ret = sqlite3_declare_vtab(db, kSchema); ret != SQLITE_OK) {
    return ret;
  }
  base::ignore_result(zErr);
  auto* ctx = GetContext(raw_ctx);
  std::unique_ptr<Vtab> res = std::make_unique<Vtab>();
  res->state = ctx->OnConnect(argc, argv);
  *vtab = res.release();
  return SQLITE_OK;
}

int ContainmentLayout::Disconnect(sqlite3_vtab* vtab) {
  std::unique_ptr<Vtab> tab(GetVtab(vtab));
  return SQLITE_OK;
}

int ContainmentLayout::BestIndex(sqlite3_vtab*, sqlite3_index_info* info) {
  // The layout is computed once at Create time and always returned whole:
  // there is nothing to push down.
  info->estimatedCost = 1;
  return SQLITE_OK;
}

int ContainmentLayout::Open(sqlite3_vtab*, sqlite3_vtab_cursor** cursor) {
  std::unique_ptr<Cursor> c = std::make_unique<Cursor>();
  *cursor = c.release();
  return SQLITE_OK;
}

int ContainmentLayout::Close(sqlite3_vtab_cursor* cursor) {
  std::unique_ptr<Cursor> c(GetCursor(cursor));
  return SQLITE_OK;
}

int ContainmentLayout::Filter(sqlite3_vtab_cursor* cursor,
                              int,
                              const char*,
                              int,
                              sqlite3_value**) {
  auto* c = GetCursor(cursor);
  auto* t = GetVtab(c->pVtab);
  auto* state =
      sqlite::ModuleStateManager<ContainmentLayout>::GetState(t->state);
  c->results = &state->results;
  c->index = 0;
  return SQLITE_OK;
}

int ContainmentLayout::Next(sqlite3_vtab_cursor* cursor) {
  GetCursor(cursor)->index++;
  return SQLITE_OK;
}

int ContainmentLayout::Eof(sqlite3_vtab_cursor* cursor) {
  auto* c = GetCursor(cursor);
  return c->results == nullptr || c->index >= c->results->size();
}

int ContainmentLayout::Column(sqlite3_vtab_cursor* cursor,
                              sqlite3_context* ctx,
                              int N) {
  auto* c = GetCursor(cursor);
  const Result& r = (*c->results)[c->index];
  switch (N) {
    case ColumnIndex::kId:
      sqlite::result::Long(ctx, r.id);
      return SQLITE_OK;
    case ColumnIndex::kLayoutDepth:
      sqlite::result::Long(ctx, r.layout_depth);
      return SQLITE_OK;
    case ColumnIndex::kDepth:
      sqlite::result::Long(ctx, r.depth);
      return SQLITE_OK;
    case ColumnIndex::kSubtreeHeight:
      sqlite::result::Long(ctx, r.subtree_height);
      return SQLITE_OK;
    default:
      return sqlite::utils::SetError(GetVtab(c->pVtab), "Bad column");
  }
  PERFETTO_FATAL("For GCC");
}

int ContainmentLayout::Rowid(sqlite3_vtab_cursor*, sqlite_int64*) {
  return SQLITE_ERROR;
}

namespace {

class ContainmentLayoutPlugin : public Plugin<ContainmentLayoutPlugin> {
 public:
  ~ContainmentLayoutPlugin() override;

  void RegisterSqliteModules(
      PerfettoSqlConnection* connection,
      std::vector<SqliteModuleRegistration>& out) override {
    out.push_back(MakeSqliteModule<ContainmentLayout>(
        "__intrinsic_containment_layout",
        std::make_unique<ContainmentLayout::Context>(connection)));
  }
};

ContainmentLayoutPlugin::~ContainmentLayoutPlugin() = default;

}  // namespace

void RegisterPlugin() {
  static PluginRegistration reg(
      []() -> std::unique_ptr<PluginBase> {
        return std::make_unique<ContainmentLayoutPlugin>();
      },
      ContainmentLayoutPlugin::kPluginId,
      ContainmentLayoutPlugin::kDepIds.data(),
      ContainmentLayoutPlugin::kDepIds.size());
  base::ignore_result(reg);
}

}  // namespace perfetto::trace_processor::containment_layout
