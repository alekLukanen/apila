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
  "model": "thinkingmachines/inkling-small:free",
  "agent_max_iterations": 20,
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

`end_turn` is how an agent says it has finished its work, so every agent has it
whether or not it is listed. A turn also ends when the agent simply answers in
text without asking for a tool.

`bash` runs a command with `bash -c` in the agent's own directory and hands back
the exit code, stdout and stderr. A command that outruns `bash_timeout` is
killed, and long output is cut short. **Nothing sandboxes it**: an agent with
`bash` enabled can do anything the user running apila can do.

