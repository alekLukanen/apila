use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
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
    ///
    /// `complete` says whether the reader reached the end of the pipe. When it
    /// did not, this is everything that had arrived rather than everything there
    /// was, and it says so: a byte count that looks exact but was measured while
    /// the command was still writing would have the model reasoning about output
    /// it never saw all of.
    fn render(&self, complete: bool) -> String {
        if self.total == 0 {
            return if complete {
                "(none)".to_string()
            } else {
                "(nothing yet; the command was still writing)".to_string()
            };
        }

        let mut text = String::from_utf8_lossy(&self.kept).into_owned();
        let dropped = self.total - self.kept.len();
        match (dropped > 0, complete) {
            (true, true) => text.push_str(&format!("\n… truncated, {} more bytes", dropped)),
            (true, false) => text.push_str(&format!(
                "\n… truncated, at least {} more bytes, and the command was still writing",
                dropped
            )),
            (false, true) => {}
            (false, false) => text.push_str("\n… the command was still writing"),
        }
        text
    }
}

/// Reads `reader` to its end on a thread of its own, into a buffer the caller
/// can look at whenever it likes, until it runs out or `stop` is set.
///
/// A reader is what keeps the command from blocking: a pipe nobody drains fills
/// after about 64KB and stops the command dead, which a `try_wait` loop would
/// then sit through until the timeout.
///
/// `stop` is the other half of that. A command can exit while something it
/// started keeps the pipe open and keeps writing — `bash -c 'yes &'` returns at
/// once and then writes forever — and a reader with nowhere to put the bytes
/// would spin at full speed for the life of the process. Once the caller has
/// stopped listening the reader stops reading, drops its end of the pipe, and
/// whatever is still writing gets a broken pipe and goes away.
fn drain(
    mut reader: impl Read + Send + 'static,
    stop: Arc<AtomicBool>,
) -> (Arc<Mutex<Stream>>, Arc<AtomicBool>) {
    let stream = Arc::new(Mutex::new(Stream::new()));
    let done = Arc::new(AtomicBool::new(false));

    let thread_stream = Arc::clone(&stream);
    let thread_done = Arc::clone(&done);
    thread::spawn(move || {
        let mut chunk = [0u8; 8_192];
        while !stop.load(Ordering::SeqCst) {
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

/// How a command ended.
enum Outcome {
    Exited(ExitStatus),
    /// It outran its timeout and was killed. `group_killed` says whether
    /// everything it started went with it, because that is the difference
    /// between telling the model the work stopped and telling it the work may
    /// still be going.
    TimedOut { group_killed: bool },
}

/// Kills the command and everything it started.
///
/// The command runs in a process group of its own, so a negative pid reaches
/// the whole group — `child.kill()` on its own signals only the shell, which
/// leaves anything it forked running and still writing to the agent's
/// directory. Signalling a group needs libc, which this crate does not depend
/// on, so it goes through `kill`; when that is not there the shell is killed on
/// its own and the model is told as much.
fn kill_group(child: &mut Child) -> bool {
    // the child was spawned into its own group, so its pid is the group's id
    let group = format!("-{}", child.id());
    let group_killed = Command::new("kill")
        .arg("-KILL")
        .arg("--")
        .arg(&group)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    let _ = child.kill();
    let _ = child.wait();
    group_killed
}

/// Runs `command` in `dir`, killing it once `timeout` has passed.
///
/// A command that fails, and one that has to be killed, both come back as
/// output rather than as an error: the exit code and whatever it printed before
/// it was stopped are what the model needs in order to try something else.
fn run_command(
    dir: &std::path::Path,
    command: &str,
    timeout: Duration,
) -> Result<ToolOutput, ToolError> {
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
        // a group of its own, so a timeout can take out everything the command
        // started rather than only the shell that started it
        .process_group(0)
        .spawn()
        .map_err(|err| ToolError::NotStarted {
            error: err.to_string(),
        })?;

    // set once this call has stopped listening, which is what stops a reader
    // spinning on output nobody is going to read
    let stop = Arc::new(AtomicBool::new(false));
    let (stdout, stdout_done) = drain(
        child.stdout.take().expect("piped stdout"),
        Arc::clone(&stop),
    );
    let (stderr, stderr_done) = drain(
        child.stderr.take().expect("piped stderr"),
        Arc::clone(&stop),
    );

    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Outcome::Exited(status),
            Ok(None) => {}
            Err(err) => {
                stop.store(true, Ordering::SeqCst);
                let _ = child.kill();
                return Err(ToolError::Failed {
                    error: err.to_string(),
                });
            }
        }
        if Instant::now() >= deadline {
            break Outcome::TimedOut {
                group_killed: kill_group(&mut child),
            };
        }
        thread::sleep(POLL_INTERVAL);
    };

    // the readers are given a moment to finish what is already in the pipe, and
    // are then told to stop. they are not joined: a command like `sleep 100 &`
    // exits at once but leaves a grandchild holding the pipe, and a reader can
    // be waiting on an end that never comes. telling it to stop is what makes
    // that wait cost nothing — it wakes on the next byte, or never, and either
    // way it is not spinning and not holding the pipe open for long
    let grace = Instant::now() + DRAIN_GRACE;
    while Instant::now() < grace {
        if stdout_done.load(Ordering::SeqCst) && stderr_done.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    stop.store(true, Ordering::SeqCst);

    let mut lines = Vec::new();
    match outcome {
        Outcome::Exited(status) => match status.code() {
            Some(code) => lines.push(format!("exit code: {}", code)),
            None => lines.push("killed by a signal".to_string()),
        },
        // the model acts on what this says, so it says what actually happened:
        // work that is still running is work it must not assume it undid
        Outcome::TimedOut { group_killed: true } => lines.push(format!(
            "timed out after {}s; it and everything it started were killed",
            timeout.as_secs()
        )),
        Outcome::TimedOut { group_killed: false } => lines.push(format!(
            "timed out after {}s and the shell was killed, but anything it \
             started may still be running",
            timeout.as_secs()
        )),
    }
    lines.push(format!(
        "stdout:\n{}",
        stdout
            .lock()
            .expect("mutex error")
            .render(stdout_done.load(Ordering::SeqCst))
    ));
    lines.push(format!(
        "stderr:\n{}",
        stderr
            .lock()
            .expect("mutex error")
            .render(stderr_done.load(Ordering::SeqCst))
    ));

    Ok(ToolOutput::new(lines.join("\n")))
}
