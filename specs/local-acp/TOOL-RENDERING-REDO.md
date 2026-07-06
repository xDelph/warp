# Local ACP Tool Rendering — Review & Redo Plan

Review of pass-1 tool-call display changes (~43 files). Use this to redo cleanly without regression.

**Related docs**

- [MINIMAL-INTEGRATION.md](./MINIMAL-INTEGRATION.md) — file tier index
- [START-FROM-SCRATCH.md](./START-FROM-SCRATCH.md) — full scratch entry point
- [RESTART-GUIDE.md](./RESTART-GUIDE.md) — per-file justifications and regressions

---

## What is actually changed

Most of the diff is import reorder / rustfmt noise (~30 files, zero behavior change).

The real work is Local ACP tool-call rendering + edit diff UI in **6 files**:

| File | What changed |
| ---- | ------------ |
| `app/src/ai/acp/tool_calls.rs` | Per-tool-kind `raw_output` handling; stop double-rendering execute stdout when `content` exists; read tools get syntax-highlighted fences; edit tools ignore giant `raw_output` JSON |
| `app/src/ai/agent/local_acp_tool_call.rs` | `has_visible_body()` counts code sections + diffs, not just plain text |
| `app/src/ai/blocklist/block.rs` | `window_local_acp_diff()` (hunk windowing); wire `CodeDiffView::new_view_only` for ACP edits |
| `app/src/ai/blocklist/inline_action/code_diff_view.rs` | New `new_view_only()` + `should_hide_header` (no approve/reject header) |
| `app/src/ai/blocklist/block/view_impl/output.rs` | Render `CodeDiffView` only when tool call is expanded |
| `app/src/ai/acp/connection.rs`, `app/src/ai/agent/mod.rs` | Leftover test verification comments — delete |

---

## What is done (functionally)

1. **Execute tools** — structured `{stdout, stderr}` renders as fenced output; plain-string `raw_output` is ignored; no duplicate output when ACP already sent `content`.
2. **Read tools** — file content gets language from path extension; raw fallback when no `content`.
3. **Edit tools** — diffs go to `LocalAcpDiff` + unified diff in body; `raw_output` success blobs are suppressed.
4. **Edit UI** — diffs shown via view-only `CodeDiffView` (no action header), windowed to changed hunks ±5 lines, hidden when collapsed.
5. **Tests** — 6 new/updated cases in `tool_calls.rs` covering the above.

---

## What is wrong / regression-prone

1. **Monolithic diff** — rendering logic, UI wiring, and 30-file fmt churn mixed together; hard to review, easy to break unrelated paths.
2. **`window_local_acp_diff` in `block.rs`** — 60 lines in a 7k-line file; belongs in the ACP layer (`tool_calls.rs` or `acp/diff_window.rs`).
3. **`CodeDiffView` hacked for ACP** — `should_hide_header` is a bolt-on to the requested-edit executor component; risks affecting non-ACP diff views.
4. **Dual diff representation** — same edit appears as markdown unified diff in `body.sections` and as `LocalAcpDiff`; redundant UI or inconsistency if one path updates and the other does not.
5. **Dead / duplicated code** — `append_tool_field_sections` unused; `command_raw_output_text` duplicates `command_output_text`.
6. **Collapsed edit = invisible diff** — intentional but UX regression if user never expands; no collapsed preview.
7. **Partial update guard** — `apply_tool_call_update` only replaces body when `!sections.is_empty()`; a later empty `raw_output` update will not clear stale body (may be fine, untested).
8. **Fmt-only files** — `conversation.rs`, `shell_command.rs`, `suggestions_tests.rs`, etc. add merge noise with zero value.

---

## How to do it better (no regression)

Follow [MINIMAL-INTEGRATION.md](./MINIMAL-INTEGRATION.md) tier rules:

- All ACP-to-message mapping stays in `app/src/ai/acp/` (single source of truth).
- Blocklist gets thin hooks only — consume `LocalAcpToolCallMessage`, do not parse JSON or window diffs.
- One representation per concern — either markdown body or structured `diffs` / `terminal_outputs`, not both for the same edit.
- Separate PRs with compile + test gates — never 43 files at once.
- Revert all fmt-only upstream edits — run `rustfmt` only on touched functional files.

---

## Tool-kind mapping rules (target design)

Single pipeline:

```text
ToolCall / ToolCallUpdate  -->  body | terminal_outputs | diffs
```

| Kind | `content` | `raw_output` | Output channel |
| ---- | --------- | ------------ | -------------- |
| Execute | command text | `{stdout, stderr}` | terminal OR body, never both |
| Read | file text | fallback text | body (fenced, ext to lang) |
| Edit | ACP Diff | ignore JSON blob | diffs only (no markdown duplicate) |
| Search / Fetch / Other | text | structured text | body |

---

## Redo plan (from zero)

### Phase 0 — Reset

- [ ] `git checkout HEAD -- .` (revert all modified upstream files from pass 1)
- [ ] Keep `app/src/ai/acp/*`, specs, and existing ACP transport — this redo is tool display only
- [ ] Delete test comments in `connection.rs` and `mod.rs`

### Phase 1 — PR-A: ACP message model (add-only, ~2 files)

**Goal:** Canonical `LocalAcpToolCallMessage` from ACP updates.

**Files**

- `app/src/ai/acp/tool_calls.rs`
- `app/src/ai/agent/local_acp_tool_call.rs`

**Tasks**

- [ ] Implement table-driven `match tool_kind` per rules above
- [ ] Deduplicate `command_*_text` into one `extract_command_output(value) -> Option<String>`
- [ ] `has_visible_body()` checks sections + terminals + diffs
- [ ] Tests only in `tool_calls.rs` — port existing 6 tests plus: partial update, empty raw_output, edit-with-diff-only

**Gate**

```bash
cargo test -p app tool_calls::
```

### Phase 2 — PR-B: Diff windowing utility (add-only, 1 file)

**Goal:** Move hunk logic out of `block.rs`.

**File:** `app/src/ai/acp/diff_window.rs`

```rust
pub fn window_diff(old: &str, new: &str, context: usize) -> (String, String)
```

**Tasks**

- [ ] Unit tests: full-file edit produces only changed hunks plus context
- [ ] Call from `tool_calls.rs` when building `LocalAcpDiff` (window at mapping time, not render time)

**Gate**

```bash
cargo test -p app diff_window::
```

### Phase 3 — PR-C: Blocklist render hook (minimal, 2–3 files)

**Goal:** Display only; zero ACP parsing in blocklist.

**Files**

- `app/src/ai/blocklist/block.rs`
- `app/src/ai/blocklist/block/view_impl/output.rs`

**Tasks**

- [ ] Keep `ensure_local_acp_edit_view()` but pass pre-windowed `LocalAcpDiff` (no `window_local_acp_diff` in block.rs)
- [ ] Do not add `should_hide_header` to shared `CodeDiffView` yet
- [ ] `render_local_acp_tool_call`:
  - title-only when `!has_visible_body()`
  - collapsible header when body or diffs exist
  - edit: render `CodeDiffView` child when expanded
  - non-edit: `render_text_sections` for body
- [ ] Do not touch import order in unrelated files

**Gate:** manual — run local ACP agent; trigger read, execute, and edit tools

### Phase 4 — PR-D: CodeDiffView view-only mode (only if PR-C needs it)

**Goal:** Isolated UI primitive, not ACP-specific hacks.

**File:** `app/src/ai/blocklist/inline_action/code_diff_view.rs`

**Tasks**

- [ ] `CodeDiffState::ViewOnly` already exists; expose `new_view_only()` cleanly
- [ ] Hide action buttons via state match, not a separate `should_hide_header` bool
- [ ] Verify Oz requested-edit path unchanged

**Gate**

```bash
cargo test -p app code_diff_view::
```

Plus Oz edit flow smoke test.

### Phase 5 — Regression checklist (after each PR)

- [ ] Execute with `content` + `raw_output` — command shown once, no stdout duplicate
- [ ] Execute with only `raw_output` JSON — stdout fenced
- [ ] Read `src/foo.rs` — `rs` fence
- [ ] Edit — diff viewer, no `afterFullFileContent` leak, collapsed shows title only
- [ ] Oz / cloud blocklist edits — unchanged (no `local_acp` cfg paths hit)
- [ ] `cargo test -p app` green; no fmt-only diffs outside PR scope

### Phase 6 — Explicit non-goals

- No changes to `connection.rs`, `submit_model.rs`, controller, harness picker
- No import reorder / rustfmt drive-by
- No dual markdown + CodeDiffView for same edit — use CodeDiffView for Edit, markdown for everything else

---

## Bottom line

The current branch solves real bugs (duplicate execute output, giant edit JSON, full-file diffs) but implements them as a tangled 43-file patch.

Redo as **4 small PRs**: mapping, diff util, render hook, optional CodeDiffView API — with tests at each gate and zero formatting churn.
