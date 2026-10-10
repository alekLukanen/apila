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

## Tools

| tool | enabled by | settings |
| --- | --- | --- |
| `end_turn` | always | none |
| `bash` | `"enabled": ["bash"]` | `bash_timeout`, in seconds (default 120) |
| `sqlite` | `"enabled": ["sqlite"]` | `max_databases` open at once (default 8), `max_rows` returned per query (default 100), `sqlite_timeout` per call, in seconds (default 30) |
| `fetch_webpage` | `"enabled": ["fetch_webpage"]` | `fetch_timeout`, in seconds (default 30), `max_page_bytes` saved per page (default 10 MiB), `allow_private_hosts` (default false), `use_proxy` to honour `HTTP_PROXY`/`HTTPS_PROXY` (default false; through a proxy only literal private addresses are refused) |
| `read_webpage_data` | `"enabled": ["read_webpage_data"]` | `max_lines` returned per call (default 500) |

`sqlite` gives an agent sqlite databases (via the `turso` crate) that it can
create, drop, list, inspect, write to and query. They last for the session: they
are held in memory by the agent that created them, never written to disk, and
gone when apila exits. Every call runs in a transaction of its own and is undone
completely if any part of it fails or it runs past `sqlite_timeout`.
