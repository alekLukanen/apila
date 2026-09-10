use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

/// How long a command may run before it is killed, when the agent's config
/// does not say.
pub const DEFAULT_BASH_TIMEOUT: u64 = 120;

/// The most either stream may contribute before it is cut short. Tool results
/// are paid for by the token and read on a terminal, so a command that prints a
/// megabyte is worth less to the model than a note saying it did.
pub const MAX_STREAM_BYTES: usize = 8_192;

/// How often the tool looks in on a running command. Short enough that a
/// command finishing is noticed straight away, long enough that waiting out a
/// slow one costs nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long the readers are given to finish once the command has exited, for
/// the case where something the command left running still holds the pipe open.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Runs a shell command in the agent's own directory. This is what makes the
/// harness a coding harness, and also the one tool that can do real damage:
/// nothing here sandboxes anything, so what an agent may do is whatever the
/// user running apila may do. It is off unless a `config.json` enables it.
pub struct BashTool;

impl BashTool {
    pub fn new() -> BashTool {
        BashTool
    }
}

/// The tool's own settings, out of the agent's `tools.configs` entry for
/// `bash`. Unknown fields are refused so a misspelled key is reported when the
/// agent's files are read, rather than silently doing nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BashSettings {
    /// How long a command may run before it is killed, in seconds.
    #[serde(default = "default_bash_timeout")]
    pub bash_timeout: u64,
}

fn default_bash_timeout() -> u64 {
    DEFAULT_BASH_TIMEOUT
}

impl BashSettings {
    /// Reads the settings out of a tool's config block, which is an empty
    /// object when the user wrote none.
    fn parse(config: &serde_json::Value) -> Result<BashSettings, String> {
        serde_json::from_value::<BashSettings>(config.clone()).map_err(|err| err.to_string())
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.bash_timeout)
    }
}

impl Tool for BashTool {
    fn name(&self) -> String {
        "bash".into()
    }

    fn description(&self) -> String {
        "Run a shell command with `bash -c` in your own working directory and \
         read back its exit code, stdout and stderr. The command is killed if \
         it runs longer than the configured timeout, and long output is cut \
         short, so prefer commands that finish quickly and print only what you \
         need."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to run. It is passed to `bash -c`, so pipes, redirection and `&&` all work."
                }
            },
            "required": ["command"]
        })
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        BashSettings::parse(config).map(|_| ())
    }

    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        // the settings are read once here rather than on every command, so a
        // state that exists is a state whose settings are already usable
        let settings = BashSettings::parse(&context.config())
            .map_err(|error| ToolError::InvalidConfig { error })?;
        Ok(Box::new(BashState { settings }))
    }
}

/// What the tool keeps for one agent: its settings, read once when the agent
/// first reached for it.
pub struct BashState {
    settings: BashSettings,
}

impl ToolState for BashState {
    fn run(
        &mut self,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let command = match arguments.get("command") {
            None => {
                return Err(ToolError::MissingArgument {
                    argument: "command".into(),
                })
            }
            Some(value) => value.as_str().ok_or_else(|| ToolError::InvalidArgument {
                argument: "command".into(),
                expected: "a string".into(),
            })?,
        };
        if command.trim() == "" {
            return Err(ToolError::InvalidArgument {
                argument: "command".into(),
                expected: "not empty".into(),
            });
        }

        run_command(&context.dir(), command, self.settings.timeout())
    }
}

/// One stream of a command's output. Everything the command writes is read, so
/// it never blocks on a full pipe, but only the first [`MAX_STREAM_BYTES`] are
/// kept — `total` remembers how much there was so the model can be told what it
/// is not seeing.
struct Stream {
    kept: Vec<u8>,
    total: usize,
}

impl Stream {
    fn new() -> Stream {
        Stream {
            kept: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        let room = MAX_STREAM_BYTES.saturating_sub(self.kept.len());
        if room > 0 {
            self.kept.extend_from_slice(&chunk[..room.min(chunk.len())]);
        }
    }

    /// How the stream reads in the tool's output.
    fn render(&self) -> String {
        if self.total == 0 {
            return "(none)".to_string();
        }
        let mut text = String::from_utf8_lossy(&self.kept).into_owned();
        if self.total > self.kept.len() {
            text.push_str(&format!(
                "\n… truncated, {} more bytes",
                self.total - self.kept.len()
            ));
        }
        text
    }
}

/// Reads `reader` to its end on a thread of its own, into a buffer the caller
/// can look at whenever it likes.
///
/// A reader is what keeps the command from blocking: a pipe nobody drains fills
/// after about 64KB and stops the command dead, which a `try_wait` loop would
/// then sit through until the timeout.
fn drain(mut reader: impl Read + Send + 'static) -> (Arc<Mutex<Stream>>, Arc<AtomicBool>) {
    let stream = Arc::new(Mutex::new(Stream::new()));
    let done = Arc::new(AtomicBool::new(false));

    let thread_stream = Arc::clone(&stream);
    let thread_done = Arc::clone(&done);
    thread::spawn(move || {
        let mut chunk = [0u8; 8_192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => thread_stream
                    .lock()
                    .expect("mutex error")
                    .push(&chunk[..read]),
            }
        }
        thread_done.store(true, Ordering::SeqCst);
    });

    (stream, done)
}

/// Runs `command` in `dir`, killing it once `timeout` has passed.
///
/// A command that fails, and one that has to be killed, both come back as
/// output rather than as an error: the exit code and whatever it printed before
/// it was stopped are what the model needs in order to try something else.
fn run_command(dir: &std::path::Path, command: &str, timeout: Duration) -> Result<ToolOutput, ToolError> {
    // `-c` rather than `-lc`: a login shell sources the user's profile, which
    // is slow and prints banner text into stdout the model would have to read
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        // a command that reads from stdin would otherwise sit there until the
        // timeout rather than seeing that there is nothing to read
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ToolError::NotStarted {
            error: err.to_string(),
        })?;

    let (stdout, stdout_done) = drain(child.stdout.take().expect("piped stdout"));
    let (stderr, stderr_done) = drain(child.stderr.take().expect("piped stderr"));

    let deadline = Instant::now() + timeout;
    let status: Option<ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => {
                return Err(ToolError::NotStarted {
                    error: err.to_string(),
                })
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(POLL_INTERVAL);
    };

    // the readers are given a moment to finish, and then read wherever they
    // got to. they are never joined: a command like `sleep 100 &` exits at once
    // but leaves a grandchild holding the pipe, so a reader can wait for an end
    // that never comes. a thread parked on a read costs nothing and goes with
    // the process
    let grace = Instant::now() + DRAIN_GRACE;
    while Instant::now() < grace {
        if stdout_done.load(Ordering::SeqCst) && stderr_done.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    let mut lines = Vec::new();
    match status {
        Some(status) => match status.code() {
            Some(code) => lines.push(format!("exit code: {}", code)),
            None => lines.push("killed by a signal".to_string()),
        },
        None => lines.push(format!(
            "timed out after {}s and was killed",
            timeout.as_secs()
        )),
    }
    lines.push(format!(
        "stdout:\n{}",
        stdout.lock().expect("mutex error").render()
    ));
    lines.push(format!(
        "stderr:\n{}",
        stderr.lock().expect("mutex error").render()
    ));

    Ok(ToolOutput::new(lines.join("\n")))
}
