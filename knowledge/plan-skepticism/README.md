# Plan skepticism

A plan is not done when it reads well. It is done when a fresh skeptic cannot find a step that fails against the current tree.

## Loop

1. Never skip sweep 1. Obvious plans hide assumption gaps.
2. Fresh skeptic each sweep (new subagent; no attachment to earlier feedback).
3. Blocking finding ⇒ change the plan or show concrete evidence the skeptic is wrong. “Pedantic” is not a resolution.
4. Cap 3 sweeps. Still blocking ⇒ **BLOCKED**. Do not implement unless the user overrides.
5. ACCEPT only on `NO BLOCKING FINDINGS`.

## What is blocking

- A test that stays green on the unfixed tree
- A step that uses the wrong API, path, or current-tree fact
- Dual copy / `cdata_blob` / `raw_zip` of a listed jar
- Unmeasured “fact” used as a restore-hash gate
- Missing error path that makes restore crash or skip-exact a healthy jar
- New dep that breaks MSRV 1.80 or `forbid(unsafe_code)`

## What is not blocking

- Naming nits
- Extra tests that do not change the gate
- Risks the plan already names with a prescribed fix

## This repo

Cloud VMs cannot reach Origin `matt-brewer/agent-skills`. Use `.cursor/skills/skeptic-plan-review/SKILL.md` in this checkout. Skip `record-hint-outcome` / `capture-lesson` / `record-repo-memory` if those skills are not present.
