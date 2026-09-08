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

Every agent needs its own `config.json` naming the model it runs on; there is
no project wide default. It should look like this
```
{
  "model": "thinkingmachines/inkling-small:free"
}
```

