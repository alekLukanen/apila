# Memory test suite: headless integration arcs for skill memory

Status: design, not implemented (revision 6, after review rounds 1–5).
Validates: `plans/memory.md`, cited below as **M§n**.

The suite runs agent sessions headlessly through `Runtime`, with no TUI. It applies controlled
changes to the environment between sessions, checks what the memory system did, and
pretty-prints every validation case and metric.

| Arc | Environment | What it shows | Tiers |
|---|---|---|---|
| **HN** | https://news.ycombinator.com (live), or an offline HN-shaped page | The basics on a real site: a skill is created, reused in a later session, and the reuse costs less. | L; D smoke |
| **Site** | "Driftwood Books", a local site the test serves and changes between sessions | Revision from feedback, the verdict gate, the shrink guard, extracting a shared sub-skill, a breaking change in a dependency, pins, compatibility checks, consolidation of blocked dependents. | D, L |
| **SQLite** | "Harbor" databases generated from a seed, whose schema and data properties change | Whether the agent understands the data model, keeps its overview skill and the analysis skills that depend on it current, and whether **later sessions benefit from the updated skills** (§9.3). | L (main), D (mechanics subset) |
| **Lifecycle** | Small independent scenarios | Migration, recovery, guidance change, re-embed, deprecation and reactivation, merge, regression and revert, acknowledge, concurrency, faults, two agents, backoff. | D |

The suite grows with the memory phases (M§16). Cases for an unimplemented phase are shown as
`SKIP (phase N)`. **§10 defines Slice 1**, the first runnable piece against today's code.
**§12 lists the memory.md amendments the suite needs**, each tagged with the phase it must
land before.

---

## 1. What exists today (verified at `8da86db`)

| Thing | Where | Relevance |
|---|---|---|
| A headless runtime | `Runtime::new(RuntimeConfig)`, `start_agent`, `send_agent_message(id, String)`, `clear_agent_session -> ClearOutcome`, `agent(id) -> Option<AgentDefinition>` (`src/core/runtime/runtime.rs`) | Public through `apila::core::…`, so `tests/` can drive sessions with no TUI. |
| Agent snapshot | `AgentDefinition::{state, messages, session_spend, memory_view, clear_pending, queued_messages}` | Settled detection, transcript, tool calls, spend. |
| How a clear works | `clear_session` sets `clear_pending`. `AgentInner::clear`, on the agent thread, calls `session_queued()` only when a session id exists, enqueues the `AgentJob` whenever skills are on, and resets `clear_pending` last, under the lock. Session rows are created lazily on the first sync. | "Settled, then barrier" is a sound order. An empty clear enqueues work without setting `queued`. |
| Memory status | `MemoryView { skills, pending, last_job, queued, jobs_base, jobs_live, message, .. }`, `memory_spend()` | Live accounting and error reporting. |
| The store | `MemoryStore::open_existing`, `sessions`, `session_messages`, `list_skills`, `skills`, `get`, `job`, `last_job`, `memory_totals`, `session_spend`. `rows` and `execute` are private. | The suite runs its own read-only SQL through `turso_core`. |
| Transcript rendering | `writer::render_transcript` (pub) | Step artifacts. |
| The writer thread | `MemoryWriter::start`; `run` loops on `jobs.recv()` one job at a time; `process` (pub) runs `recover` before `analyse_pending` | A barrier answered in queue order is sound. |
| Restart semantics | each `Runtime::new` gets a fresh `process_token()` and its own `recovered` set; `recover_sessions` enqueues one job per agent that has a `memory.db` and a readable config. `recover` marks other owners' `running` jobs `interrupted`. `Agent::drop` sends `Shutdown`, and the thread syncs and leaves its session open. | An in-process restart acts like a new process. |
| Unit-test fake | `test_support::StubOpenRouter` | `#[cfg(test)]`, so `tests/` cannot see it. |
| End-to-end memory tests | `test_runtime.rs` (`a_cleared_session_is_analysed_into_a_skill_when_skills_are_on`, `an_agent_finds_and_reads_a_skill_in_a_later_session`, `a_session_left_open_is_analysed_by_the_next_process`) | The pattern this suite generalises. |
| SQLite tool | `<agent_dir>/databases/<name>.db`; `SqliteTool::new_state` lists `*.db` files with valid names lazily, on a session's first sqlite call; the `drop` action (`drop_database`) removes `""`, `-wal` and `-log`; `ATTACH` and `VACUUM INTO` are refused to the agent | Generated databases can be placed between sessions; WAL files must be handled (§9.3, §8). |
| Webpage tool | `fetch_webpage` (`allow_private_hosts` and `use_proxy` default to false), `read_webpage_data` | Local site access in L tier. |
| OpenRouter client | no retries | A fault-injected 500 consumes exactly one scripted reply. |
| Dependencies | no `[dev-dependencies]`, no `tests/`; `turso_core`, `reqwest` (blocking), `scraper`, `regex`, `serde_json`, `chrono` and `unicode-width` are normal dependencies | **No new crates** (§14). |

---

## 2. Decisions at a glance

1. **One integration-test binary with its own runner.** `[[test]] name = "memory_suite"`,
   `path = "tests/memory_suite/main.rs"`, `harness = false`. Arcs are stateful sequences.
   The runner prints directly and still runs under plain `cargo test` (D tier only).
2. **Two tiers.**
   - **D (deterministic)**: a fake OpenRouter checks the mechanics exactly. It is free, and
     runs in under a minute.
   - **L (live)**: real models, opt-in, with budget caps. It checks behaviour statistically,
     and mechanics through **consequence checks** that hold for any model (§6.4).
   - Replay is deferred (§13).
3. **D-tier scripts are declarative** (§5.1):
   - **agent scripts make only memory calls**;
   - writer conversations come from a builder;
   - **validator, compat and consolidator replies are keyed by content**, not by order;
   - each step declares the **expected sets** of validated drafts, checked pairs and
     reviewed clusters, so a mistake reads as a set diff.
4. **One runtime API added before the suite:** the writer barrier `Runtime::wait_for_memory`
   (§4.3).
5. **Maintenance is controlled in D** (paused, then `restart()`), so intermediate states are
   observable (§4.4). **L runs production-like config**, and its checks read history and
   consequences.
6. **Every check has a kind and a scope.** The kind is `Mechanic`, `Behaviour` or `Echo`
   (Echo is never counted). The scope is `D` (presupposes a script) or `DL` (holds for any
   model). Every step has a `tiers` column.
7. **SQLite probes are forked leaves with three arms** (updated memory, stale memory,
   cold). They are compared with the repeat as the unit and one primary test per claim (an
   exact sign-flip test over per-repeat mean differences, which needs at least 4 repeats), so "the agent adapted through its updated skills" is measured directly.
   A library-wide staleness table shows whether every skill, not just the overview, kept up
   (§9.3).
8. **The environments are generated from a seed and a version.** The harness knows the
   ground truth.
9. **The suite follows memory.md as written.** Where memory.md has a gap that a step depends
   on, §12 gives the exact amendment text and the phase it must land before.

---

## 3. File layout

```
Cargo.toml                         # + [[test]] memory_suite, harness = false
tests/memory_suite/
  main.rs                          # cli, arc registry, run loop, exit code
  harness/
    mod.rs
    cli.rs                         # args + env; tolerant of libtest/nextest flags (§11)
    phase.rs                       # Phase, IMPLEMENTED_PHASE, detect_phase()
    project.rs                     # TestProject, AgentSpec, restart(), fork(), copies, redaction
    driver.rs                      # AgentDriver, SessionRun, writer_only_session()
    fake/
      mod.rs                       # FakeOpenRouter: thread per connection, routes, log
      reply.rs                     # Reply, FromFn, Gate, StepVars
      dsl.rs                       # agent(), writer(), consolidator(), expect()
      responders.rs                # content-keyed validator / compat / consolidator
    embedder.rs                    # HashingEmbedder
    proxy.rs                       # RecordingProxy (live accounting and attribution)
    probe.rs                       # MemoryProbe: read-only SQL; snapshots; row diffs
    consequences.rs                # the DL consequence checks (§6.4)
    check.rs                       # Check, Kind, Scope, Severity, Status
    metrics.rs                     # SessionMetrics, pooled paired statistics
    report.rs                      # terminal rendering, report.json
    rng.rs                         # SplitMix64
  sites/
    driftwood.rs                   # server, SiteVersion, ground truth (L tier)
    hn.rs                          # live preflight, reference parse, offline page server
  data/
    harbor.rs                      # logical rows, SchemaVersion projections, references, facts
  arcs/
    selftest.rs
    hn.rs
    site.rs
    sqlite.rs
    lifecycle.rs
tests/fixtures/memory_suite/
  hn/front_page.html, hn/page_2.html   # synthetic HN-shaped pages (invented stories)
  coverage/*.md                        # fixture skill bodies for the coverage grader's selftest
  memory_v3.sql                        # v3 schema + rows, captured before Phase 0 (§10.1)
```

Conventions (`.claude/skills/rust-coding`): comments of at most 3 lines. The pure helpers
are checked by the `selftest` arc, because `#[test]` functions do not run in a
`harness = false` binary.

---

## 4. Driving the agent headlessly

### 4.1 Seams used as they are

`Runtime::new`, `start_agent`, `send_agent_message`, `clear_agent_session`, `agent(id)`. A
restart drops the `Runtime` and builds a new one (§4.4).

### 4.2 Harness types (`driver.rs`)

```rust
impl AgentDriver<'_> {
    pub fn start(&self) -> Result<(), HarnessError>;
    pub fn say(&self, text: &str) -> TurnRun;                      // returns when settled
    pub fn end_session(&self) -> EndedSession;                     // clear, settle, barrier
    pub fn session(&self, step: &StepCtx, turns: &[&str]) -> SessionRun;
    pub fn clear_no_wait(&self) -> ClearOutcome;                   // overlap steps
    pub fn writer_only_session(&self, step: &StepCtx, writer: WriterScript) -> SessionRun; // D only
}

pub struct SessionRun {
    pub session_id: i64,
    pub turns: Vec<TurnRun>,
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ObservedCall>,   // name, parsed args, result text, is_error
    pub spend: Spend,
    pub writer: WriterOutcome,           // memory_jobs row and spend
    pub final_state: AgentState,
    pub wall: Duration,
}
```

- **Settled** means `state` is `Idle` or `Failed(_)`, `queued_messages()` is empty and
  `clear_pending()` is false. **Iterations** means assistant messages since the turn's user
  message. A `Failed` turn is recorded, not raised.
- **Session id** is read once, after the first `say` settles, as `MAX(id)` of `sessions`. It
  is the only probe read made before a barrier.
- **`end_session` snapshots before clearing.** `AgentInner::clear` empties `messages` and
  moves `session_spend` into `past_sessions`. So `end_session` first copies
  `agent(id).messages()` and `session_spend()` into the `SessionRun`, then calls
  `clear_agent_session`. After the barrier it cross-checks them against the stored
  `session_messages` and `session_spend(session_id)` (a hard check, which also tests the
  recorder).
- **`writer_only_session(script)`** (D only): one user turn ("checkpoint"), one text reply
  from the agent route, then `script` on the writer route. It is how a D step makes a change
  that only the writer can make. It is a real analysed session, so it **counts toward
  `every_sessions`, `cooldown_sessions` and the analysed-session totals**.
- **Waits:** D 15 s per turn and per barrier; L 10 min per turn and 15 min per barrier.

### 4.3 The one API to add: a writer barrier

`MemoryView` polling has gaps: an empty clear enqueues without `queued`;
`recover_sessions` enqueues silently; from Phase 2, maintenance follows analysis with no
signal.

The change is in `src/core/memory/writer.rs` and `runtime.rs`, about 30 lines, plus a unit
test in `test_writer.rs`:

```rust
enum WriterCommand { Job(AgentJob), Barrier(Sender<()>) }   // replaces Sender<AgentJob>
impl WriterHandle { pub fn barrier(&self) -> Option<Receiver<()>>; }
impl Runtime     { pub fn wait_for_memory(&self, timeout: Duration) -> bool; }
```

The thread is sequential, so a barrier answered in queue order proves that every earlier
job has finished, including the maintenance pass inside `process`. Until the API lands, the
harness polls and reports `WARN (no barrier)`.

### 4.4 Controlling maintenance (D) and reading it (L)

The barrier waits for analysis **and** the maintenance pass that follows in the same
`process` call (M§9.1). So, in D:

**Observation points.** Each Phase 2/3 check names one:

| Point | How it is reached |
|---|---|
| `post-analysis` | the step runs with **maintenance paused**: `consolidation.enabled = false`, `dependencies.max_compat_checks = 0` (needs amendment **A4**) |
| `post-maintenance` | `project.restart(delta)` with maintenance on, which runs one pass with no new session (needs amendment **A5**) |
| `history` | append-only rows, **ordered by `id`** (`created_at` is whole seconds): `skill_pins.changeset_id` / `maintenance_job`, `skill_compat_checks.maintenance_job`, `skill_feedback.dedupe_key` |

**`project.restart(delta)`:**

1. Barrier; wait until every agent is settled.
2. Assert there are **no `active` session rows**, unless the step intends recovery (L2).
   Otherwise `recover` would analyse them, which is an unscripted writer call.
3. Drop the `Runtime`. Old agent threads exit asynchronously; nothing in the harness waits
   on them, because the barrier and the settled check already cover the writer.
4. Rewrite the agents' `config.json` with `delta`.
5. Build a new `Runtime`. `recover_sessions` enqueues a job per agent, and that job runs the
   maintenance pass.
6. Barrier.
7. `start_agent` again before the next session.

The new `MemoryWriter` has fresh sweep state (**A5**), so a restart is also how a step gets
a second sweep.

**Pinned D-tier settings:**

| Setting | D value |
|---|---|
| `consolidation.every_sessions` | 1000 (trigger (d) effectively off) unless a step sets it |
| `consolidation.feedback_threshold` / `cooldown_sessions` | 2 / 3 |
| `consolidation.max_clusters` / `max_changes` / `max_confirms` / `max_iterations` | 3 / 6 / 12 / 6 |
| `dependencies.max_compat_checks` | 8, or 0 when paused |
| `writer_max_iterations` | 8 |
| `agent_max_iterations` | 12 |

**L-tier config is production-like:**

- every block at its defaults (`every_sessions` 5, maintenance always on, never paused);
- `writer_max_iterations` 12 and `agent_max_iterations` 40.

L tier never pauses, because a paused step would change what the real system does
afterwards. L checks run after each barrier, over history rows and consequences (§6.4).

Each step table has a **fires** column: the consolidation trigger expected in that step's
D-tier maintenance pass (`a`–`e`, `none`, or `—` when paused). In L, triggers are reported,
not asserted.

---

## 5. Tiers

### 5.1 D tier: the fake, declarative scripts, expected sets

**Server.** `FakeOpenRouter` is a loopback HTTP server on `127.0.0.1:0` with the same wire
shape as `StubOpenRouter`. **It handles each connection on its own thread**, so a reply held
at a `Gate` on one route (the writer) never blocks another route's requests (the agent) in
overlap scenarios.

**Usage and cost are always present and distinct.** Every reply carries
`usage {prompt_tokens, completion_tokens, total_tokens, cost}`. Embeddings carry
`usage {total_tokens, cost}`. The values are unique per route and per reply:

- cost = `route_base + n·ε`, where `route_base` is, for example, 1e-4 for the agent, 2e-4
  for the cold agent, 3e-4 for the second agent, 5e-4 for the writer, 7e-4 for the
  validator, and so on, with ε = 1e-7 and n the reply's index on its route;
- tokens follow the same scheme.

Every spend sum then identifies exactly which replies it contains. A response without
`usage.cost` would be counted `unpriced` by `Spend::add_response`, and would make the cost
checks pass trivially, so the fake never omits it. A misattributed job (L11) therefore
changes the sums.

**Routes by model.** M§11 lets each component name its own model:

| Component | D model | Reply source |
|---|---|---|
| memory agent | `test/agent` | ordered agent script |
| cold agent | `test/agent-cold` | ordered agent script |
| second agent (L11) | `test/agent-2` | ordered agent script |
| writer | `test/writer` | ordered writer script (builder) |
| validator (P1) | `test/validator` | **content-keyed responder** |
| compat (P2) | `test/compat` | **content-keyed responder** |
| consolidator (P3) | `test/consolidator` | **content-keyed responder** |
| embeddings | — | `HashingEmbedder` |

**Agent scripts are memory-only by default.** A D agent makes only the memory calls
(`search_skills`, `get_skill`, the printed `for:` follow-ups, `report_skill_outcome`), then
one text `ANSWER`. It makes no fetches, SQL or INSERTs. A step opts in to real tool calls only
when a check needs them. The D tier therefore needs neither the Driftwood server nor
per-version SQL. Drift is narrated to the writer through the transcript, and its effect is
in what the writer script stages.

**Reply primitives** (`fake/reply.rs`):

```rust
pub enum Reply {
    Text(String),
    ToolCalls(Vec<(String, serde_json::Value)>),
    FromFn(Box<dyn Fn(&FakeRequest, &mut StepVars) -> Result<Reply, String> + Send>),
    Status(u16, String),
    Gate(GateId, Box<Reply>),
}
```

- `FromFn` sees the request (the latest tool results, which include what memory served) and
  a per-step **`StepVars`** bag (`vars.set("issue", id)`, `vars.rev("catalog")`).
- A closure that returns `Err(msg)` records a **Mechanic failure** on the step and replies
  with `Text("ANSWER: {}")`, so the session ends cleanly and the run carries on.
- Served-text helpers parse the header, `rev N`, the `Depends on:` lines and the exact
  `for:` call.

**The builder DSL** (`fake/dsl.rs`). Defaults keep steps short:

```rust
agent()
    .search("load driftwood catalog")              // top hit stored as vars["catalog"]
    .get_top(|served| served.expect_rev(2))        // asserts on served text
    .follow_deps()                                 // calls each printed `for:` line verbatim
    .report(Var("catalog"), Partial, Some((Outdated, "selectors changed")))  // vars["issue"]
    .answer(json!({}))

writer()
    .outcome("scrape-driftwood-catalog", Partial)
    .revise("scrape-driftwood-catalog", |s| s.body(CATALOG_V2).addresses(Var("issue")))
    .commit(Succeeded)

validator().default(Accept).on("scrape-driftwood-catalog", Reject("drops a pitfall"))
compat().on_pair("scrape-driftwood-catalog", "paginate-driftwood", Incompatible("cursor param"))
consolidator().on_cluster_with("scrape-driftwood-authors", |c| c.revise_confirm("paginate-driftwood"))

expect()
    .validated(["scrape-driftwood-catalog"])
    .compat_pairs([("scrape-driftwood-catalog", "paginate-driftwood")])
    .clusters([["scrape-driftwood-authors"]])
```

- `revise` fills `base_revision` from the latest `read_skill` result, or from the store.
- `commit` adds the `end_turn`.
- Validator replies default to `accept`.
- **Embedding counts are derived, not declared.** The harness counts staged drafts whose
  `when_to_use` changed, plus searches, hints and re-embeds, from the step's own log, and
  checks that against the embeddings route.

**Exact accounting, as set diffs.**

- The agent and writer routes are ordered: every scripted reply must be consumed, and no
  extra request may arrive.
- The content-keyed routes are checked against `expect()`. The validator set is
  `{skill@draft}`; the compat set is `{(dependent, dependency)}`; the cluster set is the
  skills named in each consolidator request's cluster section.
- A mistake reads as `validated: expected {catalog}, got {catalog, paginate-driftwood}`, so
  it is clear whether the code or the script author's expectation is wrong.

**What D proves.** Store transitions, served text, gating, triggers and cluster selection,
pin states, jobs and spend, all exactly. It does not prove that a model chooses well; that
is L tier's job. Checks that only re-read the script are Echo, and not counted.

**Authoring size (estimate).**

| Part | Lines of Rust |
|---|---|
| Harness core (runner, driver, project, probe, report, consequences, metrics) | 2,500–3,500 |
| Fake, DSL, responders | 700–900 |
| Sites and Harbor generator, staleness grader | 900–1,200 (L tier and selftest) |
| Site D (9 steps, several with P2 and P3 branches, three responders and expected sets) | 500–800 |
| SQLite D subset (9 steps) | 400–600 |
| Lifecycle (15 scenarios; L5, L7 and L8 run 4–8 sessions each) | 900–1,300 |
| HN smoke | ~50 |
| **Total** | **~6–8.5k**, of which scripts are **about 2–3k** |

A phase PR typically touches 200–500 lines of scripts and expectations. That is accepted
complexity: the scripts encode the contract under test.

### 5.2 L tier: live models

- **Gate.** All of: `APILA_LIVE=1`; `--tier live`; a key from `OPENROUTER_API_KEY` or
  `test_env/config.json`.
- **Models.** `APILA_LIVE_MODEL` defaults to `anthropic/claude-haiku-4.5`;
  `APILA_LIVE_WRITER_MODEL` defaults to `anthropic/claude-haiku-4.5`;
  `APILA_LIVE_EMBEDDING_MODEL` defaults to `openai/text-embedding-3-small`. Validator,
  compat and consolidator are unset, so they resolve to the writer model, as in production.
  A 404 for a model id is an `Error` naming the variable to set. Choosing a Sonnet-class
  agent prints a cap warning (§5.3).
- **The RecordingProxy.** `openrouter_base_url` points at a local proxy that forwards to
  OpenRouter with `reqwest::blocking`, and never logs headers. Attribution:

  | Component | How it is recognised |
  |---|---|
  | validator, compat | forced `tool_choice.function.name` |
  | consolidator | the tools offered include consolidator-only tools |
  | writer | `save_skill` offered, without consolidator tools |
  | agent vs cold agent | session time window. `openrouter_base_url` is project-level, and agent sessions never overlap: the memory, cold and all probe arms run **strictly one after another**. Probe forks each get their own proxy listener on a fresh port, which also separates them. Only writer jobs overlap agent sessions (Q12o), and the request shape tells those apart. |
  | embeddings | path |

  It gives two hard checks:
  - each session's `session_spend()` equals the wire sum for that agent and window;
  - `MemoryView::memory_spend()` equals the wire sum of the non-agent components
    (embeddings included from Phase 4, M§6.2).
- **The memory and cold agents get byte-identical `SYSTEM.md` and `AGENTS.md`**, with no
  memory prompting. The memory agent's extra tool schemas are reported as "tool overhead
  tokens per request".
- **Which steps run.** `--until-phase-checks` (the default) stops an arc after its last step
  with an active **DL or Behaviour** check. `--full-arc` runs everything.
- **Repeats** run in parallel with `--parallel N` (each repeat has its own project and
  ports).

### 5.3 Statistics, budget, cost (L)

**Statistics.**

- `--repeat N`: N = 5 is recommended for claims; N = 1 is a smoke run.
- Soft checks pass in at least `ceil(q·N)` runs (q defaults to 0.6), and print the rate.
- Single-step efficiency ratios carry a median and `[min..max]`. They are `INCONCLUSIVE`
  when the cold-vs-cold spread (H1/H1b, S1/S1b, Q2/Q2b) is wider than the margin.
- **The SQLite probes use a hierarchical analysis** (§9.3, measure 3).
  - The independent unit is the **repeat**: all probes in a repeat share one arc history.
  - The k runs in an arm are averaged before pairing. k reduces noise within a cell; it
    never adds samples.
  - The primary test is an **exact one-sided sign-flip (permutation) test over the n
    per-repeat mean differences** (§9.3). Its smallest possible p is 1/2^n: 0.125 at n = 3,
    0.0625 at n = 4, 0.031 at n = 5. So it **cannot pass below 4 repeats**, and every pooled
    claim is `INCONCLUSIVE (n<4)` there, with descriptive numbers only.
  - **Expected power, plainly.** Expect `INCONCLUSIVE` except at n ≥ 5 with large effects.
    The correctness claim mainly detects **silent-wrong** drift (Q9p soft deletes, perhaps
    Q6p returns), where stale skills give confidently wrong answers. **Loud** drift (renames,
    splits) makes the stale agent hit an error and recover, so its answer is still correct.
    The difference then shows in the iterations ratio and the first-query error rate, which
    are reported but are not decisive. n = 5 detects mainly large effects; small ones need
    n ≥ 8.
  - Next to every primary result the report prints the **minimum needed to pass**, for
    example at n = 5: "passes when ≤ 3 of the 32 sign patterns have a sum ≥ the observed one
    (ties counted against)".

**Budget.**

- **Per-arc caps apply per repeat:** `APILA_BUDGET_HN` $3, `APILA_BUDGET_SITE` $10,
  `APILA_BUDGET_SQLITE` $25. An arc over its cap marks the rest of its steps in that repeat
  `SKIP (budget)`.
- **`APILA_LIVE_BUDGET_USD` applies per run**, summed over all repeats (default $40). When
  it is reached, no new session starts in any repeat; running sessions finish.
- The projected cost is printed before starting, and `--dry-run` prints only the plan.
- **The recommended statistics run does not fit the default run cap.** `--repeat 5` on Haiku
  projects to about $40–145. The runner (and `--dry-run`) then prints "projection $X exceeds
  APILA_LIVE_BUDGET_USD=$40; set it to at least $Y for --repeat 5", and refuses to start
  unless the cap is raised or `--allow-truncate` is passed. A statistics run is therefore
  never cut off silently.
- With a Sonnet-class agent the projection exceeds the per-arc caps too, and the runner
  names which caps to raise.

**Cost per session**, assuming no prompt caching (apila sends no cache-control hints):

- Agent: about 15 requests, context growing from about 4k to 25k tokens, so about 180k input
  and 6k output tokens. That is about $0.21 on Haiku 4.5 ($1/$5 per M) and about $0.63 on
  Sonnet 4.5 ($3/$15 per M), with a range of 0.4–2.5× depending on exploration.
- Writer analysis on Haiku: $0.05–0.35.
- A memory session costs $0.25–0.55 on Haiku. A cold session or a probe (never analysed)
  costs $0.08–0.50.

| Arc (1 repeat) | Memory sessions | Cold sessions | Probe sessions (k=1) | Haiku agent | Sonnet agent | Serial time |
|---|---|---|---|---|---|---|
| HN | 2 (H2, H3) | 3 (H1, H1b, H4) | — | $0.7–2 | $2–5 | 5–10 min |
| Site | 8 (S1–S7, S9) | 4 (S1, S1b, S2, S3) | — | $2.3–6.4 | $6–17 | 30–50 min |
| SQLite | 13 (Q1–Q11, Q11f, Q12o) | 4 (Q2, Q2b, Q3, Q11) | 24 (7 task probes + Q8d, × 3 arms) | $5–22 | $14–58 | 1.5–3 h |
| **Full** | 23 | 11 | 24 | **$8–30** | **$22–80** | **2–4 h** |

`--repeat 5 --parallel 5` takes about the same wall time as one repeat, at five times the
cost.

---

## 6. Assertions

### 6.1 The check model

```rust
pub enum Kind  { Mechanic, Behaviour, Echo }
pub enum Scope { D, DL }                    // D: presupposes the script; DL: holds for any model
pub enum Status { Pass, Fail, Warn, Inconclusive, Skip(SkipWhy), Error(String) }

pub struct Check {
    pub id: &'static str, pub memory_ref: &'static str,
    pub kind: Kind, pub scope: Scope, pub since: Phase, pub until: Option<Phase>,
    pub obs: Obs,                 // PostAnalysis | PostMaintenance | History | Session
    pub severity: Severity, pub what: String, pub detail: Option<String>,
    pub state_breaking: bool,     // a Hard FAIL of this check cascades along `needs`
}

pub struct Step {
    pub id: StepId, pub tiers: Tiers,            // D | L | DL
    pub needs: Vec<Need>,                        // All(id) | Any(ids) | Optional(id)
    pub runs_since: Phase,                       // D only
    pub obs: Obs, pub fires: Fires, pub checks: Vec<Check>,
}
```

- **Mechanic:** what memory stored, served, gated, called or charged. **Behaviour:** what a
  real model chose (L only). **Echo:** dictated by the script; shown dimmed with
  `--verbose`; never counted.
- **Exit codes:** a Hard failure gives `FAIL` (exit 1). A Soft failure gives `WARN` (exit 0,
  or 1 with `--strict`). `INCONCLUSIVE` never fails the run. Infrastructure problems give
  `Error` (exit 2).
- **Steps declare `needs`.**
  - Only a `SKIP`, an `Error`, or a Hard `FAIL` of a check marked `state_breaking`
    cascades. Examples of `state_breaking`: the step's session never analysed; the expected
    skill is missing. Every cascade follows the `needs` edges only.
  - A `WARN`, an `INCONCLUSIVE`, or an ordinary Hard `FAIL` (for example, a wrong pin
    state) is reported, and the dependent steps still run.
  - **An edge to a step excluded by the current tier is ignored.** For example, in D,
    H2's `needs H0` (H0 is L only) does not apply.
  - **`needs A or B`** is satisfied when either ran without cascading (for example S5's
    `needs S4 or S4L`). **`needs A?`** marks an optional edge: it orders the steps, but the
    step still runs if A was skipped (for example Q6's `needs Q3, Q4r?` in D).
- **`runs_since: Phase`** is per step, apart from each check's `since`. It is the phase from
  which the step's **script** can run (the writer tools it uses exist). **`runs_since` applies
  to D only.** In L a step always runs, and only its checks are phase-gated (`since`). Which L
  steps run is set by `--until-phase-checks` / `--full-arc`.
  - Below `runs_since` the step is `SKIP (phase N)`. That skip **does not cascade**: steps
    that `need` it run if their own `runs_since` allows. They lose only the state the
    skipped step would have made, and their checks that depend on that state carry the
    same `since`.
  - A step that spans phases (S3, S9, Q2) declares **per-phase script variants**, for
    example Q2 before P2 with no `depends_on`, and from P2 with the edge.
- **A step whose only active checks are invariants** prints `PASS (invariants only)`, so it
  is not mistaken for coverage.
- **An expected set a step does not declare defaults to ∅.** A step that does not mention
  compat expects no compat calls.

In the step tables, checks are tagged **M·D** (Mechanic, D only), **M·DL** (Mechanic in both
tiers), or **B** (Behaviour, L).

### 6.2 What is checked, by source

| Source | Read through | Examples |
|---|---|---|
| Store state | `MemoryProbe` (read-only SQL; v3 and v4 helpers, each returning `None` when its table does not exist) | revisions, pins by changeset, feedback status, verdicts, job counts |
| The crate's metrics | `skill_metrics`, `skill_history`, `LibraryCounts` once they exist | cross-checked against the probe's raw count |
| Served text | `FromFn` in D; the transcript in L | `rev 2`; the exact `for:` call; "not compatible … yet"; the deprecation banner |
| Non-agent calls | the expected sets in D; the proxy in L | validated drafts; checked pairs; reviewed clusters |
| Task results | `ANSWER: {json}`; the agent's output databases | graded against ground truth (B) |
| Skill content | current bodies | SQLite coverage (B) |
| Spend | `SessionRun`, `memory_view`, job tables, the proxy | exact in D; wire equals app in L |

### 6.3 Invariants after every step (all tiers, hard, M·DL)

1. After the barrier, no session is `analysing` and no job row is `running`.
2. Revisions are append-only, contiguous, with `parent_id` correct (row diff against the
   previous snapshot). (P0)
3. Pins are append-only. Memo rows change only by upsert with source `repin`, `confirm` or
   `acknowledge`. Every edge of every revision has a pin on a revision of the edge's skill or
   its merge survivor. (P2)
4. The graph is acyclic, and its depth is within `max_depth` or grandfathered. (P2)
5. `merged_into` chains are one hop long. (P3)
6. **Live accounting:** `MemoryView::memory_spend()` (`jobs_base + jobs_live`) equals the
   probe's sum of spend over `memory_jobs` (and `maintenance_jobs` from P2). This tests the
   live status plumbing against the stored rows. It does not compare one SQL sum with
   another.

   It is evaluated only when `MemoryView.loaded` is true. Otherwise it reports
   `SKIP (not loaded)`. After a `restart()` without a `DIRECTIVE.md`, `jobs_base` is read
   only when the agent thread handles its first `Run` (`load_totals`), so it is not loaded
   until the next session's first message.
7. `MemoryView.message` holds no error, unless the step expects one.

### 6.4 Consequence checks (M·DL)

These are Mechanic checks over whatever the model did. They are how L tier checks mechanics
without presupposing a script, and D runs them too.

**They are delta checks.** At each barrier, a check considers only what changed since the
previous barrier, and the state as it was at the commit that caused each change. It never
re-judges old rows against today's state. The **delta** has two parts:

- **new rows**: ids above the previous snapshot's maximum per table. `skill_outcomes` and
  `skill_compat_checks` have no `id` column, so for them the delta is a **row diff** against
  the previous snapshot, keyed by their primary keys;
- **changed rows**: the row diff `probe.rs` already makes between snapshots. This catches
  status updates (feedback `addressed` or `closed`, skill `status`, `merged_into`,
  `replaced_by`) and memo upserts, which add no new id.

"As of changeset K" is rebuilt from ids: for each skill, its newest revision with an id
below K's first revision id; pins ordered by id. Skill status and `merged_into` come from
the **previous barrier's snapshot**, because `skills` keeps no history (M§5.1).

| # | Antecedent (rows in the delta) | Consequence | Phase |
|---|---|---|---|
| C1 | each new revision D@r with `breaking = 1`, in changeset K | Let S be the **active** dependents X (status from the previous snapshot; M§7.3 propagates to active dependents only) whose current revision **as of K** had an edge resolving to D (through `merged_into` as of the previous snapshot) with a pin below r, **excluding** any X that K itself revised to drop, add or confirm that edge. For each X@k in S: a `dep:<D>:<r>` row exists on X@k, and no pin row on X@k's edge reaches ≥ r except one with `how = 'confirmed'`. A later revision of X is not judged by C1. | P2 |
| C2 | each new revision committed by job J that, **as of its changeset**, (i) addresses feedback **not created by J itself** (for an analysis job: not `source='writer'` with `session_id` = J's session; for a maintenance job: id ≤ the maximum feedback id at J's start, its watermark; M§6.5 rule 4), or (ii) targets a skill with dependents, or (iii) is consolidator-authored | a validator request **naming that draft's skill** exists among J's requests. Requests are matched by content (the skill name and draft body in the validator input), not by time window, because an analysis job and the maintenance pass after it share the writer thread and whole-second timestamps. In D: exactly one per such draft, from the expected set. | P1 / P2 / P3 |
| C3 | each session analysed in the delta with verdict `failed` or `unclear` | it is the `source_session` of no revision and no new skill | P1 |
| C4 | each pin row appended in the delta with `how = 'compat'` | the memo row for its pair is `compatible`; or it is `source='repin'` with `maintenance_job` ≥ the pin's (a repin later in the same pass overwrote it) | P2 |
| C5 | each feedback row whose status changed to `addressed` (row diff) | `resolved_by` is set, and is a revision of the feedback's skill. Rows reopened by a revert are exempt (open question O5). | P1 |
| C6 | each successful `get_skill` in the step's own transcripts | a `skill_uses` row with the revision its served header names | P1 |
| C7 | each (session, skill) pair in the delta's analysed sessions with at least one `via = 'get'` use | exactly one writer outcome for that pair, on the **last** revision the session read (M§6.3) | P1 |
| C8 | each revision with a new pin row, or a new or changed `dep:` feedback row (row diff) | no open `dep:` feedback on it for an edge whose pin is Current, or for an edge it no longer has | P2 |
| C9 | each `search_skills` result in the step's own transcripts | it lists no skill that was `merged` in the **previous barrier's snapshot**, and no `deprecated` skill whose replacement chain was intact in that snapshot. A skill whose status differs between the previous and current snapshots is exempt, since its change may have landed mid-session (overlap steps). | P3 |

**Vacuity is reported, never hidden.**

- Each check prints its antecedent count, for example `C1 ×2`. With zero applicable rows it
  prints **`N/A`**, not `PASS`.
- The arc summary has a **consequence coverage** row: `C1 applicable in 2/12 steps · C2
  5/11 · C4 0/11 …`.
- When every phase-defining check of a phase (P2: C1, C4, C8) is `N/A` across the arc, the
  summary says, in plain words, "dependency handling was not exercised in this run; no P2
  behaviour claim". The run is not failed, because a model choosing not to declare
  dependencies is a behaviour, but it is flagged with `WARN`.
- Behaviour checks that make these features happen in L are listed in §9.2 (S6) and §9.3
  (Q2, Q3, Q4).

**Role finders (L).** Skills are located by role, never by a D-tier name. The report prints
which skill each finder picked.

| Role | Finder |
|---|---|
| catalog scraper | active skill whose body names `/catalog` and whose `when_to_use` mentions the catalog; ties broken by most recent use |
| pagination | active skill with the most dependents among those whose body names `next` or `cursor`; none if no skill has dependents |
| overview | active skill with the most `schema_facts` hits (qualified names) at the current version |
| monthly / region / segment analysis | active skill read in the step that first ran that task, otherwise the best `when_to_use` match on the task text (HashingEmbedder, which needs no API) |

---

## 7. Pretty-printed report

### 7.1 Where it goes

- **stdout**, directly. ANSI colour only on a terminal, and never when `NO_COLOR` is set.
- `$CARGO_TARGET_TMPDIR/memory-suite/<run-id>/report.txt` and `report.json` are always
  kept at the run root.
- `--compare <old report.json>` prints the change in metrics.
- Tables are drawn with `unicode-width`.

### 7.2 Per step (D example)

```
━━ Site · S3 · drift v2: class names renamed ━━━━━━━━━━━━━━━━━━━━━━━━━━━ tier D · phase 1
 needs S2 · obs post-analysis · fires —
 ─ checks ────────────────────────────────────────────────────────────────────────────────
 PASS  M·DL S3.served-rev1     get_skill served scrape-driftwood-catalog rev 1   M§6.3
 PASS  M·DL S3.use-recorded    skill_uses(session 3, rev 1, via=get)             M§6.3
 PASS  M·D  S3.agent-issue     feedback #1 outdated, source=agent, on rev 1      M§6.4
 PASS  M·D  S3.rev2            rev 2 parent=rev 1, author=writer, addresses #1   M§6.7
 PASS  M·D  S3.validated       {scrape-driftwood-catalog} (addresses → exempt)   M§6.5 r4, M§6.6
 PASS  M·D  S3.accounting      agent 5/5 · writer 4/4 · embeddings 2 (derived)
 PASS  M·DL consequences C2 ×1 · C5 ×1 · C6 ×1 · C7 ×1 · C1 C4 C8 C9 N/A · invariants (7)
 SKIP  M·DL S3.hint-shown      (phase 4)                                         M§6.2
 ─ metrics ───────────────────────────────────────────────────────────────────────────────
 agent    iterations 5 · tool calls 4 (search 1, get 1, report 1) · 90 tok · $0.0005
 writer   job #3 done · +0 new, 1 revised, 1 outcome · 72 tok · $0.0004 · validator 1
 library  1 skill · 2 revisions · 0 open issues
```

### 7.3 End of arc (L example, SQLite)

```
══ SQLite · summary ═══════════════════════════════════════════════════ tier L · ×5 · phase 2
 step  env   result  M·DL P/F  B P/W/I  iters(med)  cost(med)  skills revs pins d/b/k
 Q1    v1    PASS    3/0       2/0/0    14          $0.24      1      1    –
 …
 ─ understanding ─────────────────────────────────────────────────────────────────────────
 Q1  data-model grade (cold-equivalent): tables 8/8 · pks 8/8 · fks 7/8 · units 3/3 · traps 1/2
 Q8d describe-current (v6), median [range] over 5 repeats:  updated 0.93 [0.86..0.97] · stale 0.71 [0.60..0.82] · cold 0.80 [0.74..0.88]
 coverage of overview: v2 31/31 · v3 32/32 · v4 36/36 · v5 36/37 (stale: customers.region) …
 ─ probes: 7 task probes × 5 repeats (unit = repeat), k=1, censored 2/105 ───────────────
                           updated vs stale                 updated vs cold
 correct first ANSWER *    0.88 vs 0.60 · sign-flip p=0.031 0.88 vs 0.68 · sign-flip p=0.094
   d_r per repeat          +.29 +.43 +.14 +.29 +.29         +.29 +.14 +.43 −.14 +.29
   to pass (α=0.1, n=5)    ≤3 of 32 sign patterns ≥ observed (here 1)   (here 3)
 iterations ratio          0.71 [0.60..0.86] hier. 90% CI   0.58 [0.48..0.70]
 first-query error rate    0.05 vs 0.38                     0.05 vs 0.45
 * primary
 ─ library staleness (all active skills, after each step) ───────────────────────────────
 Q4  stale 1 in 1 (region: orders.total)   ← read? no
 Q5  stale 0 · Q7  stale 1 in 1 (segment: customers.region) ← read? no · dup overviews: none
 ─ consequence coverage ─────────────────────────────────────────────────────────────────
 C1 applicable 3/12 steps · C2 6/12 · C4 2/12 · C5 5/12 · C8 4/12 · edges to overview 4 skills
 ─ memory spend ──────────────────────────────────────────────────────────────────────────
 analysis $2.10 (11 jobs) · validator $0.31 · compat $0.12 · consolidate $0.44
 tool overhead 410 tok/request · cold-vs-cold spread ±0.18
 ═ 0 failed · 2 warnings · 1 inconclusive · 4 skipped (phase 3) ══════════════════════════
```

### 7.4 A whole arc as a user sees it

**D tier, Site arc at P1** (one failing step expanded; the passing steps print one line
unless `--verbose`):

```
━━ Site ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ tier D · phase 1
 S1  PASS                     6 mech · 0 skipped        agent 4 it · writer 3 req
 S2  PASS                     5 mech · 2 skipped (P2)   agent 4 it · writer 3 req
 S3  PASS                     9 mech · 1 skipped (P4)   agent 5 it · writer 4 req · val 1
 S4  PASS                     7 mech                    agent 3 it · writer 6 req · val 0
 S5  FAIL                     6 mech · 1 failed         agent 4 it · writer 5 req · val 2
 ┌─ S5 · drift v3: cursor pagination ── needs S4 or S4L · obs post-analysis · fires —
 │ FAIL  M·D  S5.validated   expected {scrape-driftwood-catalog@draft2}
 │                           got      {scrape-driftwood-catalog@draft1, …@draft2}
 │                           → the validator ran on the rejected shrink draft too (A3: the
 │                             deterministic rules must run before the validator)
 │                           conversation: …/site/S5/validator-1.md, validator-2.md
 │ PASS  M·D  S5.rev3        rev 3 addresses #4                            M§6.7
 │ PASS  M·DL consequences   C2 ×1 · C5 ×1 · C6 ×1 · C7 ×1 · C1 C4 C8 C9 N/A
 └─ artifacts kept: $CARGO_TARGET_TMPDIR/memory-suite/2026-10-22T14-03-11/site/S5/
 S6  SKIP (phase 2)           runs_since P2
 S7  SKIP (phase 2)           runs_since P2
 S8  SKIP (phase 2)           runs_since P2
 S9  SKIP (phase 2)           runs_since P2
══ Site · summary ═══════════════════════════════════════════════════════════════════════
 mech 32 passed · 1 failed · 4 steps skipped (phase 2) · echo 11 (not counted)
 consequence coverage: C2 3/5 steps · C5 2/5 · C6 5/5 · C7 4/5 · C1 C4 C8 C9 n/a (phase)
 spend exact: agent $0.0021 · writer $0.0105 · validator $0.0021 · embeddings 9 calls
```

**SQLite D at P0**, where Q2's checks are all P2 or B:

```
 Q2  PASS (invariants only)   0 mech active · 3 skipped (P2) · 2 B (L only)
```

**An L per-step block** (aggregated over `--repeat 5`):

```
━━ Site · S6 · drift v4: authors section ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ tier L · ×5 · P2
 PASS  M·DL consequences    C1 ×0 N/A · C2 ×6 · C6 ×14 · C7 ×9 · C8 ×0 N/A    5/5 repeats
 WARN  B   S6.shared-pagination  a skill with ≥ 2 dependents     2/5 (needs 3)  failed: r2 r4 r5
 PASS  B   S6.answer             authors 15/15, catalog 60/60    5/5
 INCONCLUSIVE B S6.efficiency    iterations 0.81× cold [0.58..1.12]; cold-vs-cold ±0.30
 metrics (median [min..max])  iterations 18 [14..27] · cost $0.31 [0.22..0.48] · validator 1 [1..2]
```

**An `INCONCLUSIVE` probe section.** This is what most runs show:

```
 ─ probes: 7 task probes × 3 repeats (unit = repeat), k=1, censored 1/63 ────────────────
                           updated vs stale                 updated vs cold
 correct first ANSWER *    0.81 vs 0.67 · INCONCLUSIVE (n<4: smallest possible p = 0.125)
   d_r per repeat          +.14 +.29 0.00                   +.29 +.14 +.14
 iterations ratio          0.77 [0.55..1.04] hier. 90% CI   0.61 [0.49..0.80]
 note: correctness mainly detects silent-wrong drift; loud drift shows in iterations
```

**The no-P2-claim line**, when C1, C4 and C8 are `N/A` across an arc:

```
 WARN  dependency handling was not exercised in this run (C1, C4, C8 n/a in 12/12 steps);
       no P2 behaviour claim. The model declared no dependencies (edges to overview: 0).
```

**Repeats.** `--repeat N` prints **one aggregated block per step**:
- each Mechanic check shows its pass count over repeats, and lists failing repeats by
  index (`failed: r2 r4`);
- B checks show their rate against `ceil(q·N)`;
- metrics show median and range;
- each repeat's full per-step blocks are written to `<run>/repeat-<i>/report.txt`.

**`report.json` shape** (what `--compare` reads):

```json
{
  "run_id": "2026-10-22T14-03-11", "tier": "L", "phase": 2, "repeats": 5,
  "models": {"agent": "…", "writer": "…", "embedding": "…"},
  "arcs": {
    "site": {
      "steps": {
        "S6": {
          "status": "WARN",
          "checks": {"S6.shared-pagination": {"kind": "B", "scope": "DL", "tier": "L", "status": "WARN",
                       "rate": [2, 5], "needed": 3, "failed_repeats": [2, 4, 5]}},
          "consequences": {"C2": {"applicable": 6, "status": "PASS"}, "C1": {"applicable": 0, "status": "N/A"}},
          "metrics": {"iterations": {"median": 18, "min": 14, "max": 27}, "cost_usd": {"median": 0.31}}
        }
      },
      "summary": {"mech": {"pass": 40, "fail": 0}, "b": {"pass": 9, "warn": 2, "inconclusive": 1}}
    },
    "sqlite": {"probes": {"updated_vs_stale": {"d_r": [0.29, 0.43, 0.14, 0.29, 0.29], "p": 0.031,
                  "status": "PASS", "iterations_ratio": {"est": 0.71, "ci90": [0.60, 0.86]}}}}
  },
  "spend": {"total_usd": 61.2, "by_component": {"agent": 38.0, "writer": 15.1, "validator": 3.9}}
}
```

`--compare` matches by `arc → step → check id`, and prints status changes and metric
ratios.

### 7.5 Metrics

| Metric | Source |
|---|---|
| Iterations, tool calls by name, exploration calls (`outline`, `schema`, `list`), first-query errors | `SessionRun` |
| Agent tokens and cost | `session_spend()`, the proxy |
| Writer job counts and spend | `memory_jobs` |
| Validator, compat and consolidator calls and spend | expected sets (D), the proxy (L), `maintenance_jobs` |
| Skill reads, searches, served bytes | tool calls |
| Usage, success, miss, precision and unknown rates | `skill_metrics`, cross-checked |
| Revisions, open issues, pins due, blocked and kept | probe, `LibraryCounts` |
| Data-model grade, describe-current grade, overview coverage | SQLite (§9.3) |
| Pooled probe statistics | SQLite (§9.3) |
| Tool overhead, cold-vs-cold spread | L |

---

## 8. Isolation, copies, artifacts, secrets

- **Run root:** `$CARGO_TARGET_TMPDIR/memory-suite/<run-id>/`. Each arc and scenario gets
  `<arc>/project/`, holding:
  - `config.json`;
  - a shared `SYSTEM.md` (with the ANSWER protocol);
  - `SKILL_MEMORY.md` per arc;
  - `<agent>/` and `<agent>-cold/`, with byte-identical `AGENTS.md`.

  Each arc has its own `Runtime`, fake or proxy, and site, on ephemeral ports.
- **`AgentSpec`** writes the M§11 blocks, and leaves out blocks whose phase has not been
  reached (unknown fields are refused).
- **Copying `memory.db`.** For step artifacts, `--state` resumes, and probe forks, copies
  are taken **after the barrier and with every agent settled**. They copy `memory.db`
  **together with `memory.db-wal` and `memory.db-log`**, if present, so WAL contents are not
  lost. `selftest` checks that a copied store opens and lists the same skills.
- **`project.fork(name, delta)`:**
  1. copy the project directory as above into `<arc>/forks/<name>/`;
  2. apply `delta` (config, databases version);
  3. return a fresh `TestProject` for a throwaway `Runtime`.

  Forks are used by probes (§9.3).
- **Per step:**
  - `transcript.md`;
  - one markdown file per non-agent conversation (`writer.md`, `validator-*.md`,
    `compat-*.md`, `consolidator-*.md`);
  - `calls.jsonl`;
  - the `memory.db` copy;
  - `library.md`;
  - `checks.txt`.
- **Cleanup.** Passing arcs are deleted unless `--keep`. Failing and warning arcs are kept,
  with their path printed. The last 5 runs are kept.
- **Secrets.** Kept `config.json` files have the key replaced with `<redacted>`. The proxy
  never logs headers.

---

## 9. The arcs

**Step columns:**

- **tiers**: `D`, `L` or `DL`.
- **needs**: upstream steps.
- **obs**: D-tier observation point.
- **fires**: the D-tier consolidation trigger.

In L tier each step runs with the production config, and the consequence checks C1–C9 run
after it. **The task protocol** (`AGENTS.md`): finish with `ANSWER: <json>`.

### 9.1 HN arc

**Environment.**

- **Live:** default `fetch_webpage` settings.
- **H0 preflight:** the harness parses the front page itself (`tr.athing`,
  `span.titleline > a`, `span.score`, `a.morelink`).
- **Offline fallback:** synthetic HN-shaped pages, served locally with
  `allow_private_hosts: true`. The report shows `WARN H0.live-unreachable`.
  `APILA_HN=live` makes unreachability an `Error`; `APILA_HN=offline` forces the fallback.
- No live HN content is checked in.
- **Tolerance:**
  - at least 25 rows, non-empty titles and URLs, integer points;
  - at least 80 % title overlap with a reference parse taken straight after the session
    (the better of two parses);
  - fewer than 20 HN requests a run.

**Agents:** `hn` and `hn-cold`, with `fetch_webpage`, `read_webpage_data` and `sqlite`.

| Step | tiers | needs | Task | Expected |
|---|---|---|---|---|
| H0 | L | — | preflight | `PASS`, or `WARN` and the offline page |
| H1, H1b | L | H0 | **cold**, ×2: "Fetch the HN front page; store the top 30 stories (rank, title, url, points, comments) in `hn.stories`. ANSWER `{count, top_title}`." | H1b gives the noise estimate. B: answers correct. |
| H2 | DL | H0 (L only; ignored in D) | **memory**, session 1: the same task | M·DL: `analysed`; job done with spend > 0; at least 1 skill; from P0, rev 1 `author='writer'` with `source_session`. B: body names `hn` and an HN selector; answer correct. |
| H3 | DL | H2 | **memory**, session 2: "Refresh `hn` with today's front page and pages 2–3." | B: `search_skills` then `get_skill` before the first fetch. M·DL (P1): C6; B (P1): writer outcome `worked`. |
| H4 | L | H1, H3 | **cold**: the H3 task | B: H3 iterations ≤ 0.75 × H4 (median, spread, noise from H1/H1b). This is the headline A1 number. |

**D:** H2 and H3 with memory-only agent scripts, checking only the pipeline: analysed, skill
row, served body, exact spend, and (from P1) the use row. This is part of Slice 1.

### 9.2 Site arc: Driftwood Books

**Server (L tier and selftest).** One accept loop with a thread per connection, on
`127.0.0.1:0`. Responses are rendered from `(SiteVersion, path)`. `set_version` is called
only between sessions. The catalog has 60 books in 3 pages and 15 authors (L tier), from a
seeded `SplitMix64`.

| Version | Diff from the previous one |
|---|---|
| v1 | `div.book > h2.title, span.author, span.price`; `/catalog?page=N`; `a.next` |
| v2 | classes renamed: `article.product-card > h3.product-title, .byline, .amount` |
| v2x | `/catalog` answers 503 "maintenance" |
| v3 | cursor pagination `/catalog?after=<isbn>`, `a[rel=next]` |
| v4 | adds `/authors?after=` with the same pagination; isbn and stock move to `/book/<isbn>` |
| v5 | **breaking**: `button[data-next-cursor]` and `?cursor=` in both sections |
| v6 | **non-breaking** pagination extra: next links carry `&size=20` and a `rel=last` link appears; the old form still works |

**Agents:** `shop` and `shop-cold`. `fetch_webpage` has `{"allow_private_hosts": true}`.

| Step | tiers | needs | Change, task | obs | fires | Expected |
|---|---|---|---|---|---|---|
| S1 | DL | — | v1: "Load the full catalog (title, author, price_cents) into db `books`. ANSWER `{count, total_cents}`." L: plus a cold pair, and S1b cold. | post-analysis | — | M·DL: a skill exists (L: found by role). B: count correct. |
| S2 | DL | S1 | v1: "Which 5 books are cheapest?". L: plus a cold pair. | post-analysis | — | M·DL: served body is the skill S1 saved (D: `FromFn` reads the top search hit, then the served header). M·DL (P1): C6, C7; `last_worked_at` set after a `worked` outcome. B: skill read before the first fetch; iterations ≤ 0.75 × cold. |
| S3 | DL | S2 | **v2**: the S1 task. L: plus a cold pair. | post-analysis | — | **Until P0**, M·D: 1 skill, body replaced. **P0**, M·DL: a new revision, the old revision unchanged. **P1**, M·D: <br>• agent `report` creates feedback (vars `issue`) on rev 1; <br>• rev 2 `addresses=[issue]`; <br>• **validated = {catalog}**, because a draft that addresses earlier feedback is exempt, and every exempt draft is validated (M§6.5 rule 4, as written); <br>• outcome `partial`. <br>M·DL (P1): C2, C5. B: the new revision names the new selectors. |
| S4 | D | S3 | v2x (narrated): the S1 task, which fails | post-analysis | — | **P1** (needs **A2**). Writer: `report_issue(catalog, outdated)`, `record_outcome(catalog, failed)`, `revise(catalog)`, `end_turn(failed)` [rejected: rule 7], text [Done 1], text [Done 2: permissive]. M·D: writer requests 6; the permissive commit keeps the outcome and issue; `skill_changesets.dropped` names the revision; verdict `failed`; no new revision; validated = ∅ (deterministic rejection comes first, **A3**). M·DL: C3. |
| S4L | L | S3 | v2x: the S1 task | — | — | M·DL: C3. B: verdict `failed` or `unclear`. |
| S5 | DL | S4 or S4L | **v3**: the S1 task | post-analysis | — | **P1** (needs **A1**, **A3**). Agent: `report(catalog, partial, outdated)` → vars `issue2`. Writer: `outcome(catalog, partial)` → `revise(catalog, body 60 % shorter, no addresses)` → `end_turn(succeeded)` [rejected: shrink guard; no validator call] → `revise(catalog, the same body, addresses=[issue2])` [replaces the staged draft, **A1**] → `end_turn(succeeded)`. M·D: writer requests 5; validated = {catalog}; rev 3 addresses `issue2`. M·DL: C2, C5. |
| S6 | DL | S5 | **v4**: "Load all authors into db `authors`.", then the S1 task in the same session | post-analysis | — | **P2**, M·D: <br>• one changeset holds new `paginate-driftwood`@1, catalog@4 and new `scrape-driftwood-authors`@1, each with an edge, `[[paginate-driftwood]]` and a `declared` pin at @1; <br>• validated = {catalog} (extraction exemption); the validator request contains pagination's body. <br>B: a shared pagination skill with at least 2 dependents. |
| S7 | DL | S6 | **v5**: the S1 task; D: paused | post-analysis | — | **P2**, M·D: <br>• agent `follow_deps()` calls the printed `get_skill(paginate-driftwood, for: "scrape-driftwood-catalog@4")`, which serves @1 with `via='dependency'`; <br>• `report` on pagination (vars `issue3`); <br>• writer revises pagination@2 `breaking`, addressing `issue3`, plus catalog@5 `confirm`; <br>• validated = {paginate-driftwood, catalog}; <br>• catalog@5 pin `confirmed` at @2; <br>• authors@1 pin **Blocked**, with `dep:<pagination>:2` open; <br>• served `get_skill(authors)` shows pagination @1 with "rev 2 is not compatible with scrape-driftwood-authors yet". <br>M·DL: C1, C2, C8. |
| S8 | D | S7 | `restart()` on | post-maintenance | P2 —, **P3 b** | **P2**, M·D: compat_pairs = ∅ (the authors pair is blocked by a breaking range). **P3**, M·D: clusters = {{authors}} (`on_cluster_with(authors)` → `revise_confirm(pagination)`); validated = {authors}; authors@2 pin `confirmed`, Current; `dep:` closed; a consolidate job `done`, `revised=1`; watermark persisted. |
| S9 | DL | S8 (D), S7 (L) | **v6**: the S1 task; D: paused. Then S9r: `restart()` on. Then S9r2: `restart()` on. | post-analysis, then post-maintenance | S9r: P2 —, **P3 b**; S9r2: none | **P2/P3**, M·D (analysis): <br>• agent `follow_deps()` reads pagination through the printed `for:` call (a read is required before reporting, M§6.4), then reports `incomplete` on it; <br>• writer revises pagination@3, **non-breaking**, addressing it, **with no confirm on catalog**; <br>• validated = {pagination}; <br>• post-analysis: **catalog Due** (pin @2, target @3, no breaking revision in the range). Authors: at **P2** still pinned @1 with breaking @2 in range, so **Blocked**; at **P3** pinned @2 since S8, so **Due**. <br>S9r: <br>• compat responder: `on_pair(catalog, pagination) → Incompatible`, `on_pair(authors, pagination) → Compatible`; <br>• compat_pairs at **P2** = {(catalog, pagination)}, at **P3** = {(catalog, pagination), (authors, pagination)}; <br>• catalog stays at @2, **Blocked by memo**, `dep:<pagination>:3` open (C1-style, from compat); <br>• at P3 authors moves `how='compat'` (C4); <br>• at P3 the new `dep:` feedback fires (b) in the same pass: clusters = {{catalog}} → `acknowledge_block(catalog, pagination)`; `pins_kept = 1`. <br>S9r2: **compat_pairs = ∅** (a pair blocked by its memo is never re-checked, M§7.3); at P3, clusters = ∅ (acknowledged). |

L tier runs S1–S7 and S9 as sessions (S4L in place of S4). S8 and the restarts are D-only.

### 9.3 SQLite arc: Harbor

This arc carries the user's central request: **understand the data model, keep the skills
current as it changes, and benefit in later sessions**. It is mainly an L-tier arc. D covers
the mechanics that Site does not already cover.

**Domain.**

- `sales.db`:
  - `customers(id, name, email, region, created_at)`
  - `products(id, sku, name, category, unit_price_cents)`
  - `orders(id, customer_id, ordered_at, status, total)`, with `total` in dollars and a
    `cancelled` status
  - `order_items(order_id, product_id, qty, unit_price_cents)`
- `fulfillment.db`:
  - `warehouses(id, name, region)`
  - `carriers(id, name)`
  - `shipments(id, order_id, warehouse_id, carrier_id, shipped_at, delivered_at, promised_at)`
  - `shipment_events(shipment_id, at, kind)`

Size: about 400 customers, 120 products, 6 000 orders and 15 000 items over 2025.

**Generator (`data/harbor.rs`).**

```rust
pub fn logical(seed: u64) -> Logical
pub fn project(l: &Logical, v: SchemaVersion, dir: &Path) -> Result<(), HarnessError>
pub fn reference(l: &Logical, v: SchemaVersion, task: Task) -> serde_json::Value
pub fn schema_facts(v: SchemaVersion) -> SchemaFacts     // tables, qualified columns, pks, fks, units, traps
pub fn logical_digest(dir: &Path) -> Digest              // ordered rows per table, not file bytes
```

- **References come from the logical rows in Rust.** `selftest` cross-checks them against
  SQL over the projected files for every version, and checks `schema_facts` against
  `PRAGMA table_info`.
- **Before every session:**
  1. delete `<name>.db`, `-wal` and `-log`;
  2. project a fresh file;
  3. checkpoint and close it.
- **After every session:** compare `logical_digest` with the expected digest. A logical
  digest, not a byte digest, because opening a file can rewrite bytes. A difference is
  `WARN agent wrote to upstream db`, listing the `execute` and `drop` calls involved.
- `AGENTS.md`: "`sales` and `fulfillment` are refreshed from upstream nightly and are
  read-only; put your own tables in other databases."

**Versions.**

| Version | Change | Trap |
|---|---|---|
| Q-v1 | baseline | `total` in dollars; revenue excludes `cancelled` |
| Q-v2 | `orders.total` → `orders.total_cents` | rename plus unit change |
| Q-v3 | + `customers.segment` | new property |
| Q-v4 | + `returns(order_id, returned_at, amount_cents, reason)` | net revenue |
| Q-v5 | `region` moves to `customer_addresses(customer_id, region, valid_from)` | split; latest address |
| Q-v6 | `shipment_events` merged into `shipments` (`status`, `status_at`) | merge |
| Q-v7 | + `orders.deleted_at` (about 3 % soft-deleted) | silent wrong answers |
| Q-v8 | `fulfillment.db` → `logistics.db` | artifact staleness |
| Q-eu | + `sales_eu.db`: `clients`, `purchases.amount_eur`, `purchase_lines` | generalisation |

`SKILL_MEMORY.md` requires `## Tables used`, `## Query`, `## Gotchas` and `## Depends on`.

**Agents:** `analyst` and `analyst-cold`, both with `sqlite` only.

#### Measuring understanding and evolution (L, Behaviour)

1. **Q1 data-model grade.** The ANSWER is
   `{tables:[{name, pk, fks:[{column, references}]}], joins:[…], units:{…}, gotchas:[…]}`,
   graded field by field against `schema_facts(v1)`. This is a **cold-equivalent baseline**
   of the model's own understanding. It says nothing about memory.
2. **Overview coverage after every step.** The current overview body is matched against
   `schema_facts(v)` by **qualified names**: `table.column`, or a column on a line or in a
   section that names its table. Bare names such as `id` or `region` never count alone. A
   removed name counts as stale unless it appears in a "renamed from" or "was" phrase, which
   is matched by pattern. `selftest` checks the grader against
   `fixtures/memory_suite/coverage/*.md`, including the "renamed from" allowance. The report
   prints `covers 36/37; stale: customers.region`.
3. **Probes: forked leaves, three arms, pooled.** After each drift step's barrier, the
   harness runs a **probe**: a fresh task that needs the changed part of the model.

   **Why forks.** A probe never runs in the main project. If it did, its `/clear` would be
   analysed and change the library the next steps depend on, and its cost would be charged
   to the arc.

   **The arms.** Each arm is a `project.fork(..)` with the databases at the **current**
   version:

   | Arm | `memory.db` | Agent |
   |---|---|---|
   | **updated** | the copy after the drift step's barrier | `analyst` |
   | **stale** | the copy from **before** the drift step, kept as that step's predecessor artifact | `analyst` |
   | **cold** | none | `analyst-cold` |

   **How a fork runs.** Its config pauses maintenance (**A4**; before P2 there is none). The
   fork's `Runtime` runs `say(task)`, then is **dropped without clearing**, so nothing is
   analysed. The fork is then deleted. **k** runs per arm (`--probe-k`, default 1) are cheap,
   because forks share nothing.

   **Scrubbing the fork.** A fork's `Runtime::new` runs `recover_sessions`, and `recover`
   would make a copied `active` session pending and analyse it. In L that is an unbudgeted
   writer call that changes the arm's library during the probe. So `fork()`:
   - sets every `active` **and `pending`** session in the copied `memory.db` to **`ended`**
     (a writer job that failed in L leaves its session `pending`, and recovering it would be
     an unbudgeted writer call that changes the arm). It prints the count of scrubbed
     sessions per state in the probe's artifacts;
   - asserts that no job row is `running`;
   - only then builds the fork's `Runtime`.

   **Measures per run:**
   - correct first `ANSWER` (binary);
   - first-query error (binary: any sqlite error before the first successful `query`);
   - `schema` calls;
   - iterations.

   **Censored runs.** A run that hits `agent_max_iterations` (40), or ends `Failed`, is
   scored iterations = 40 and correct = false. Censored runs are counted per arm in the
   report.

   **The task probes pooled** are the seven: Q4p, Q5p, Q6p, Q7p, Q8p, Q9p, Q10p. Within a
   repeat, each probe gives one (updated, stale, cold) triple of arm means over k (with k > 1
   a cell is a fraction, such as 0.5).

   **Claims.** The repeat is the unit, and each claim has exactly one primary test.
   - **updated vs stale** ("adapted through updated skills"):
     - *statistic:* for each repeat r, d_r = the mean over the 7 probes of
       (updated − stale) correct-first-answer, using the k-averaged cells;
     - *primary test:* an **exact one-sided sign-flip permutation test** over the n values
       d_r, α = 0.1. It enumerates all 2^n sign patterns, and p is the share whose sum is
       **at least** the observed sum. **Ties count as ≥ observed**, including zero d_r
       (which flip to themselves). The test is computed in **integer numerators**: each
       d_r is kept as the integer Σ(correct counts updated − stale) over a common
       denominator (the number of probes kept × k), so equal sums compare exactly and no
       tie is lost to float rounding;
     - *runs that end in `Error`* (infrastructure, not censoring) are retried once. If the
       retry also errors, that probe is dropped from **all three arms in that repeat**, and
       that repeat's d_r averages over the remaining probes. The common denominator is then
       built per repeat. Dropped probes are listed in the report;
     - *secondary* (reported, not decisive): the iterations ratio (geometric mean) with a
       90 % **hierarchical bootstrap** CI (resample repeats, then probes within each
       repeat);
     - **`PASS`** when the primary p ≤ 0.1.
     - **`WARN`** (only with n ≥ 4) when the mirrored test (toward stale) has p ≤ 0.1, or
       the secondary CI lies entirely above 1. Both are significant evidence against the
       claim, not merely a point estimate.
     - **`INCONCLUSIVE`** otherwise, and always when n < 4.
     - The report prints each d_r, the p-value, and the minimum needed to pass (§5.3).
   - **updated vs cold** ("memory helps at all"): the same test, with d_r over
     (updated − cold).
   - First-query error rate and `schema` calls are descriptive.

   Per-probe numbers are descriptive only. Probes are **leaves** (`needs` the drift step;
   nothing needs them) and are **L only**.

   **The fork barrier.** Under A5 the fork's own recover job runs a maintenance pass with
   the sweep (compat and consolidation are paused). So after the fork's `Runtime::new`, the
   harness calls `wait_for_memory` **before** the probe's `say`. Any sweep rows (for
   example `artifact:` feedback) are kept as part of the arm, since the main project would
   sweep the same way on its next writer instance. They are listed in the probe's
   artifacts.
4. **Q8d, "describe the current data model" probe** (after Q-v6, three arms). The same
   structured ANSWER as Q1, graded against `schema_facts(v6)` into a score from 0 to 1.
   This grades understanding **through memory**. It is reported **on its own**, not pooled:
   the per-arm median and range over repeats.
   - **PASS** needs n ≥ 4 repeats, and **in every repeat** updated > stale and updated >
     cold. The median difference must also exceed the Q1 grade spread (max − min of the
     cold-equivalent Q1 grades over repeats), so noise cannot pass it.
   - **WARN** needs n ≥ 4, with stale or cold ≥ updated in at least half the repeats.
   - **INCONCLUSIVE** otherwise; descriptive only below n = 4.
5. **Library staleness after every step** (the whole library, not just the overview).
   - For each **active** skill's current revision, the coverage grader lists the qualified
     names it uses that do not exist in `schema_facts(v)` (minus "renamed from" phrases).
   - **SQL aliases are resolved.** Inside fenced code blocks and inline SQL, the grader
     reads `FROM` / `JOIN <table> [AS] <alias>` (and comma joins), so `SUM(o.total) FROM
     orders o` counts as `orders.total`. A bare column in a section that names two tables
     that both have it is counted as **ambiguous, never stale**. The `coverage/` fixtures
     include an aliased stale query, and the ambiguous case.
   - The report prints a per-step table:
     `stale facts: 3 in 2 skills (monthly: orders.total; region: customers.region, …)`.
   - **B soft check:** a skill **read in the step's session** that still contains stale
     facts after the step's analysis is a `WARN`. The writer saw the drift and did not fix
     the skill.
   - **Duplicate overviews:** two active skills that each cover more than 50 % of the
     current tables are reported as `near-duplicate overviews: A, B`. A `WARN` from P1,
     because the writer should find the existing overview with `find_skills` and revise it.
   - The table also covers skills that were not read, but those rows are descriptive only,
     because nothing obliged the writer to touch them.

#### Steps

| Step | tiers | needs | Change, task | obs | fires | Expected |
|---|---|---|---|---|---|---|
| Q1 | DL | — | Q-v1: "Describe the data model of `sales` and `fulfillment` …" (structured) | post-analysis | — | M·DL: at least 1 skill; the writer prompt contains the `SKILL_MEMORY.md` text. B: grade (measure 1); owner headings. |
| Q2 | DL | Q1 | Q-v1: "Monthly 2025 revenue (exclude cancelled)." L: plus a cold pair, and Q2b cold. | post-analysis | — | **P2**, M·D: monthly has an edge to the overview, with a `declared` pin. **B (P2): the monthly-role skill has an edge to the overview-role skill.** B: within 0.5 % every month; reads the overview first. |
| Q3 | DL | Q2 | Q-v1: "Revenue by region for Q4." L: plus a cold pair. | post-analysis | — | **P2**, M·D: region has an edge to the overview. **B (P2): the region-role skill has an edge to the overview-role skill.** B: `schema` calls memory ≤ 1, cold ≥ 2; iterations ≤ 0.7 × cold; answer within 0.5 %. |
| Q4 | DL | Q3 | **Q-v2**: the Q2 task; D: paused | post-analysis | — | **P2**, M·D: overview@2 `breaking`; monthly@2 `confirm`; validated = {overview, monthly}; region Blocked; `dep:` open; served `get_skill(region)` shows overview @1 with the note. M·DL: C1, C2, C8. **B (P2): the step commits an overview revision with `breaking = 1` (set by the model or raised by the validator) or a `confirm` onto one.** B: answer correct after drift; staleness table (measure 5): no stale facts in skills read this session. |
| Q4r | D | Q4 | `restart()` on | post-maintenance | P2 —, **P3 b** | **P2**, M·D: compat_pairs = ∅ (region's pair is blocked by a breaking range). **P3**, M·D: clusters = {{region}} → `revise_confirm(overview)`; validated = {region}; compat_pairs = ∅; pin Current; `dep:` closed. |
| Q4p | L | Q4 | probe: "Average order value per month in Q3 (exclude cancelled)." | — | — | Three arms (measure 3). |
| Q5 | L | Q4 | **Q-v3**: "Revenue by customer segment for 2025." | — | — | M·DL: C1, C4, C8. B: coverage includes `customers.segment`. |
| Q5p | L | Q5 | probe: "Wholesale share of revenue by region." | — | — | Three arms. |
| Q6 | DL | Q3, Q4r? (D); Q5 (L) | **Q-v4**: "Net monthly revenue after returns." | post-analysis | — | **P1**, M·D: the writer log shows `find_skills` before any write; a scripted `save_skill("harbor-monthly-revenue")` is rejected with "exists at rev N … `revise_skill`", which `FromFn` reads; then `revise` (no addresses, no dependents, not proven) gives validated = ∅. M·DL: C6. B: no near-duplicate skill, or a new one with an edge to monthly. |
| Q6p | L | Q6 | probe: "Return rate by product category." | — | — | Three arms. |
| Q7 | L | Q6 | **Q-v5**: the Q3 task | — | — | M·DL: C1, C2, C6, C8. B: answer correct (uses `customer_addresses`, latest address). B: if region was read at a stale pin, the transcript shows the printed `for:` call followed. |
| Q7p | L | Q7 | probe: "Customers per region who ordered in December." | — | — | Three arms; coverage has no stale `customers.region`. |
| Q8 | L | Q7 | **Q-v6**: "On-time delivery rate by carrier." | — | — | B: within 1 point. M·DL: C1, C8. |
| Q8p | L | Q8 | probe: "Median days to deliver per warehouse." | — | — | Three arms. |
| Q8d | L | Q8 | probe: describe the current data model | — | — | Measure 4. |
| Q9 | DL | Q6 (D), Q8 (L) | **Q-v7**: the Q2 task, then a second user turn: "These numbers are too high: soft-deleted orders (`deleted_at`) must be excluded." D: paused; then Q9r `restart()` on. | post-analysis, then post-maintenance | Q9r: **a** (threshold 1: `wrong` plus `failed`) | **P1**, M·D: writer `outcome(monthly, failed)`, `report_issue(monthly, wrong)`, verdict `partial`, **no revision** (so the issue stays open). **P3**, M·D (Q9r): candidates are monthly only, because region was moved at Q4r, nothing else depends on a changed revision, and no new `dep:` or regression feedback exists since the Q4r watermark. So clusters = {{monthly}} → revise addressing the issue; validated = {monthly}. M·DL: C2, C5. |
| Q9p | L | Q9 | probe: "Revenue for Nov–Dec 2025." | — | — | Three arms; the updated arm should be correct on its first ANSWER. |
| Q10 | DL | Q9 | **Q-v8**: the Q8 task (L) / an overview read (D). Then Q10r `restart()` on; Q10s `restart()` with `every_sessions = 1`; Q10t `restart()` on. | post-maintenance, history | Q10 session's pass: none; Q10r: none; Q10s: **d**; Q10t: none | **P3** (needs **A5**, **A6**), M·D: <br>• **D places real files**: from Q1 on (from P3; earlier phases may omit them), D places minimal Harbor v1 files (`databases/sales.db`, `databases/fulfillment.db`) from a tiny fixed `CREATE TABLE` fixture in the suite, not the Slice 3 generator, and the D overview body names both, so the overview revision's `artifacts` records them at acceptance (M§9.2). At Q10 the harness renames `fulfillment.db` to `logistics.db` before the session. <br>• Under A5 the sweep re-runs on the pass after Q9r's commit, which is the Q10 session's own pass. So the check is: **by Q10r, exactly one** `outdated` row with key `artifact:databases/fulfillment.db` exists, on the overview revision that recorded it. One row does not reach threshold 2, so (a) does not fire. <br>• Q10s: one analysed session since Q9r, with feedback open, fires (d). Clusters = {{overview}} → `dismiss_feedback`. <br>• Q10t: the sweep reopens nothing (dedupe key); (d) does not fire (no analysed session since Q10s). <br>B (L): the agent adapts to `logistics`. |
| Q10p | L | Q10 | probe: "Shipments per carrier in December." | — | — | Three arms. |
| Q11 | L | Q10 | **Q-eu**: "EU monthly revenue for 2025 in EUR." L: plus a cold pair. | — | — | B: within 0.5 %; the memory agent reads the overview and revenue skills; its schema calls are on `sales_eu` only. B: afterwards the overview covers `sales_eu`, or a new skill has an edge to it. M·DL: C1, C8. |
| Q11f | L | Q11 | A short trailing session, ended with `clear_no_wait`: "List the databases you can see." Q11 itself ended with a normal barrier, so **all Q11 checks were read before Q11f started**. | — | — | Only the overlap check in Q12o. |
| Q12o | L | Q11f | **Real-use overlap.** Starts straight after Q11f's `clear_no_wait`, so it runs during Q11f's analysis: "Top 5 products by 2025 revenue." Neither Q11f nor Q12o is probed. | — | — | M·DL: no `MemoryView.message` error from the overlap; C6 and C9 (with the overlap exemption); both sessions analysed after the barrier; the delta is attributed to "Q11f+Q12o". No efficiency check. |

D runs Q1, Q2, Q3, Q4, Q4r, Q6, Q9, Q9r and Q10, with memory-only agent scripts, and with minimal Harbor files placed for the artifact check. **Every** D overview revision's body names both Harbor files, so each later revision keeps them in `artifacts` (Q10). As in Site, **the restarts (Q4r, Q9r, Q10r, Q10s, Q10t) are D-only**. In L those rows run only their session part. Pins,
Blocked and Due are already shown in Site S6–S9. The other steps are L only.

### 9.4 Lifecycle arc (D only, independent scenarios)

Each scenario builds a fresh project and has no `needs` outside itself.

| ID | Scenario | Setup | fires | Expected (M·D) |
|---|---|---|---|---|
| L1 | Migration (P0) | load `memory_v3.sql`; `restart()` | — | ids, bodies and vectors intact; rev 1 `author='migration'`; `meta.skills_migrated_at`; search finds the same ids. |
| L2 | Recovery (now) | a session left open (no clear); `restart()` (recovery intended) | — | analysed by the next runtime; until P1, a skill saved. |
| L3 | Guidance change (P1, P3) | a proven skill; edit `SKILL_MEMORY.md`; `restart()`; a session that revises the skill for a real issue and drops a heading | — | accepted because `guidance_hash` differs; validated = {skill}. P3: a scripted restyle-only consolidator revision is rejected (evidence-free). |
| L4 | Re-embed (P3) | 2 skills; `restart()` with `embedding_model` changed | none | a `reembed` job row with rows added for the new model; search finds both afterwards. Checked from the job row. |
| L5 | Deprecation and reactivation (P3) | authors@1 with dependent `author-report` | see the walk-through | see the walk-through |
| L6 | Merge (P3) | 2 `writer_only_session`s save near-duplicates (embedder similarity ≥ 0.9); a third skill depends on the one to be absorbed; one agent `unclear` issue on a duplicate; `restart()` with `every_sessions = 1` | **d** | clusters include the duplicate pair; `merge_skills`; the absorbed skill `merged`; `get_skill(absorbed)` serves the survivor; the dependent is Due "moved to survivor" and is re-pinned `compat` in the next restart's pass (compat_pairs = {(dependent, survivor)}). |
| L7 | Regression and revert (P3) | X→D; 2 `worked` on X; D@2 confirmed into X (a forward move); 2 `failed` on X | **b** (the regression feedback is raised by the sweep earlier in the same pass) | `regression` feedback `pin:<id>`; `revert_skill`; pins `how='reverted'` at D@1; the reopened feedback is on the revert revision. L7b: `repin_dependency` instead, giving an `incompatible` memo (`repin`); a later pass has compat_pairs = ∅ for the pair; a repin to a never-pinned, unmemoised revision is rejected. |
| L8 | Acknowledged block (P3) | D gets a breaking revision; 4 dependents blocked; `restart()` | b, then none | `acknowledge_block` ×4; `pins_kept = 4`; 3 more `writer_only_session`s with `every_sessions = 1` give clusters = ∅; dismissing a `dep:` row without acknowledging is rejected (rule 12); D gets a newer revision, and the 4 return as Blocked. |
| L9 | Concurrency (P1) | X@1; session A ends with `clear_no_wait`; A's writer reply is `Gate(G1, revise X)`; session B starts at once | — | B: `get_skill(X)` serves @1 → the harness opens G1 and waits for X@2 in the store → opens **G2**, which holds B's next agent reply → B's `get_skill(X)` serves @2. Then: `skill_uses` has both revisions for B; B's **writer** outcome is on @2 (M§6.3); no `MemoryView.message` error. Variant: B's `report_skill_outcome` (gated) lands while A's commit runs, and both writes persist. |
| L10 | Faults and retry (P1) | the writer's 2nd reply is `Status(500)` | — | job `failed`; session `pending`, attempts 1; drafts discarded; `stalled()`. The retry happens on the next enqueue, here a `restart()`: analysed once, changes applied once. |
| L11 | Two agents, one writer (now) | `shop` clears with `clear_no_wait`; its first writer reply is `Gate(G1, …)`; once shop's job row is `running`, `shop-2` clears with `clear_no_wait`; then G1 opens. This fixes the enqueue order. Writer replies are also keyed by the transcript (`FromFn` checks which agent's session it is analysing), so a wrong order is reported, not a hang. | — | both analysed, shop first; with the distinct per-reply costs, each job's spend is on its own agent's `memory_spend()`, and the sums identify exactly which writer replies each job holds. |
| L12 | Consolidation failure and backoff (P3) | the consolidator reply is `Status(500)` on an (a)-triggering skill | **a** | job `failed`; watermark not advanced; the next run waits 2 analysed sessions (`writer_only_session`s), then succeeds and resets backoff. |
| L13 | Validator cache and rule order (P1) | proven skills A and B; one writer session revises both | — | **Attempt 0 (A3):** B's draft fails the shrink guard (deterministic) while A is gated and valid, so the validator makes 0 calls and the commit is rejected. The writer re-revises B (**A1**). Attempt 1: validator accepts A and rejects B (validated {A, B}). The writer re-revises B again. Attempt 2: validated adds only {B} (A is cached). 3 validator calls in all. |
| L14 | Agent report cap (P1) | 4 `report_skill_outcome` issues on X in one session | — | the 4th is refused; 3 feedback rows. |
| L15 | Expanded serving (P2) | a diamond A→B→D@1, A→C→D@2; small `max_expanded_bytes` | — | D@1 and D@2 served once each, labelled; the older one compacted; overflow listed by name. |

**The L5 walk-through** (P3). `RECENT_OUTCOMES` = 10; weight 1.0 on the current revision
and 0.5 on older ones. The setup adds 3 `writer_only_session`s on other skills, so cooldown
(3) does not block the first revision.

1. **Session a:** writer `failed` on authors@1, issue kind **`outdated`**. (A `wrong` issue
   with a `failed` outcome would fire (a) at threshold 1.) No trigger fires.
2. **Session b:** `failed` on @1, `outdated`. `recent_failures()` is true, so (c) fires (as
   does (a), because there are 2 open issues on one skill). Clusters = {{authors}}: revise
   to @2, addressing both.
3. **Session c:** `failed` on @2, `outdated`.
4. **Session d:** `failed` on @2. (c) fires.
   - Confidence: f = 1 + 1 + 0.5 + 0.5 = 3; s = 0; confidence = 1 / 5 = **0.20**, with 4
     judged outcomes, and the addressing @2 failed.
   - `deprecate_skill(authors)`, with no `replaced_by`.
   - Feedback `deprecated:<authors>:<job>` opens on `author-report`.
   - Search **shows** authors flagged `deprecated` (the chain is broken, M§6.2).
   - `get_skill` shows the banner.
5. **`restart()` on.** The new dependency feedback fires (b). Clusters = {{author-report}}
   → commit nothing. The review row is fruitless.
6. **Session e:** `worked` on authors@2. **`restart()` with `every_sessions = 1`.** (d)
   fires: feedback is open, and authors has evidence newer than its review.
   - `reactivate_skill(authors, evidence=[outcome id])`.
   - Authors is `active`.
   - The "deprecated" feedback is `closed` with resolution "reactivated".

---

## 10. Slice 1, then later slices and phases

### 10.1 Slice 1: the first runnable slice (against today's code)

This is a required first deliverable. It turns green against `main` as it is today.

| Part | Contents |
|---|---|
| Runtime API | the writer barrier and its unit test in `test_writer.rs` (§4.3) |
| Runner | `main.rs` and `cli.rs`: arc and step filters, a no-match exit of 0, `--list`, libtest flag tolerance; `phase.rs` (`IMPLEMENTED_PHASE` at its pre-P0 value, and `detect_phase` for the v3 schema) |
| Report | per-step block (§7.2), arc summary, run summary, `report.txt`, `report.json` |
| Project and driver | `TestProject`, `AgentSpec`, `AgentDriver` (`say`, `end_session`, `session`, `clear_no_wait`), `restart()`, WAL-safe copies, redaction |
| Fake | thread per connection; ordered agent and writer routes; `Text`, `ToolCalls`, `FromFn` with `StepVars`, **`Gate`** (L11 needs it); distinct per-reply usage and cost; accounting; `HashingEmbedder`; the minimal DSL (`agent().search().get_top().answer()`, `writer().save().commit()`) |
| Probe | v3 schema (`sessions`, `memories`, `memory_jobs`, `session_costs`); snapshots; invariants 1, 6 and 7 |
| Steps | **HN H2+H3 smoke (D)**; **Site S1–S3 (D)**, with S3 asserting the overwrite (`until: P0`); **SQLite Q1 (D)**, with Q2 included and printed `PASS (invariants only)` until P2 (it exercises reuse of the overview and the multi-session flow, but its own checks are P2 and B); **L2** recovery; **L11** two agents (with the gate); `selftest` (embedder bounds and duplicate texts, unscripted call detection, barrier, copy and reopen) |
| Not in Slice 1 | the D-tier Harbor files (only Q10 needs them, from P3) |
| Fixture | **capture `memory_v3.sql`** with `cargo test --test memory_suite -- --capture-v3 tests/fixtures/memory_suite/memory_v3.sql`: it runs Site S1–S3 in D and dumps every v3 table (`meta`, `sessions`, `session_messages`, `memories`, `session_costs`, `memory_jobs`). turso has no `.dump`, so the harness writes `CREATE TABLE` statements copied from `sqlite_schema`, then one `INSERT` per row, with vectors as `X'…'` blob literals. L1 loads it with one `execute` per statement. Time-sensitive: before Phase 0 merges. |

Done when `cargo test --test memory_suite` prints all of these as `PASS`, `PASS (invariants
only)` or `SKIP (phase N)`, the exact (distinct, non-zero) spend checks hold, and the
snapshot-before-clear cross-check (§4.2) passes.

### 10.2 Slice 2 and Slice 3 (still before Phase 0)

- **Slice 2, live basics:** the RecordingProxy with attribution; budget and projection; the
  HN arc in L (H0–H4); the Driftwood server v1–v3; Site S1–S3 in L with cold pairs and
  noise.
- **Slice 3, SQLite in L:** the Harbor generator (all versions, `schema_facts`, logical
  digest, selftest cross-checks); `project.fork`; the three-arm probes and pooled
  statistics; the coverage grader with its fixtures; Q1–Q4p in L. This gives **today's
  baseline** for understanding and for adapting without revisions. With `--full-arc`, the
  whole SQLite arc runs as a behaviour study.

### 10.3 What each phase unlocks

| Phase | Harness additions | Steps and checks |
|---|---|---|
| P0 | v4 probe helpers; invariant 2 | L1; S3 flips; H2 `source_session` |
| P1 (needs A1–A3) | validator responder; builder `revise`/`outcome`/`issue`; C2, C3, C5–C7 | S3–S5, S4L; Q6, Q9 (outcome and issue; Q6 `needs Q3, Q4r?` so Q4's `runs_since: P2` does not block it); L3 (first half), L9, L10, L13, L14 |
| P2 (needs A4–A5) | compat responder; pin and graph probes; invariants 3–4; C1, C4, C8 and the consequence coverage row; pausing; probe forks pause maintenance | S6, S7, S8 (compat_pairs = ∅), S9 (P2 expectations); Q2–Q4 edges, Q4r (compat_pairs = ∅); the B edge and breaking checks in Q2–Q4; L15; the `Upkeep` metrics row |
| P3 (needs A6) | consolidator responder and expected clusters; `maintenance_jobs` and review probes; invariant 5; C9 | S8, S9 (P3 expectations); Q4r, Q9r, Q10; L3 (second half), L4–L8, L12 |
| P4 | hint and ranking checks | A `[memory]` hint after the first user message, recorded once; a variant of Q3 with `DIRECTIVE.md`; embeddings counted (wire equals app; derived counts include hints); the lexical boost reorders but never admits a skill below `min_similarity`; `missed` recorded (M·D) and the miss rate reported (B) |

### 10.4 How a phase PR updates the suite

Four edits, all in `tests/memory_suite`:

1. Raise `IMPLEMENTED_PHASE`.
2. Write the newly active scripts and expectations.
3. Delete the `until:` checks for the old behaviour.
4. Run D tier and one L run, and paste the summaries into the PR.

`detect_phase()` checks the constant against the code: `schema_version` 4 means P0; writer
tools offered include `revise_skill` for P1; `confirm_dependency` or a compat request for P2;
consolidator tools for P3; a `[memory]` hint for P4. A disagreement is an `Error`.

### 10.5 Coverage boundaries

The suite tests cross-component wiring end to end. Left to the M§16 unit tests:

- graph algorithm edge cases (cycle paths, blame for violations);
- the validation rules one by one;
- `head_and_tail` cutting;
- the `expand` depth cap on old deep revisions;
- dedupe-key collapsing on merge and revert, beyond the happy paths in L6 and L7;
- backlog trigger (e) beyond L8;
- `max_depth` violations.

### 10.6 `selftest` (always first)

- Harbor: same seed gives the same digest; references equal SQL; `schema_facts` equal
  `PRAGMA table_info`.
- `SiteVersion`: each version differs from the previous one only in its declared knobs.
- Embedder: near-duplicates reach at least 0.9; unrelated arc texts stay below 0.2
  (stopwords removed). **Within each arc, no two scripted `when_to_use` texts reach
  `duplicate_similarity` (0.9), except the planned pair in L6.** The sweep runs even while
  maintenance is paused, so an accidental duplicate candidate would otherwise change a
  step's state.
- The staleness grader is checked against its fixtures (stale analysis-skill bodies, a
  duplicate overview).
- The coverage grader is checked against its fixtures.
- An unscripted call is detected.
- Gates hold and release.
- The barrier resolves on an idle writer.
- A copied store reopens.
- `detect_phase` agrees with the constant.

---

## 11. Running it

```toml
[[test]]
name = "memory_suite"
path = "tests/memory_suite/main.rs"
harness = false
```

| Command | Does |
|---|---|
| `cargo test` | Unit tests plus the suite, D tier |
| `cargo test --test memory_suite -- --arc site` | One arc |
| `cargo test --test memory_suite -- lifecycle.L9` | One scenario (positional `arc[.step]` filter) |
| `… -- --arc site --until S7` | Steps up to S7, with their `needs` |
| `… -- --arc site --from S7 --state <run>/site/S6` | Resume from a kept step (WAL-safe copy) |
| `APILA_LIVE=1 cargo test --release --test memory_suite -- --tier live --arc hn` | HN live |
| `APILA_LIVE=1 … -- --tier live --repeat 5 --parallel 5` | Full live run with statistics |
| `… -- --tier live --dry-run` | Plan and projected cost |
| `… -- --tier live --full-arc --probe-k 2` | Every step; 2 probe runs per arm |
| `… -- --keep --strict --verbose` | Keep artifacts; warnings fail; show Echo checks |

**CLI tolerance.** A filter that matches nothing runs nothing and exits 0 (`cargo test
<filter>` passes the filter to every test binary). The CLI ignores `--nocapture`,
`--test-threads`, `-q`, `--color` and `--ignored`. It answers `--list` (one
`arc.step: test` per line), and accepts `--format` (honouring `--list --format terse`).

**Environment variables:**

| Variable | Meaning |
|---|---|
| `APILA_LIVE` | Must be `1` for live runs |
| `OPENROUTER_API_KEY` | Key; fallback `test_env/config.json` |
| `APILA_LIVE_MODEL`, `APILA_LIVE_WRITER_MODEL`, `APILA_LIVE_EMBEDDING_MODEL` | Models |
| `APILA_LIVE_BUDGET_USD`, `APILA_BUDGET_HN`, `APILA_BUDGET_SITE`, `APILA_BUDGET_SQLITE` | Caps |
| `APILA_HN` | `auto`, `live`, `offline` |
| `NO_COLOR` | Plain output |

---

## 12. memory.md amendments and open questions

The suite follows memory.md as written. The amendments below fill gaps that specific steps
depend on. Each gives exact text to fold into memory.md, and **must be accepted before the
named phase is implemented**, so the implementer builds to it. This document does not edit
memory.md.

### 12.1 Required amendments

**A1: draft replacement. Before P1.** Used by S5 and L13.

In M§6.7, under **`CommitTool`**, after step 4, add:

> 5. Staged drafts are keyed by skill. A `save_skill` or `revise_skill` for a skill that
>    already has a staged draft in this job replaces that draft, and the tool output says
>    "replaced the staged draft of `<name>`". A pin-only confirmation is keyed by
>    (skill, dependency) and is replaced the same way.

**A2: the verdict survives a rejected commit. Before P1.** Used by S4.

In M§6.7, **How the loop ends**, replace item 1 of the permissive final attempt with:

> 1. The verdict is the one given in the most recent `end_turn` call, even if that call was
>    rejected. Only when no `end_turn` call ever carried a verdict does it become `unclear`,
>    so all revision and new-skill drafts are dropped. Pin-only confirmations are not gated
>    by the verdict and are kept if they validate (§6.5).

**A3: order of the deterministic rules and the gated validator. Before P1.** Used by S4, S5
and L13.

In M§6.7, **`CommitTool`**, replace step 2 with:

> 2. Run deterministic validation over the whole set. Only if every draft passes does the
>    gated validator run, once for each gated draft without a cached verdict. On the
>    permissive final attempt, drafts that fail deterministic validation are dropped first
>    (with the drafts that depend on them), and the validator then runs on the surviving
>    gated drafts.

**A4: `max_compat_checks` may be 0. Before P2.** Used by every `post-analysis` Due or
Blocked check (S7, S9, Q4) and by probe forks.

In M§5.4, replace the `dependencies.max_compat_checks` row with:

> | `dependencies.max_compat_checks` | 8 | Compatibility checks per maintenance pass, 0 to 64. 0 runs none; pins stay where they are and due pairs wait. |

In M§11, after "Values are range-checked in each `resolve`", add:

> Ranges are given in §5.4. `dependencies.max_compat_checks: 0` is valid and turns
> compatibility checks off, for operators and for tests that must observe pins before a
> pass moves them.

**A5: sweep state and maintenance on recover-only jobs. Before P2.** Used by `restart()`
(S8, S9r, Q4r, Q9r, Q10r–t, Lifecycle).

In M§9.1, replace the first sentence of item 1 (`sweep`) with:

> 1. **`sweep`** (no LLM). Runs once per agent per `MemoryWriter` instance (tracked in
>    memory on the writer thread, not in `meta`), and again **on the next maintenance pass**
>    after any commit that changed revisions. It never re-runs inside the pass whose commit
>    changed revisions.

And after "One **maintenance pass** runs the steps below in order", add:

> The pass runs at the end of every `process` call for the agent, including a recover-only
> job with no pending session.

**A6: the form of an artifact path in its dedupe key. Before P3.** Used by Q10.

In M§9.2, after "it inserts an `outdated` feedback row (…)", add:

> `<path>` in `artifact:<path>` is relative to the agent directory with `/` separators
> (for example `artifact:databases/sales.db`), or absolute when the revision referenced an
> absolute path.

### 12.2 Proposed spec change, for the user to decide (not assumed)

**P-1: when an `addresses`-exempt draft triggers the validator.** M§6.5 rule 4 says
"every exempt draft counts as having used the exemption", so any revision that addresses
earlier feedback is validated. **The suite tests this as written** (S3: validated = {catalog}).

An alternative reading counts the exemption only when the draft would otherwise fail the
guard. It would save validator calls on small fixes, at the cost of not checking that
small fixes really address their feedback (M§6.6 criterion 2). If the user prefers it,
replace in rule 4:

> A draft uses the exemption only when it would fail the guard without it.

S3's expectation then becomes validated = ∅.

### 12.3 Open questions (the suite works within today's text)

| # | Question | How the suite copes | Proposed resolution |
|---|---|---|---|
| O1 | Reactivation has no trigger of its own. | L5 reaches it through (d) with `every_sessions = 1`. | Add trigger (f): "a `worked` or `partial` writer outcome on a deprecated skill since the watermark." |
| O2 | Duplicate candidates have no trigger. | L6 uses (d) plus one open feedback row. | Count new duplicate candidates for (d). |
| O3 | Is an agent's `report_skill_outcome` made before a second read re-targeted to the later revision? | L9 asserts only the writer's outcome. | "An agent outcome stays on the revision it named." |
| O4 | Do trivial sessions ("thanks") count for `every_sessions` and cooldown? The text says "analysed sessions", so yes. | `writer_only_session` counts, as stated. | Keep, or count only `succeeded` and `partial` verdicts. |
| O5 | When a revert reopens feedback (M§6.8), is `resolved_by` cleared? | C5 exempts reopened rows; L7 asserts only the status and the re-attachment. | "Reopening sets `status='open'`, clears `resolved_by`, and keeps the old value in `resolution`." |

---

## 13. Deferred: R tier (replay)

Replay is deferred until L tier has run for a while. Each phase changes the tool surface,
so cassettes would mostly be stale. The proxy already logs every exchange. When replay is
added, HN cassettes are never promoted into the repo, and headers are never stored.

---

## 14. Dependencies

None added.

| Need | Met by |
|---|---|
| HTTP servers | `std::net::TcpListener`, a thread per connection |
| Outbound HTTP | `reqwest::blocking` |
| HTML parsing | `scraper` |
| SQLite | `turso_core` |
| JSON | `serde_json` |
| Run ids | `chrono` |
| Tables | `unicode-width` |
| Randomness | `SplitMix64` |
| Colour | ANSI codes behind `IsTerminal` |
| Exact sign-flip permutation test and hierarchical bootstrap | about 80 lines in `metrics.rs` |
