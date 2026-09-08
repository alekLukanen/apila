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
use crate::core::runtime::agent::{
    AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, DIRECTIVE_FILE_NAME, SYSTEM_FILE_NAME,
};

/// The model an agent runs on unless a test names another one.
pub const TEST_MODEL: &str = "openai/gpt-4o";

/// How long a test waits on an agent's background request before giving up.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A stand in for openrouter that answers every chat completion with the same
/// reply and keeps the request bodies it was sent. Point a project's
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub openrouter");
        let addr = listener.local_addr().expect("stub openrouter address");
        let requests = Arc::new(Mutex::new(Vec::new()));

        let body = serde_json::json!({
            "id": "stub-completion",
            "model": "openai/gpt-4o",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": reply},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
        })
        .to_string();

        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let request = read_request(&mut stream);
                seen.lock().expect("mutex error").push(request);

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
    let dir = project_dir(name);
    let server = StubOpenRouter::start(reply);
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
        format!(
            r#"{{"openrouter_api_key": "sk-or-test", "default_model": "{}"{}}}"#,
            TEST_MODEL, base_url
        ),
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

/// Writes an agent's config.json, naming the model it runs on.
pub fn write_agent_config(dir: &Path, model: &str) {
    fs::write(
        dir.join(AGENT_CONFIG_FILE_NAME),
        format!(r#"{{"model": "{}"}}"#, model),
    )
    .expect("write agent config");
}

/// Writes an agent's DIRECTIVE.md, so it opens the conversation by itself.
pub fn write_directive(dir: &Path, directive: &str) {
    fs::write(dir.join(DIRECTIVE_FILE_NAME), directive).expect("write directive file");
}
