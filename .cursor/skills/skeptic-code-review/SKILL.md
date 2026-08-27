---
name: skeptic-code-review
description: Run skeptic sweeps on an implementation that is already in hand. Use when code changes need adversarial review before they are treated as done.
---

# Skeptic Code Review

After an implementation is put together, it must survive skeptic review before it is treated as done. The skeptic's job is to find bugs, contract violations, and missing tests; the loop repeats until a sweep finds nothing blocking.

## The sweep loop

1. **Use the diff and files already in hand** (do not invent a review target).
2. **Spawn a skeptic subagent** (a general-purpose subagent via the Task tool). Give it, verbatim: the original user request, the files/diff to review, and the repository/workspace paths it needs to verify claims. Use the prompt template below.
3. **Triage the findings.** For each **blocking** finding, fix the code or tests. Resolve findings yourself when the fix is clear; escalate to the user only for genuine scope or product decisions. Apply **non-blocking** findings at your discretion.
4. **Run another sweep** with a fresh skeptic subagent against the revised tree. Do not reuse the previous subagent.
5. **Stop when a sweep returns zero blocking findings**, or after **3 sweeps**. If blocking findings remain after the third sweep, present the work labeled **BLOCKED**. Do **not** merge or treat it as final unless the user explicitly overrides.
6. When updating a Cursor goal, write the current status back to the goal after each sweep.

## Skeptic prompt template

```text
You are a skeptic reviewing an implementation. Your only job is to find
problems; do not praise the change or rubber-stamp it. Verify behavior
against the actual codebase at <workspace path> rather than trusting the
diff description.

Original request:
<user request>

Diff / files under review:
<paths and summary, or full diff>

Hunt specifically for:
- Contract violations (AGENTS.md / DESIGN.md / PLAN.md)
- Tests that stay green on the unfixed tree, or that log instead of fail
- Dual copies, renamed JSON fields, format bumps used as a "fix"
- Missing error paths, rollback, and restore identity gaps
- New dependencies that break MSRV or forbid(unsafe_code)
- Gaps between what was requested and what the code delivers

Return a list of findings. Classify each as BLOCKING (wrong results, contract
break, untested gate) or NON-BLOCKING (improvement or noteworthy risk). For
each finding give: the file/step it concerns, the concrete problem, the
evidence (file/line), and a suggested fix. If you find no blocking problems
after genuinely attempting to break the change, say exactly: NO BLOCKING
FINDINGS.
```

## Rules

- Never skip the first sweep, even for changes that look obviously fine.
- Do not present the implementation as final while any blocking finding remains.
- After 3 sweeps still blocking: present **BLOCKED**; do not merge unless the user explicitly overrides.
- A finding is only resolved by changing the code/tests or by concrete evidence that the skeptic is wrong; "the skeptic is being pedantic" is not a resolution.
- Report to the user how many sweeps ran and what changed as a result.
- After every finished loop (clean or BLOCKED): if record-hint-outcome is not in this repo, say `no effectiveness signal`.

Related knowledge: `knowledge/code-review-skepticism/README.md` in this repository.
