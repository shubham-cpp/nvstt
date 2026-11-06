# Codebase Memory usage audit

Date: 2026-09-26

## Scope

This audit checks local use of `codebase-memory-mcp` across Pi, OpenCode, Codex, Grok, and direct CLI use.

The audit uses local config files, session logs, SQLite stores, shell history, and the Codebase Memory project list.

## Short verdict

Codebase Memory is useful on this machine. It is not an unused tool.

It is most useful in Pi and Codex. It has real but lighter use in OpenCode and Grok. The direct CLI is useful mostly as support for MCP and indexing, not as a daily manual tool.

The overbuilt part is the broad install and strong always-use instructions in every client. Keep the server. Trim the clients where you do not do serious repo work.

## Evidence summary

| Surface | Actual use | Evidence | Verdict |
|---|---:|---|---|
| Pi | 922 active session calls | `/home/shubham/.config/pi/agent/sessions/**/*.jsonl` | High |
| Codex | 1,386 MCP call references inside completed exec calls across 95 sessions | `/home/shubham/.config/codex/sessions/**/*.jsonl` | High |
| OpenCode | 44 completed calls across 5 sessions | `/home/shubham/.local/share/opencode/opencode.db` | Medium |
| Grok | 45 completed calls across 9 sessions | `/home/shubham/.config/grok/sessions/**/events.jsonl` | Medium-light |
| Direct CLI | 26 shell-history matches, 4 direct index commands | Fish history audit | Low as manual CLI, useful as backend |
| Storage | 20 indexed projects, about 1.1 GiB | `list_projects`, `/home/shubham/.cache/codebase-memory-mcp` | Active infra |

## Pi

Pi has real heavy use.

Config sources:

- `/home/shubham/.config/pi/agent/mcp.json` enables `codebase-memory-mcp`.
- `/home/shubham/.config/pi/agent/extensions/cbmem.ts` registers the tools and calls the CLI with `spawn(BIN, ["cli", "--json", tool], ...)`.
- `/home/shubham/.config/pi/agent/AGENTS.md` tells agents to use graph tools first.

Actual active Pi session calls:

| Tool | Calls |
|---|---:|
| `search_graph` | 286 |
| `get_code_snippet` | 198 |
| `check_index_coverage` | 192 |
| `index_status` | 74 |
| `trace_path` | 56 |
| `index_repository` | 52 |
| `list_projects` | 32 |
| `get_architecture` | 20 |
| `query_graph` | 10 |
| `get_graph_schema` | 2 |

The dotfiles mirror has the same session call counts. I do not count those twice.

Pi result: keep it.

## Codex

Codex has the strongest use after Pi.

Config sources:

- `/home/shubham/.config/codex/config.toml` enables `codebase-memory-mcp`.
- The same file approves the main tools.
- Session and subagent hooks call `codebase-memory-mcp hook-augment`.

Session evidence:

A local JSONL scan found 95 sessions with real calls and 1,386 `tools.mcp__codebase_memory_mcp__...` call references inside completed Codex exec calls.

Tool counts:

| Tool | Calls |
|---|---:|
| `search_graph` | 552 |
| `get_code_snippet` | 211 |
| `check_index_coverage` | 205 |
| `trace_path` | 94 |
| `index_status` | 80 |
| `list_projects` | 71 |
| `search_code` | 64 |
| `get_architecture` | 62 |
| `index_repository` | 23 |
| `query_graph` | 18 |
| `detect_changes` | 3 |
| `get_graph_schema` | 3 |

Codex result: keep it.

## OpenCode

OpenCode has real use, but it is much smaller.

Config sources:

- `/home/shubham/.config/opencode/opencode.json` enables `codebase-memory-mcp`.
- `/home/shubham/.config/opencode/AGENTS.md` tells agents to use graph tools.
- `/home/shubham/.config/opencode/plugins/cbm-augment.ts` runs `codebase-memory-mcp hook-augment`.

Runtime evidence:

- `/home/shubham/.local/share/opencode/log/opencode.log` shows 20 successful MCP connects.
- The same log shows 4 connection failures.
- The same log shows 39 duplicate `codebase-memory` skill warnings.
- `/home/shubham/.local/share/opencode/opencode.db` shows 44 completed calls in 5 sessions.

Actual call counts:

| Tool | Calls |
|---|---:|
| `search_graph` | 15 |
| `check_index_coverage` | 13 |
| `list_projects` | 5 |
| `index_status` | 5 |
| `get_code_snippet` | 3 |
| `index_repository` | 3 |

OpenCode result: keep only if you use OpenCode for repo work. The warnings and connection failures are real friction.

## Grok

Grok has real use, but it is lighter than Pi and Codex.

Config sources:

- `/home/shubham/.config/grok/config.toml` enables `codebase-memory-mcp`.
- `/home/shubham/.config/grok/rules/codebase-memory.md` adds global rules.
- `/home/shubham/.config/grok/agents/codebase-memory*.md` defines graph agents.
- `/home/shubham/.config/grok/skills/codebase-memory/SKILL.md` defines the skill.

Actual completed MCP calls:

| Tool | Calls |
|---|---:|
| `list_projects` | 9 |
| `search_graph` | 8 |
| `search_code` | 7 |
| `get_code_snippet` | 6 |
| `index_status` | 5 |
| `check_index_coverage` | 5 |
| `trace_path` | 3 |
| `get_architecture` | 2 |

Grok result: keep only if you use Grok for codebase work. Otherwise make it opt-in.

## Direct CLI and server

The CLI is active as infrastructure.

Evidence:

- `/home/shubham/.local/bin/codebase-memory-mcp` exists and reports version `0.10.8`.
- The binary is about 280 MiB.
- `/home/shubham/.cache/codebase-memory-mcp` uses about 1.2 GiB.
- The cache has 20 project DBs.
- Fish history has 26 matches for Codebase Memory terms.
- Fish history has 4 direct `codebase-memory-mcp cli index_repository ...` commands.
- `/home/shubham/.cache/codebase-memory-mcp/_config.db` has `auto_index=true`.
- `/home/shubham/.cache/codebase-memory-mcp/logs/cbm-daemon.log` had activity on 2026-09-25.

Current indexed projects from the graph tool total about 1.1 GiB. The largest index is `home-shubham-Downloads-Packages-GitClones-t3code` at about 775 MiB.

CLI result: keep the binary if you keep MCP. Do not think of the CLI as the main value. The main value comes through MCP clients.

## What helped

The tool helped with these tasks:

- Symbol search across large repos.
- Reading exact function snippets without opening many files.
- Tracing callers and callees.
- Checking index coverage before relying on graph results.
- Fast architecture summaries.
- Impact checks with `detect_changes`.

The count pattern supports this. `search_graph`, `get_code_snippet`, and `check_index_coverage` dominate real use.

## What is overbuilt

The install is too broad.

Problems:

- Every client gets strong always-use rules.
- OpenCode shows duplicate skill warnings.
- Some clients use it only lightly.
- The cache costs about 1.2 GiB.
- The binary is large.
- Agents can overuse graph search for literal text or config files.
- Graph coverage is best effort. Agents still need source reads for final claims.

## Recommendation

Keep Codebase Memory for Pi and Codex.

Keep the server and cache if you continue coding with those clients.

Make OpenCode and Grok opt-in unless you actively use them for repo work.

Reduce the strong global wording from "always prefer" to "use for structural code discovery" in clients where it causes noise.

Prune stale large indexes. Start with projects you do not use often, especially the 775 MiB `t3code` index. Use the tool's `delete_project` command for that, not manual file deletion.

Do not remove the whole tool now. The evidence shows real value. Revisit removal only if Pi and Codex usage drops for a month.
