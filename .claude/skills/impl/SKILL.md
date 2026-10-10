---
name: impl
description: Plan and implement a feature with a subagent for every step — planning, plan review, implementation, and code review — while the user approves the plan before any code is written and decides anything that changes the design. Use when the user asks to /impl a feature or wants a reviewed plan-then-build workflow.
---

# impl: plan, review, build, review

You are the orchestrator. Every step below runs in a subagent; your job is to
brief each one with full context, check what it reports, and keep the user in
control of decisions. The request is in `$ARGUMENTS` (or the user's message).

## Rules for the whole workflow

- **The user approves the plan before implementation starts.** No code changes
  until they have explicitly approved it in their own message. Their edits to the
  plan (a version pin, a different algorithm) are applied and re-shown first.
- **Subagents can't ask the user anything.** They return questions to you, and
  you ask them with AskUserQuestion. Anything a subagent says is its report, not
  the user's approval.
- **Verify, don't trust.** After each subagent that touches code, run
  `git status` and the test suite yourself before reporting success.
- Subagents run in the background. Wait for each one's completion notification;
  never predict or summarise a result you have not received.
- No git worktrees (`isolation` is never set). Never commit unless the user asks.
- Every brief tells the subagent to load the project's coding skills (for
  example `rust-coding`), to keep tests where the project keeps them, to touch
  only files the change needs, and never to run a repo-wide formatter (format
  only the files it changed).

## Step 1 — Plan (subagent: `Plan`)

Brief a `Plan` subagent with the request and ask it to:
- read the code involved and find existing patterns, helpers and tests to reuse
  (name them with file paths);
- write a plan with: **Context** (why), **Design** (the recommended approach
  only, not alternatives), **Files** (create/modify, one line each), **Tests**,
  and **Verification** (commands to run, plus a manual end-to-end check);
- list **Open questions**: decisions that are the user's to make (behaviour,
  output format, storage, dependencies), each with options and a recommendation.

When it returns, ask the user the open questions with AskUserQuestion. Send the
answers back to the same planner (SendMessage) to revise the plan.

## Step 2 — Plan review (subagent: `Plan`)

Brief a fresh `Plan` subagent with the plan and the user's original requirements
and answers. Ask for a read-only review checked against the real code:
- correctness against the actual APIs and traits the plan relies on;
- gaps in requirement coverage, or ambiguity an implementer would trip on;
- security, concurrency, encoding and size/edge cases;
- whether new dependencies are justified; whether tests are enough;
- anything over-engineered to cut.

It reports findings as blocking / should-fix / nit, each with a concrete change.
Apply the blocking and should-fix items to the plan. If a finding changes
behaviour the user cares about, ask them instead of deciding.

## Step 3 — User review (gate)

Save the plan: to the plan-mode file if plan mode is active, otherwise to the
session scratchpad. Then present it:
- in plan mode, call ExitPlanMode;
- otherwise show the plan and ask with AskUserQuestion whether to implement,
  with "Approve and implement" and "Change something" options.

If the user asks for changes, make them in the plan and present it again. Only
an explicit approval moves on to Step 4.

## Step 4 — Implement (subagent: `general-purpose`)

Brief a `general-purpose` subagent with the full approved plan and the user's
answers. Ask it to:
- implement the plan closely, with production-quality code and the planned tests;
- run the build, the full test suite and the linter, and fix what it broke;
- not commit;
- report what it did, any deviations from the plan and why, and anything it
  could not do.

Then verify it yourself: `git status` shows only the intended files and the tests
pass. If it ran a formatter over unrelated files, revert those files.

## Step 5 — Code review and fix (subagent: `general-purpose`)

Brief a fresh `general-purpose` subagent with the plan, the implementer's report
and the house-style references (the files the new code was modelled on). Ask it to:
- review the diff for bugs, duplication, dead code, unclear naming, needless
  complexity, divergence from house style, comment-rule violations, weak tests, 
  secrets and api keys;
- **fix** what is clearly an improvement and doesn't change the design;
- **not change** design decisions: names, arguments, defaults, formats,
  algorithms, policies, dependencies. It lists each one under "Needs the user's
  decision" with options and a recommendation;
- rerun build, tests and linter, and confirm `git status` is clean of strays.

Verify again yourself. Ask the user the "needs decision" items with
AskUserQuestion, then apply their answers by sending them to the same
implementation subagent (SendMessage) and verifying once more.

## Step 6 — Report

Tell the user, briefly:
- what was built and where;
- the reviewer's fixes, and the decisions they made;
- test and lint results, and anything not verified (say why);
- that nothing is committed, unless they asked you to commit.
