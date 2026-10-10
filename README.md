# Apila
A general purpose agent harness for coding.

## How to Run

You can run the TUI for a directory containing agent configuration
```
cargo run --bin main -- --project-dir="../apila_test_agents/"
```

The directory must have this general structure 
```
apila_test_agents/
   agent_name/
      config.json
      AGENTS.md
      DIRECTIVE.md // (optional)
   config.json
   SYSTEM.md
```

The base `config.json` should look like this
```
{
  "openrouter_api_key": "<key-here>", 
  "openrouter_base_url": "https://openrouter.ai/api/v1"
}
```

Every agent needs its own `config.json`. It should look like this
```
{
  "model": "anthropic/claude-sonnet-5.5",
  "agent_max_iterations": 20,
  "reasoning": { "effort": "medium" },
  "tools": {
    "enabled": ["bash"],
    "configs": [
      { "tool": "bash", "bash_timeout": 30 }
    ]
  }
}
```

`model` and `agent_max_iterations` are required; there is no project wide
default for either.

* `agent_max_iterations` is the most requests the agent may make to the model
  in a single turn. Every tool call the model makes costs one, so it is the
  ceiling on what one turn can cost. An agent that reaches it stops and reports
  that it did.
* `reasoning.effort` is optional and sets how hard a reasoning model thinks:
  `xhigh`, `high`, `medium`, `low`, `minimal` or `none`, following OpenRouter's
  names. Left unset, no `reasoning` is sent and the provider's default applies;
  `none` explicitly turns reasoning off. It must be nested under `reasoning`,
  not OpenRouter's top-level `reasoning_effort`, which apila does not read. The
  TUI shows it after the model name, e.g. `openai/gpt-4o (high)`.
* `tools.enabled` names the tools the agent may call. It defaults to nothing, so
  an agent has to opt in before it can run commands.
* `tools.configs` carries each tool's own settings. Nothing outside the tool
  reads them, so a tool can add a setting without this file's shape changing. A
  config for a tool that is not enabled is ignored.
* `memory` is optional and switches on what the agent learns from its past
  sessions. See [Memory](#memory).

## Sessions

A session lasts from starting an agent until you type `/clear` in its chat and
press Enter. `/clear` ends the session and starts a new one the way starting
the agent does: on its `DIRECTIVE.md` if it has one, otherwise waiting for your
first message. If the agent is in the middle of a turn, the clear is queued and
runs when the turn ends; anything you send after a queued `/clear` opens the
new session.

Every agent's sessions are recorded as they happen in
`<agent_dir>/memory/memory.db`, whether or not memory is configured.

## Memory

With `memory.skills` enabled, an agent turns its finished sessions into skills
(markdown notes on procedures that worked) and can search them in later
sessions.

```
{
  "model": "anthropic/claude-sonnet-5.5",
  "agent_max_iterations": 20,
  "tools": { "enabled": ["sqlite"] },
  "memory": {
    "skills": {
      "enabled": true,
      "embedding_model": "openai/text-embedding-3-small",
      "min_similarity": 0.25,
      "writer_model": "anthropic/claude-haiku-4.5",
      "writer_max_iterations": 8
    }
  }
}
```

* `enabled` defaults to `false`. When it is `true`, `embedding_model` and
  `writer_max_iterations` are required.
* `embedding_model` is the OpenRouter embedding model skills are searched with.
  A skill is embedded as its name and its `when_to_use`, so changing the model
  leaves skills saved under the old one out of searches.
* `min_similarity` (default 0.25, between -1 and 1) is how similar a skill has
  to be to a request for `search_skills` to return it.
* `writer_model` is the model that writes the skills. It defaults to the
  agent's own model; a cheaper one works well.
* `writer_max_iterations` is the most requests the writer may make while
  analysing one session.

When a session ends with `/clear`, a background writer reads its transcript and
saves up to three skills from it, updating an existing skill rather than
duplicating it. A session left open when apila closed is analysed the next time
apila starts. A session that fails to be analysed is retried, up to three times
in all. Sessions that end while skills are disabled are kept but never analysed.

Enabling skills gives the agent two tools, which are not listed in
`tools.enabled`:

* `search_skills` takes a `request` and an optional `limit` (default 5, at most
  20) and lists the id, name, similarity and `when_to_use` of each skill at or
  above `min_similarity`.
* `get_skill` takes an `id` and returns the whole skill.

## Tools

| tool | enabled by | settings |
| --- | --- | --- |
| `end_turn` | always | none |
| `bash` | `"enabled": ["bash"]` | `bash_timeout`, in seconds (default 120) |
| `sqlite` | `"enabled": ["sqlite"]` | `max_databases` saved at once (default 8), `max_rows` returned per query (default 100), `sqlite_timeout` per call, in seconds (default 30) |
| `fetch_webpage` | `"enabled": ["fetch_webpage"]` | `fetch_timeout`, in seconds (default 30), `max_page_bytes` saved per page (default 10 MiB), `allow_private_hosts` (default false), `use_proxy` to honour `HTTP_PROXY`/`HTTPS_PROXY` (default false; through a proxy only literal private addresses are refused) |
| `read_webpage_data` | `"enabled": ["read_webpage_data"]` | `max_lines` returned per call (default 500) |
| `search_skills`, `get_skill` | `memory.skills` (see [Memory](#memory)) | none of their own |

`sqlite` gives an agent sqlite databases (via the `turso` crate) that it can
create, drop, list, inspect, write to and query. Each one is saved at
`<agent_dir>/databases/<name>.db` and is still there in later sessions and
after apila restarts, until the agent drops it. `max_databases` counts every
saved database. Every call runs in a transaction of its own and is undone
completely if any part of it fails or it runs past `sqlite_timeout`. `ATTACH`,
`DETACH` and `VACUUM INTO` are refused.

Only one apila process can use a project's saved databases and memory at a
time; a second one reports that the database is in use by another apila
process.
