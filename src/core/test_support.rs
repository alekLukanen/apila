//! Helpers shared by the test modules. They stand up the real things the
//! runtime talks to — a project directory on disk and an http server speaking
//! openrouter's chat completion shape — so tests drive the runtime through
//! its own interface rather than reaching inside it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use crate::core::config::config::CONFIG_FILE_NAME;
use crate::core::runtime::agent_config::{
    AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, DIRECTIVE_FILE_NAME, SYSTEM_FILE_NAME,
};

/// The model an agent runs on unless a test names another one.
pub const TEST_MODEL: &str = "openai/gpt-4o";

/// The iteration bound an agent gets unless a test names another one. High
/// enough that no test hits it by accident.
pub const TEST_MAX_ITERATIONS: u32 = 10;

/// How long a test waits on an agent's background request before giving up.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// One canned reply in the script a stub server answers with.
pub struct StubReply {
    body: String,
}

impl StubReply {
    /// The assistant answering in plain text, which ends the turn.
    pub fn text(content: &str) -> StubReply {
        StubReply::from_choice(
            serde_json::json!({"role": "assistant", "content": content}),
            "stop",
        )
    }

    /// The assistant asking for one tool call.
    pub fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> StubReply {
        StubReply::tool_calls(vec![(id.to_string(), name.to_string(), arguments)])
    }

    /// The assistant asking for several tool calls in one message.
    pub fn tool_calls(calls: Vec<(String, String, serde_json::Value)>) -> StubReply {
        let calls: Vec<serde_json::Value> = calls
            .into_iter()
            .map(|(id, name, arguments)| {
                serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        // the api carries the arguments as a json document
                        // encoded in a string, not as json
                        "arguments": arguments.to_string(),
                    },
                })
            })
            .collect();

        StubReply::from_choice(
            serde_json::json!({
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": calls,
            }),
            "tool_calls",
        )
    }

    fn from_choice(message: serde_json::Value, finish_reason: &str) -> StubReply {
        StubReply {
            body: serde_json::json!({
                "id": "stub-completion",
                "model": "openai/gpt-4o",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason,
                }],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
            })
            .to_string(),
        }
    }
}

/// A stand in for openrouter that answers chat completions from a script and
/// keeps the request bodies it was sent. Point a project's
/// `openrouter_base_url` at [`StubOpenRouter::base_url`].
///
/// The server thread lives for the rest of the test binary; it holds one
/// loopback port and goes away with the process.
pub struct StubOpenRouter {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl StubOpenRouter {
    /// Starts the server on a loopback port of the operating system's
    /// choosing, replying to every request with `reply` as the assistant.
    pub fn start(reply: &str) -> StubOpenRouter {
        StubOpenRouter::start_delayed(reply, Duration::ZERO)
    }

    /// The same server, holding each reply back by `delay`. Tests that need
    /// an agent to still be working when they look at it use this, rather
    /// than trying to catch a reply that has already landed.
    pub fn start_delayed(reply: &str, delay: Duration) -> StubOpenRouter {
        StubOpenRouter::start_script(vec![StubReply::text(reply)], delay)
    }

    /// Answers each request with the next reply in `replies`.
    ///
    /// Once the script runs out the last reply repeats, so a one reply script
    /// behaves exactly as the single reply server always has — and a test that
    /// means to run an agent out of iterations only has to script one tool
    /// call.
    pub fn start_script(replies: Vec<StubReply>, delay: Duration) -> StubOpenRouter {
        assert!(!replies.is_empty(), "a stub server needs a reply");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub openrouter");
        let addr = listener.local_addr().expect("stub openrouter address");
        let requests = Arc::new(Mutex::new(Vec::new()));

        let bodies: Vec<String> = replies.into_iter().map(|reply| reply.body).collect();
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            let mut answered = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let request = read_request(&mut stream);
                seen.lock().expect("mutex error").push(request);
                thread::sleep(delay);

                let body = &bodies[answered.min(bodies.len() - 1)];
                answered += 1;

                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.flush();
            }
        });

        StubOpenRouter {
            base_url: format!("http://{}", addr),
            requests,
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// The bodies of the chat completion requests received so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("mutex error").clone()
    }
}

/// Reads one http request off the socket, returning its body. The request has
/// to be drained before the reply goes out or the client sees the connection
/// close mid write.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut length = 0usize;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// Waits for an agent's background thread to land, since the runtime answers
/// off the caller's thread. Panics rather than hanging the test suite.
pub fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {}", what);
}

// Project directories ///////////////
//////////////////////////////////////

/// An empty project directory holding a config.json and a project level
/// SYSTEM.md. `name` says which test it belongs to and has to be unique
/// across the suite, since the directory is wiped on the way in.
pub fn project_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("apila-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create project dir");
    write_project_config(&dir, None);
    fs::write(dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    dir
}

/// A project whose agents talk to a stub openrouter over loopback instead of
/// the real one, for the tests that run an agent. Nothing leaves the machine.
pub fn project_with_server(name: &str, reply: &str) -> (PathBuf, StubOpenRouter) {
    project_with_delayed_server(name, reply, Duration::ZERO)
}

/// The same project, with a server that takes `delay` to answer, so a test
/// can look at an agent while it is still working.
pub fn project_with_delayed_server(
    name: &str,
    reply: &str,
    delay: Duration,
) -> (PathBuf, StubOpenRouter) {
    let dir = project_dir(name);
    let server = StubOpenRouter::start_delayed(reply, delay);
    write_project_config(&dir, Some(&server.base_url()));
    (dir, server)
}

/// A project whose stub openrouter answers from a script, for the tests that
/// run an agent through a tool loop.
pub fn project_with_script(name: &str, replies: Vec<StubReply>) -> (PathBuf, StubOpenRouter) {
    let dir = project_dir(name);
    let server = StubOpenRouter::start_script(replies, Duration::ZERO);
    write_project_config(&dir, Some(&server.base_url()));
    (dir, server)
}

/// Writes the project's config.json, pointing it at `base_url` when the test
/// runs its own openrouter.
fn write_project_config(dir: &Path, base_url: Option<&str>) {
    let base_url = match base_url {
        Some(base_url) => format!(r#", "openrouter_base_url": "{}""#, base_url),
        None => String::new(),
    };
    fs::write(
        dir.join(CONFIG_FILE_NAME),
        format!(r#"{{"openrouter_api_key": "sk-or-test"{}}}"#, base_url),
    )
    .expect("write config");
}

/// Creates a fully configured agent directory inside `project`, unless
/// `with_agents_file` says to leave out its AGENTS.md.
pub fn agent_dir(project: &Path, name: &str, with_agents_file: bool) -> PathBuf {
    let dir = project.join(name);
    fs::create_dir_all(&dir).expect("create agent dir");
    if with_agents_file {
        fs::write(dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    }
    write_agent_config(&dir, TEST_MODEL);
    dir
}

/// Writes an agent's config.json, naming the model it runs on. The iteration
/// bound is required so it is written too, and no tools are enabled, which is
/// what most tests want.
pub fn write_agent_config(dir: &Path, model: &str) {
    write_agent_config_json(
        dir,
        &format!(
            r#"{{"model": "{}", "agent_max_iterations": {}}}"#,
            model, TEST_MAX_ITERATIONS
        ),
    );
}

/// Writes an agent's config.json with a `tools` block, for the tests that mean
/// to run tools.
pub fn write_agent_config_with_tools(
    dir: &Path,
    model: &str,
    max_iterations: u32,
    tools: serde_json::Value,
) {
    write_agent_config_json(
        dir,
        &serde_json::json!({
            "model": model,
            "agent_max_iterations": max_iterations,
            "tools": tools,
        })
        .to_string(),
    );
}

/// Writes an agent's config.json verbatim, for the tests that mean to leave a
/// setting out or write a bad one.
pub fn write_agent_config_json(dir: &Path, json: &str) {
    fs::write(dir.join(AGENT_CONFIG_FILE_NAME), json).expect("write agent config");
}

/// Writes an agent's DIRECTIVE.md, so it opens the conversation by itself.
pub fn write_directive(dir: &Path, directive: &str) {
    fs::write(dir.join(DIRECTIVE_FILE_NAME), directive).expect("write directive file");
}
