# Code-review skepticism

A change is not done when it compiles. It is done when a fresh skeptic cannot find a contract break or a test that would stay green on the unfixed tree.

## Loop

1. Never skip sweep 1.
2. Fresh skeptic each sweep.
3. Blocking finding ⇒ change the code or tests, or show concrete evidence the skeptic is wrong.
4. Cap 3 sweeps. Still blocking ⇒ **BLOCKED**. Do not merge unless the user overrides.
5. ACCEPT only on `NO BLOCKING FINDINGS`.

## What is blocking

- AGENTS.md / DESIGN.md / PLAN.md contract break
- Dual `cdata_blob` + content, or `raw_zip` of a listed jar
- Renamed v1 JSON fields or a format bump used as a “fix”
- Tests that log instead of fail, or that are already green on the unfixed tree
- MSRV 1.80 / `forbid(unsafe_code)` / edition-2024 dependency
- Restore path that crashes, drops members, or 10×-shrinks a fat jar

## What is not blocking

- Style nits that do not change behavior
- Extra coverage the plan already listed as optional

## This repo

Cloud VMs cannot reach Origin `matt-brewer/agent-skills`. Use `.cursor/skills/skeptic-code-review/SKILL.md` in this checkout. Skip `record-hint-outcome` / `capture-lesson` / `record-repo-memory` if those skills are not present.
