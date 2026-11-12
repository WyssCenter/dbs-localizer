# Phase C — subagent contract (clean-room)

**Applies to:** `dbsloc/**`, `crates/**`, `tests/**`

Applies when the **orchestrator** (human or lead agent) spawns **subagents** for **Wyss DBS Localizer implementation** on branch `phase-c` (or any Phase C work). **Default:** subagents do **all implementation** (code + tests); the main agent does not edit implementation files first—only after a subagent **retry** fails (see `AGENTS.md` §5 Phase C). The orchestrator must paste the **block** below into every subagent prompt.

## Mandatory prompt block (paste into every subagent)

```
CLEAN-ROOM (GPL firewall):
- Read and write ONLY this repository: `specs/`, `dbsloc/`, `crates/`, `tests/`, `AGENTS.md`, `pyproject.toml`, `Cargo.toml` — not sibling repos.
- FORBIDDEN: any on-disk or remote path to Lead-DBS MATLAB (e.g. a local `leaddbs` checkout, clone, or download).
- FORBIDDEN: web search, HTTP fetch, URL open, "look up on GitHub", package registry browse, or any action that could retrieve Lead-DBS or MATLAB reference source online.
- FORBIDDEN: read MCP / browser tools that use the network, unless the orchestrator explicitly allows for non-code deps (e.g. ty/ruff version — prefer offline).
- Sources allowed: repo specs/*, AGENTS.md, LICENSE, and papers cited by title only (no download). `specs/*.md` are the contracts.
- If a detail is missing: return "UNSPECIFIED" + which spec section needs amendment; do NOT invent from memory of Lead-DBS.
- Subagent: readonly unless this task is explicitly to implement a file; then only touch paths in the task.
- Every new source file starts with the SPDX header of its directory (`docs/LICENSING.md`): Apache-2.0 for every file under `fastwarp/` (git subtree of the engine; change it only in a commit that touches nothing else and carries a message fit for a public repository, see `docs/LICENSING.md` *The fastwarp subtree*), GPL-3.0-only elsewhere.
```

**Orchestrator settings:** give explore/review subagents read-only tools; grant write access only for named implementation tasks. **Network:** not every agent tool can force a subagent offline — the **orchestrator** should run such sessions with **network disabled** at OS/VPN level, or **disable** networked MCP servers and web tools for that session.

**Reliability:** if a subagent **fails** (timeout, API limit, or tool error), **retry** at least once (smaller scope or split prompt). If still failing, **the orchestrator implements in the main session** so work continues (see `AGENTS.md` Phase C).

## Apache-2.0 reference exception (orchestrator-granted, per task)

When the task says "port from ITK/ANTs", the subagent may read the listed files under `third_party/itk_ref/` (ITK 5.4 / ANTs 2.6.5, Apache-2.0; git-ignored, see its `README.md`) and must (1) keep the upstream copyright line and the Apache header from the "Header template for ported files" section of `fastwarp/docs/THIRD_PARTY_PORTS.md` in every derived file, (2) add a line to `fastwarp/docs/THIRD_PARTY_PORTS.md` naming the upstream file, class and what changed, (3) not read anything else outside the repository. Lead-DBS remains forbidden without exception. Without this grant in the task, the block above applies unchanged.

## After implementation (orchestrator, not subagent)

A similarity check (**MOSS** or equivalent) against Lead-DBS is not required (owner decision 2026-11-12, `AGENTS.md` §5).
