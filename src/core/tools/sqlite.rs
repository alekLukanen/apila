use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use turso_core::{
    Connection, Database, LimboError, NonNan, Numeric, OpenFlags, OpenOptions, PlatformIO,
    SqliteDialect, Statement, StepResult, Value,
};
use turso_parser::ast::{Cmd, Stmt};

use crate::core::tools::tool::{
    required_string, Tool, ToolContext, ToolError, ToolOutput, ToolState,
};

pub const DEFAULT_MAX_DATABASES: usize = 8;

pub const DEFAULT_MAX_ROWS: usize = 100;

pub const DEFAULT_SQLITE_TIMEOUT: u64 = 30;

/// The most a single call may say back. Tool results are paid for by the token,
/// so a query that matches a whole table is worth less to the model than a note
/// saying it did.
pub const MAX_OUTPUT_BYTES: usize = 8_192;

/// What a query's rows are allowed to take of [`MAX_OUTPUT_BYTES`]. The rest is
/// kept for the footer, which is the one line that must never be cut: it is
/// how the model learns that it is not seeing every row.
const ROW_BYTES: usize = MAX_OUTPUT_BYTES - 256;

pub(crate) const MAX_NAME_LENGTH: usize = 64;

/// Where an agent's databases are kept, inside its own directory.
pub const DATABASES_DIR: &str = "databases";

/// Sqlite databases an agent can create, fill and query, saved in its own
/// directory so later sessions still have them. Built on `turso_core` because
/// only the engine can stop a slow statement.
pub struct SqliteTool;

impl SqliteTool {
    pub fn new() -> SqliteTool {
        SqliteTool
    }
}

/// The tool's own settings, out of the agent's `tools.configs` entry for
/// `sqlite`. Unknown fields are refused so a misspelled key is reported when
/// the agent's files are read, rather than silently doing nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteSettings {
    /// Counts every database saved in the agent's directory, from this session
    /// or an earlier one, so dropping one frees its place.
    #[serde(default = "default_max_databases")]
    pub max_databases: usize,

    /// Rows past this are left out of a query's output, and its footer says so.
    #[serde(default = "default_max_rows")]
    pub max_rows: usize,

    /// How long one call may run before it is stopped and undone, in seconds.
    #[serde(default = "default_sqlite_timeout")]
    pub sqlite_timeout: u64,
}

fn default_max_databases() -> usize {
    DEFAULT_MAX_DATABASES
}

fn default_max_rows() -> usize {
    DEFAULT_MAX_ROWS
}

fn default_sqlite_timeout() -> u64 {
    DEFAULT_SQLITE_TIMEOUT
}

impl SqliteSettings {
    /// Reads the settings out of a tool's config block, which is an empty
    /// object when the user wrote none.
    fn parse(config: &serde_json::Value) -> Result<SqliteSettings, String> {
        let settings = serde_json::from_value::<SqliteSettings>(config.clone())
            .map_err(|err| err.to_string())?;
        // any of these at zero would leave a tool that can be enabled and
        // never used
        if settings.max_databases == 0 {
            return Err("`max_databases` must be at least 1".into());
        }
        if settings.max_rows == 0 {
            return Err("`max_rows` must be at least 1".into());
        }
        if settings.sqlite_timeout == 0 {
            return Err("`sqlite_timeout` must be at least 1".into());
        }
        Ok(settings)
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.sqlite_timeout)
    }
}

impl Tool for SqliteTool {
    fn name(&self) -> String {
        "sqlite".into()
    }

    fn description(&self) -> String {
        "Create and query sqlite databases of your own. They are saved in your \
         directory and are still there in later sessions, so call `list` first \
         to see what you already have and reuse it, and `drop` what you no \
         longer need: only a limited number may be kept. Start with `create`, \
         build tables and load rows with `execute`, read them back with \
         `query`, and use `schema` to see a database's tables. Every call runs \
         in a transaction of its own and is undone completely if any part of \
         it fails or it runs past the configured timeout, so do not write \
         BEGIN, COMMIT or ROLLBACK. Pass values through `params` rather than \
         writing them into the sql. Long results are cut short, so select only \
         the columns and rows you need."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "drop", "list", "schema", "execute", "query"],
                    "description": "`create` makes a new empty database, `drop` deletes one for good, `list` names the saved databases, `schema` shows a database's tables, indexes and views, `execute` runs statements that change things (CREATE, INSERT, UPDATE, DELETE), and `query` runs one statement and returns the rows it produces."
                },
                "database": {
                    "type": "string",
                    "description": "The database to act on. Letters, digits, `_` and `-` only. Required for every action except `list`."
                },
                "sql": {
                    "type": "string",
                    "description": "The sql to run, for `execute` and `query`. `execute` without `params` accepts several statements separated by `;`; otherwise it must be a single statement."
                },
                "params": {
                    "type": "array",
                    "items": {"type": ["string", "number", "boolean", "null"]},
                    "description": "Values bound to `?1`, `?2`, ... in a single statement, for `execute` and `query`. Integers must fit in 64 bits; pass larger ones as strings."
                }
            },
            "required": ["action"]
        })
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        SqliteSettings::parse(config).map(|_| ())
    }

    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        let settings = SqliteSettings::parse(&context.config())
            .map_err(|error| ToolError::InvalidConfig { error })?;
        let dir = context.dir().join(DATABASES_DIR);
        let databases = saved_databases(&dir)
            .map_err(|err| ToolError::NotStarted {
                error: format!("the saved databases could not be listed: {}", err),
            })?
            .into_iter()
            .map(|name| (name, None))
            .collect();
        Ok(Box::new(SqliteState {
            settings,
            io: Arc::new(PlatformIO::new().map_err(failed)?),
            dir,
            databases,
        }))
    }
}

/// The names of the databases saved in `dir`. A file whose name is not one
/// `create` could have made is not one of them.
fn saved_databases(dir: &Path) -> std::io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("db") {
            continue;
        }
        if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
            if valid_name(name) {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

/// One database the agent has open. The `Database` is held alongside its
/// connection so the file stays open exactly as long as this entry does.
struct OpenDatabase {
    _database: Arc<Database>,
    connection: Arc<Connection>,
}

/// What the tool keeps for one agent: its settings and every saved database,
/// by name, opened the first time a call needs it. Kept sorted so `list`
/// reads the same from one call to the next.
pub struct SqliteState {
    settings: SqliteSettings,
    io: Arc<PlatformIO>,
    /// The agent's databases directory.
    dir: PathBuf,
    databases: BTreeMap<String, Option<OpenDatabase>>,
}

impl ToolState for SqliteState {
    fn run(
        &mut self,
        _context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let action = required_string(arguments, "action")?;
        let content = match action {
            "create" => self.create(arguments)?,
            "drop" => self.drop_database(arguments)?,
            "list" => self.list()?,
            "schema" => self.schema(arguments)?,
            "execute" => self.execute(arguments)?,
            "query" => self.query(arguments)?,
            _ => {
                return Err(ToolError::InvalidArgument {
                    argument: "action".into(),
                    expected: "one of create, drop, list, schema, execute, query".into(),
                })
            }
        };
        Ok(ToolOutput::new(truncate(content)))
    }
}

impl SqliteState {
    fn create(&mut self, arguments: &serde_json::Value) -> Result<String, ToolError> {
        let name = database_name(arguments)?;
        let path = self.path(name);
        if self.databases.contains_key(name) || path.exists() {
            return Err(ToolError::InvalidArgument {
                argument: "database".into(),
                expected: format!("a name not already in use; `{}` already exists", name),
            });
        }
        if self.databases.len() >= self.settings.max_databases {
            return Err(ToolError::Rejected {
                error: format!(
                    "{} databases are saved, which is as many as you may have. drop one you \
                     no longer need first",
                    self.databases.len()
                ),
            });
        }

        fs::create_dir_all(&self.dir).map_err(failed)?;
        let database = self.open_file(&path, OpenFlags::Create)?;
        self.databases.insert(name.to_string(), Some(database));
        Ok(format!("created `{}`", name))
    }

    fn drop_database(&mut self, arguments: &serde_json::Value) -> Result<String, ToolError> {
        let name = database_name(arguments)?;
        // removed first so the file is closed before it is deleted
        if self.databases.remove(name).is_none() {
            return Err(self.not_open(name));
        }
        let path = self.path(name);
        for suffix in ["", "-wal", "-log"] {
            let mut file = path.clone().into_os_string();
            file.push(suffix);
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(failed(err)),
            }
        }
        Ok(format!("dropped `{}`", name))
    }

    fn list(&mut self) -> Result<String, ToolError> {
        if self.databases.is_empty() {
            return Ok("(no databases)".into());
        }
        let names: Vec<String> = self.databases.keys().cloned().collect();
        let mut lines = Vec::with_capacity(names.len());
        for name in names {
            let line = match self.open(&name) {
                Ok(connection) => {
                    let tables = count_tables(&connection).map_err(failed)?;
                    format!("{} ({} {})", name, tables, plural(tables, "table"))
                }
                // one that cannot be opened is still saved, and still counts
                Err(err) => format!("{} (could not be opened: {})", name, err),
            };
            lines.push(line);
        }
        Ok(lines.join("\n"))
    }

    fn schema(&mut self, arguments: &serde_json::Value) -> Result<String, ToolError> {
        let name = database_name(arguments)?;
        let database = self.open(name)?;
        self.query_in_transaction(
            &database,
            "SELECT type, name, sql FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            Vec::new(),
        )
    }

    fn execute(&mut self, arguments: &serde_json::Value) -> Result<String, ToolError> {
        let name = database_name(arguments)?;
        let sql = required_string(arguments, "sql")?;
        let params = params(arguments)?;
        // anything bound has to be bound to one statement; a script would bind
        // it to the first and leave the model guessing about the rest
        check_sql(sql, params.is_some())?;
        let connection = &self.open(name)?;
        let deadline = Deadline::after(self.settings.timeout());

        match params {
            // with nothing to bind the sql may be a whole script, which is how a
            // model loads a table in one call rather than one row per call
            None => {
                self.in_transaction(connection, || run_script(connection, sql, &deadline))?;
                Ok("ok".into())
            }
            Some(params) => {
                let changed = self.in_transaction(connection, || {
                    let mut statement = prepare(connection, sql, params, &deadline)?;
                    while step(&mut statement)? {}
                    Ok(statement.n_change())
                })?;
                Ok(format!(
                    "ok, {} {} changed",
                    changed,
                    plural(changed, "row")
                ))
            }
        }
    }

    fn query(&mut self, arguments: &serde_json::Value) -> Result<String, ToolError> {
        let name = database_name(arguments)?;
        let sql = required_string(arguments, "sql")?;
        let params = params(arguments)?.unwrap_or_default();
        check_sql(sql, true)?;
        let database = self.open(name)?;
        self.query_in_transaction(&database, sql, params)
    }

    /// Runs one statement in a transaction and renders a header, its rows and a
    /// footer. Stops at `max_rows` rows or [`ROW_BYTES`] of output, whichever is
    /// first, and the footer says so.
    fn query_in_transaction(
        &self,
        connection: &Arc<Connection>,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<String, ToolError> {
        let max_rows = self.settings.max_rows;
        let deadline = Deadline::after(self.settings.timeout());

        self.in_transaction(connection, || {
            let mut statement = prepare(connection, sql, params, &deadline)?;
            let mut lines = Vec::new();
            let mut used = 0;
            let columns = (0..statement.num_columns())
                .map(|index| statement.get_column_name(index).into_owned())
                .collect::<Vec<_>>();
            if !columns.is_empty() {
                let header = columns.join(" | ");
                used += header.len() + 1;
                lines.push(header);
            }

            let mut kept = 0;
            let mut stopped = Stopped::AtEnd;
            while step(&mut statement)? {
                if kept == max_rows {
                    stopped = Stopped::AtMaxRows;
                    break;
                }
                let row = statement.row().expect("a row after stepping to one");
                let mut line = row
                    .get_values()
                    .map(render_value)
                    .collect::<Vec<_>>()
                    .join(" | ");
                if used + line.len() + 1 > ROW_BYTES {
                    if kept > 0 {
                        stopped = Stopped::AtOutputLimit;
                        break;
                    }
                    // a first row too long to show whole is still shown, cut,
                    // rather than leaving the model with a header and nothing
                    line = cut(line, ROW_BYTES.saturating_sub(used));
                }
                used += line.len() + 1;
                lines.push(line);
                kept += 1;
            }

            lines.push(stopped.footer(kept));
            Ok(lines.join("\n"))
        })
    }

    /// Runs `body` in a transaction: committed if it succeeds, rolled back if it
    /// fails. Sql that ends the transaction itself is reported, since part of
    /// it may then have been kept.
    fn in_transaction<T>(
        &self,
        connection: &Arc<Connection>,
        body: impl FnOnce() -> Result<T, SqlError>,
    ) -> Result<T, ToolError> {
        run_control(connection, "BEGIN").map_err(failed)?;
        let result = body();
        let still_open = !connection.get_auto_commit();

        let err = match result {
            Ok(value) if still_open => match run_control(connection, "COMMIT") {
                Ok(()) => return Ok(value),
                Err(err) => SqlError::Sql(err.to_string()),
            },
            Ok(_) => {
                return Err(ToolError::Rejected {
                    error: "the sql ended the transaction this call runs in, so some of \
                            it may have been kept. do not write BEGIN, COMMIT or ROLLBACK: \
                            every call already runs in a transaction of its own"
                        .into(),
                })
            }
            Err(err) => err,
        };

        let what = self.describe(err);
        // a failed statement can end the transaction on its own way out, which
        // leaves nothing to roll back — but also nothing to say was undone
        if !connection.get_auto_commit() {
            if let Err(rollback_err) = run_control(connection, "ROLLBACK") {
                return Err(ToolError::Failed {
                    error: format!(
                        "{}, and rolling back what the call did failed too, so some of it \
                         may have been kept: {}",
                        what, rollback_err
                    ),
                });
            }
        } else if !still_open {
            return Err(ToolError::Rejected {
                error: format!(
                    "{}. the transaction this call ran in had already been ended, so \
                     some of it may have been kept",
                    what
                ),
            });
        }
        Err(ToolError::Rejected {
            error: format!("{}. nothing this call did was kept", what),
        })
    }

    /// What went wrong, in words the model can act on.
    fn describe(&self, err: SqlError) -> String {
        match err {
            SqlError::Sql(message) => message,
            SqlError::TimedOut => format!(
                "the call ran for longer than {}s and was stopped",
                self.settings.sqlite_timeout
            ),
        }
    }

    /// A connection to the database called `name`, opening it if this is the
    /// first call to need it, or an error naming the ones that do exist.
    fn open(&mut self, name: &str) -> Result<Arc<Connection>, ToolError> {
        let path = self.path(name);
        let opened = match self.databases.get(name) {
            None => return Err(self.not_open(name)),
            Some(Some(database)) => return Ok(Arc::clone(&database.connection)),
            Some(None) => self.open_file(&path, OpenFlags::None)?,
        };
        let connection = Arc::clone(&opened.connection);
        self.databases.insert(name.to_string(), Some(opened));
        Ok(connection)
    }

    fn open_file(&self, path: &Path, flags: OpenFlags) -> Result<OpenDatabase, ToolError> {
        let path_text = path.to_str().ok_or_else(|| ToolError::Failed {
            error: format!("{} is not valid utf-8", path.display()),
        })?;
        let database = Database::open(
            self.io.clone(),
            path_text,
            OpenOptions::new(Arc::new(SqliteDialect)).flags(flags),
        )
        .map_err(open_failed)?;
        let connection = database.connect().map_err(open_failed)?;
        Ok(OpenDatabase {
            _database: database,
            connection,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.db", name))
    }

    /// Says that `name` is not a database and names the ones that are, so the
    /// model can pick one rather than guess again.
    fn not_open(&self, name: &str) -> ToolError {
        let open = if self.databases.is_empty() {
            "there are none; create one first".to_string()
        } else {
            format!(
                "the databases are: {}",
                self.databases
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        ToolError::InvalidArgument {
            argument: "database".into(),
            expected: format!("an existing database; `{}` is not one, and {}", name, open),
        }
    }
}

enum Stopped {
    AtEnd,
    AtMaxRows,
    AtOutputLimit,
}

impl Stopped {
    /// The footer for a query that kept `kept` rows before stopping.
    fn footer(&self, kept: usize) -> String {
        match self {
            Stopped::AtEnd => format!("{} {}", kept, plural(kept as i64, "row")),
            Stopped::AtMaxRows => format!("showing the first {} rows; there are more", kept),
            Stopped::AtOutputLimit => format!(
                "showing the first {} {}; there are more that did not fit in the output, \
                 so select fewer columns or rows",
                kept,
                plural(kept as i64, "row")
            ),
        }
    }
}

// Running sql ///////////////////////
//////////////////////////////////////

/// Why sql the call sent did not run to the end.
enum SqlError {
    /// What the sql asked for could not be done; turso's own message.
    Sql(String),
    /// The call outran its timeout and turso stopped it part way through.
    TimedOut,
}

impl From<LimboError> for SqlError {
    fn from(err: LimboError) -> SqlError {
        SqlError::Sql(err.to_string())
    }
}

/// When a call has to be finished by. One deadline covers every statement a
/// call runs, so a script of many quick statements is bounded the same as one
/// slow one.
struct Deadline(Instant);

impl Deadline {
    fn after(timeout: Duration) -> Deadline {
        Deadline(Instant::now() + timeout)
    }

    /// What is left of the call's time, for the next statement to run in.
    fn remaining(&self) -> Result<Duration, SqlError> {
        let left = self.0.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(SqlError::TimedOut);
        }
        Ok(left)
    }
}

/// Prepares one statement and binds `params`, with turso's own timeout armed
/// so even a single long step can be stopped part way.
fn prepare(
    connection: &Arc<Connection>,
    sql: &str,
    params: Vec<Value>,
    deadline: &Deadline,
) -> Result<Statement, SqlError> {
    let mut statement = connection.prepare(sql)?;
    bind(&mut statement, params)?;
    statement.set_query_timeout_override(Some(Some(deadline.remaining()?)));
    Ok(statement)
}

fn bind(statement: &mut Statement, params: Vec<Value>) -> Result<(), SqlError> {
    for (index, value) in params.into_iter().enumerate() {
        let position = NonZero::new(index + 1).expect("one or more");
        statement.bind_at(position, value)?;
    }
    Ok(())
}

/// Steps `statement` to its next row: true if there is one, false once done.
/// File io is driven inline, and a wait turso asks for is slept rather than
/// spun.
fn step(statement: &mut Statement) -> Result<bool, SqlError> {
    loop {
        match statement.step()? {
            StepResult::Row => return Ok(true),
            StepResult::Done => return Ok(false),
            StepResult::IO | StepResult::Yield => statement._io().step()?,
            StepResult::Sleep { duration } => {
                thread::sleep(duration);
                statement._io().step()?
            }
            StepResult::Interrupt => return Err(SqlError::TimedOut),
            // only this agent ever holds a connection to its databases, so
            // nothing else can be holding the lock
            StepResult::Busy => return Err(SqlError::Sql("the database is locked".into())),
        }
    }
}

/// Runs every statement in `sql`, one after another, to the end. Rows a
/// statement produces are read past and dropped: a script is for changing
/// things, and `query` is for reading them.
fn run_script(
    connection: &Arc<Connection>,
    sql: &str,
    deadline: &Deadline,
) -> Result<(), SqlError> {
    let mut rest = sql;
    while let Some((mut statement, end)) = connection.consume_stmt(rest)? {
        statement.set_query_timeout_override(Some(Some(deadline.remaining()?)));
        while step(&mut statement)? {}
        rest = &rest[end..];
    }
    Ok(())
}

/// Runs one of the statements the tool wraps a call in. These are never put
/// under the call's timeout: a ROLLBACK that could itself be stopped part way
/// would leave the database in whatever state the call was stopped in.
fn run_control(connection: &Arc<Connection>, sql: &str) -> Result<(), LimboError> {
    let mut statement = connection.prepare(sql)?;
    statement.set_query_timeout_override(Some(None));
    loop {
        match statement.step()? {
            StepResult::Done => return Ok(()),
            StepResult::Row | StepResult::Yield => {}
            StepResult::IO | StepResult::Sleep { .. } => statement._io().step()?,
            StepResult::Interrupt | StepResult::Busy => {
                return Err(LimboError::InternalError(format!(
                    "`{}` could not finish",
                    sql
                )))
            }
        }
    }
}

/// How many tables a database holds, for `list`. Not under a timeout: it reads
/// one small table.
fn count_tables(connection: &Arc<Connection>) -> Result<i64, LimboError> {
    let mut statement = connection.prepare(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    statement.set_query_timeout_override(Some(None));
    let mut count = 0;
    loop {
        match statement.step()? {
            StepResult::Row => {
                if let Some(Value::Numeric(Numeric::Integer(value))) =
                    statement.row().and_then(|row| row.get_values().next())
                {
                    count = *value;
                }
            }
            StepResult::Done => return Ok(count),
            StepResult::IO | StepResult::Yield | StepResult::Sleep { .. } => {
                statement._io().step()?
            }
            StepResult::Interrupt | StepResult::Busy => {
                return Err(LimboError::InternalError(
                    "counting tables could not finish".into(),
                ))
            }
        }
    }
}

// Checking calls ////////////////////
//////////////////////////////////////

/// Refuses sql the tool will not run, before any of it is: `VACUUM INTO`,
/// `ATTACH` and `DETACH`, which reach files other than the database, and —
/// when `single` — more than one statement.
fn check_sql(sql: &str, single: bool) -> Result<(), ToolError> {
    // whitespace is collapsed first so a newline between the two words does
    // not get past the check
    let normalised = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    if normalised.contains("VACUUM INTO") {
        return Err(ToolError::InvalidArgument {
            argument: "sql".into(),
            expected: "free of `VACUUM INTO`; a database cannot be written out to \
                       another file"
                .into(),
        });
    }
    if attaches(sql) {
        return Err(ToolError::InvalidArgument {
            argument: "sql".into(),
            expected: "free of `ATTACH` and `DETACH`; each call works on the one \
                       database it names"
                .into(),
        });
    }

    if single && count_statements(sql) > 1 {
        return Err(ToolError::InvalidArgument {
            argument: "sql".into(),
            expected: "a single statement here. `query` and `execute` with `params` run \
                       one statement; send a script to `execute` without `params`"
                .into(),
        });
    }
    Ok(())
}

/// How many statements `sql` holds. Sql that does not parse counts the
/// statement it failed on and stops there; turso reports what is wrong with it
/// when it is run.
fn count_statements(sql: &str) -> usize {
    let mut count = 0;
    for statement in turso_parser::parser::Parser::new(sql.as_bytes()) {
        count += 1;
        if statement.is_err() {
            break;
        }
    }
    count
}

/// Whether any statement in `sql` is an `ATTACH` or `DETACH`. Sql that does
/// not parse stops the search; turso refuses it when it is run.
fn attaches(sql: &str) -> bool {
    for statement in turso_parser::parser::Parser::new(sql.as_bytes()) {
        match statement {
            Ok(Cmd::Stmt(stmt) | Cmd::Explain(stmt) | Cmd::ExplainQueryPlan { stmt, .. }) => {
                if matches!(stmt, Stmt::Attach { .. } | Stmt::Detach { .. }) {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

/// A name that is safe to use as a file name: letters, digits, `_` or `-`
/// only, so it can never be read as a path.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LENGTH
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The `database` argument, checked to be a name rather than anything that
/// could be read as a path.
fn database_name(arguments: &serde_json::Value) -> Result<&str, ToolError> {
    let name = required_string(arguments, "database")?;
    if !valid_name(name) {
        return Err(ToolError::InvalidArgument {
            argument: "database".into(),
            expected: format!("1 to {} letters, digits, `_` or `-`", MAX_NAME_LENGTH),
        });
    }
    Ok(name)
}

/// The values to bind, or `None` when the call sent none. Kept apart from an
/// empty list because `execute` only runs a whole script when there is nothing
/// to bind.
fn params(arguments: &serde_json::Value) -> Result<Option<Vec<Value>>, ToolError> {
    let invalid = |expected: &str| ToolError::InvalidArgument {
        argument: "params".into(),
        expected: expected.into(),
    };
    let values = match arguments.get("params") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Array(values)) => values,
        Some(_) => return Err(invalid("an array")),
    };
    values
        .iter()
        .map(|value| match value {
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::Bool(value) => Ok(Value::Numeric(Numeric::Integer(*value as i64))),
            serde_json::Value::Number(number) => {
                if let Some(integer) = number.as_i64() {
                    return Ok(Value::Numeric(Numeric::Integer(integer)));
                }
                // an integer too big for sqlite would otherwise come back as the
                // nearest float, and an id that is off by a few is worse than
                // one that was refused
                if number.is_f64() {
                    // json has no NaN, so every float it carries is one turso takes
                    let float = NonNan::new(number.as_f64().expect("is f64"))
                        .expect("json numbers are never NaN");
                    return Ok(Value::Numeric(Numeric::Float(float)));
                }
                Err(invalid(
                    "made of integers that fit in 64 bits; pass a larger one as a string",
                ))
            }
            serde_json::Value::String(text) => Ok(Value::build_text(text.clone())),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                Err(invalid("an array of strings, numbers, booleans and nulls"))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

// Output ////////////////////////////
//////////////////////////////////////

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        // the bytes themselves are no use to the model on a line of text
        Value::Blob(blob) => format!("<blob {} bytes>", blob.len()),
        // numbers and text read the way sqlite itself would print them
        value => value.to_string(),
    }
}

/// Cuts `content` down to [`MAX_OUTPUT_BYTES`] and says how much was left out.
/// Query output is already kept under the limit row by row; this is what keeps
/// everything else from running over.
fn truncate(content: String) -> String {
    if content.len() <= MAX_OUTPUT_BYTES {
        return content;
    }
    cut(content, MAX_OUTPUT_BYTES)
}

/// Cuts `content` to at most `limit` bytes, on a character boundary, and says
/// how much was left out.
pub(crate) fn cut(mut content: String, limit: usize) -> String {
    if content.len() <= limit {
        return content;
    }
    let mut end = limit;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = content.len() - end;
    content.truncate(end);
    content.push_str(&format!(" … truncated, {} more bytes", dropped));
    content
}

fn plural(count: i64, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{}s", noun)
    }
}

/// Something went wrong that the call is not to blame for: the database would
/// not open, or the transaction around the call would not start.
fn failed(err: impl std::fmt::Display) -> ToolError {
    ToolError::Failed {
        error: err.to_string(),
    }
}

/// A database that would not open. Another apila process holding the file is
/// worth saying in words, since nothing about the call is wrong.
fn open_failed(err: LimboError) -> ToolError {
    match err {
        LimboError::LockingError(_) => ToolError::Failed {
            error: "the database is in use by another apila process".into(),
        },
        err => failed(err),
    }
}
