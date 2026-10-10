use std::path::PathBuf;

use super::sqlite::{SqliteTool, MAX_OUTPUT_BYTES};
use super::tool::{Tool, ToolContext, ToolError, ToolState};
use crate::core::runtime::agent_config::ToolSettings;
use crate::core::runtime::tool_registry::ToolRegistry;

/// A started sqlite tool, configured with `config`. The tool never touches
/// disk, so the directory it is handed does not need to exist.
fn state(config: serde_json::Value) -> (ToolContext, Box<dyn ToolState>) {
    let context = ToolContext::new(PathBuf::from("/nonexistent"), config);
    let state = SqliteTool::new()
        .new_state(&context)
        .expect("start the tool");
    (context, state)
}

fn call(
    context: &ToolContext,
    state: &mut Box<dyn ToolState>,
    arguments: serde_json::Value,
) -> Result<String, ToolError> {
    state
        .run(context, &arguments)
        .map(|output| output.content())
}

fn ok(
    context: &ToolContext,
    state: &mut Box<dyn ToolState>,
    arguments: serde_json::Value,
) -> String {
    call(context, state, arguments).expect("the call succeeds")
}

fn err(
    context: &ToolContext,
    state: &mut Box<dyn ToolState>,
    arguments: serde_json::Value,
) -> ToolError {
    call(context, state, arguments).expect_err("the call fails")
}

/// A started tool with one database, `db`, holding a small `people` table.
fn with_people() -> (ToolContext, Box<dyn ToolState>) {
    let (context, mut state) = state(serde_json::json!({}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
                    INSERT INTO people (name, age) VALUES ('ada', 36);
                    INSERT INTO people (name, age) VALUES ('alan', 41);"
        }),
    );
    (context, state)
}

#[test]
fn a_script_loads_a_table_that_a_query_reads_back() {
    let (context, mut state) = with_people();

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT name, age FROM people ORDER BY id"
        }),
    );
    assert_eq!(content, "name | age\nada | 36\nalan | 41\n2 rows");
}

#[test]
fn params_are_bound_by_position() {
    let (context, mut state) = with_people();

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "INSERT INTO people (name, age) VALUES (?1, ?2)",
            "params": ["grace", 85]
        }),
    );
    assert_eq!(content, "ok, 1 row changed");

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT age FROM people WHERE name = ?1",
            "params": ["grace"]
        }),
    );
    assert_eq!(content, "age\n85\n1 row");
}

#[test]
fn every_kind_of_param_reads_back_as_itself() {
    let (context, mut state) = with_people();

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT ?1 AS n, ?2 AS b, ?3 AS f, ?4 AS s",
            "params": [null, true, 1.5, "text"]
        }),
    );
    assert_eq!(content, "n | b | f | s\nNULL | 1 | 1.5 | text\n1 row");
}

#[test]
fn a_nested_param_is_refused() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT ?1",
            "params": [[1, 2]]
        }),
    );
    assert!(
        matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "params")
    );
}

#[test]
fn databases_do_not_see_each_others_tables() {
    let (context, mut state) = with_people();
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "other"}),
    );

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "other", "sql": "SELECT * FROM people"}),
    );
    assert!(matches!(error, ToolError::Rejected { .. }), "{error:?}");
}

#[test]
fn list_names_every_database_and_how_many_tables_it_has() {
    let (context, mut state) = state(serde_json::json!({}));
    assert_eq!(
        ok(&context, &mut state, serde_json::json!({"action": "list"})),
        "(no databases)"
    );

    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "b"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "a"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "execute", "database": "a", "sql": "CREATE TABLE t (x)"}),
    );

    assert_eq!(
        ok(&context, &mut state, serde_json::json!({"action": "list"})),
        "a (1 table)\nb (0 tables)"
    );
}

#[test]
fn schema_shows_the_sql_each_table_was_made_with() {
    let (context, mut state) = with_people();

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "schema", "database": "db"}),
    );
    assert!(content.starts_with("type | name | sql\n"), "{content}");
    assert!(
        content.contains("table | people | CREATE TABLE people"),
        "{content}"
    );
}

#[test]
fn a_dropped_database_is_gone_and_the_error_names_what_is_left() {
    let (context, mut state) = with_people();
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "kept"}),
    );
    assert_eq!(
        ok(
            &context,
            &mut state,
            serde_json::json!({"action": "drop", "database": "db"})
        ),
        "dropped `db`"
    );

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1"}),
    );
    assert!(
        error.to_string().contains("the open databases are: kept"),
        "{error}"
    );
}

#[test]
fn a_name_cannot_be_created_twice() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn a_name_that_could_be_a_path_is_refused() {
    let (context, mut state) = state(serde_json::json!({}));

    for name in ["", "../escape", "a b", "x.db"] {
        let error = err(
            &context,
            &mut state,
            serde_json::json!({"action": "create", "database": name}),
        );
        assert!(
            matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "database"),
            "{name}: {error:?}"
        );
    }
}

#[test]
fn missing_and_unknown_arguments_are_reported() {
    let (context, mut state) = with_people();

    assert!(matches!(
        err(&context, &mut state, serde_json::json!({})),
        ToolError::MissingArgument { ref argument } if argument == "action"
    ));
    assert!(matches!(
        err(&context, &mut state, serde_json::json!({"action": "create"})),
        ToolError::MissingArgument { ref argument } if argument == "database"
    ));
    assert!(matches!(
        err(&context, &mut state, serde_json::json!({"action": "query", "database": "db"})),
        ToolError::MissingArgument { ref argument } if argument == "sql"
    ));
    assert!(matches!(
        err(&context, &mut state, serde_json::json!({"action": "vacuum", "database": "db"})),
        ToolError::InvalidArgument { ref argument, .. } if argument == "action"
    ));
}

#[test]
fn no_more_than_max_databases_can_be_open() {
    let (context, mut state) = state(serde_json::json!({"max_databases": 1}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "one"}),
    );

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "two"}),
    );
    assert!(matches!(error, ToolError::Rejected { .. }), "{error:?}");
}

#[test]
fn rows_past_max_rows_are_left_out_and_the_model_is_told() {
    let (context, mut state) = state(serde_json::json!({"max_rows": 2}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "CREATE TABLE t (x); INSERT INTO t VALUES (1); INSERT INTO t VALUES (2); INSERT INTO t VALUES (3);"
        }),
    );

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT x FROM t ORDER BY x"}),
    );
    assert_eq!(content, "x\n1\n2\nshowing the first 2 rows; there are more");
}

#[test]
fn a_long_first_row_is_cut_on_a_character_boundary() {
    let (context, mut state) = state(serde_json::json!({}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );

    // a two byte character throughout, so a cut at an even byte count would
    // land inside one if the boundary were not looked for
    let content = ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT ?1 AS v",
            "params": ["é".repeat(MAX_OUTPUT_BYTES)]
        }),
    );
    assert!(content.len() <= MAX_OUTPUT_BYTES, "{}", content.len());
    assert!(
        content.contains("… truncated, "),
        "{}",
        &content[content.len() - 80..]
    );
    assert!(
        content.ends_with("\n1 row"),
        "{}",
        &content[content.len() - 80..]
    );
}

#[test]
fn rows_that_do_not_fit_are_left_out_but_the_footer_is_kept() {
    let (context, mut state) = state(serde_json::json!({}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "CREATE TABLE t (x TEXT);
                    INSERT INTO t SELECT printf('%.200c', 'x') FROM generate_series(1, 100);"
        }),
    );

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT x FROM t"}),
    );
    assert!(content.len() <= MAX_OUTPUT_BYTES, "{}", content.len());
    let footer = content.lines().last().expect("a footer");
    assert!(footer.starts_with("showing the first "), "{footer}");
    assert!(footer.contains("did not fit in the output"), "{footer}");
}

#[test]
fn max_rows_rows_exactly_are_all_shown() {
    let (context, mut state) = state(serde_json::json!({"max_rows": 2}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1 UNION ALL SELECT 2"}),
    );
    assert_eq!(content, "1\n1\n2\n2 rows");
}

#[test]
fn bad_sql_is_rejected_with_turso_s_message() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT * FROM nowhere"}),
    );
    assert!(matches!(error, ToolError::Rejected { .. }), "{error:?}");
    assert!(error.to_string().contains("nowhere"), "{error}");
}

#[test]
fn a_script_that_fails_part_way_keeps_none_of_it() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "CREATE TABLE pets (name TEXT);
                    INSERT INTO people (name, age) VALUES ('grace', 85);
                    INSERT INTO nowhere VALUES (1);"
        }),
    );
    assert!(matches!(error, ToolError::Rejected { .. }), "{error:?}");
    assert!(
        error.to_string().contains("nothing this call did was kept"),
        "{error}"
    );

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT count(*) FROM people"}),
    );
    assert_eq!(content, "count(*)\n2\n1 row");
    let schema = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "schema", "database": "db"}),
    );
    assert!(!schema.contains("pets"), "{schema}");
}

#[test]
fn sql_that_ends_the_transaction_itself_is_reported() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "INSERT INTO people (name, age) VALUES ('grace', 85); COMMIT;"
        }),
    );
    assert!(
        error
            .to_string()
            .contains("do not write BEGIN, COMMIT or ROLLBACK"),
        "{error}"
    );

    // the next call still gets a transaction of its own
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1"}),
    );
}

#[test]
fn several_statements_with_params_are_refused_before_any_run() {
    let (context, mut state) = with_people();

    // an empty `params` is still `params`: turso would bind it to the first
    // statement and skip the rest without saying so
    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "DELETE FROM people; DROP TABLE people;",
            "params": []
        }),
    );
    assert!(matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "sql"));

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT count(*) FROM people"}),
    );
    assert_eq!(content, "count(*)\n2\n1 row");
}

#[test]
fn a_query_with_several_statements_is_refused() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1; DROP TABLE people"}),
    );
    assert!(matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "sql"));
}

#[test]
fn a_single_statement_with_a_trailing_semicolon_is_one_statement() {
    let (context, mut state) = with_people();

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1 AS one;  "}),
    );
    assert_eq!(content, "one\n1\n1 row");
}

#[test]
fn vacuum_into_is_refused_however_it_is_written() {
    let (context, mut state) = with_people();

    for sql in [
        "VACUUM INTO '/tmp/apila-escape.db'",
        "vacuum\n  into '/tmp/apila-escape.db'",
        "CREATE TABLE t (x); VACUUM INTO '/tmp/apila-escape.db'",
    ] {
        let error = err(
            &context,
            &mut state,
            serde_json::json!({"action": "execute", "database": "db", "sql": sql}),
        );
        assert!(
            matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "sql"),
            "{sql}: {error:?}"
        );
    }
    assert!(!std::path::Path::new("/tmp/apila-escape.db").exists());
}

#[test]
fn an_integer_too_big_for_sqlite_is_refused() {
    let (context, mut state) = with_people();

    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "query",
            "database": "db",
            "sql": "SELECT ?1",
            "params": [u64::MAX]
        }),
    );
    assert!(
        matches!(error, ToolError::InvalidArgument { ref argument, .. } if argument == "params")
    );
}

#[test]
fn dropping_a_database_that_is_not_open_is_an_error() {
    let (context, mut state) = state(serde_json::json!({}));

    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "drop", "database": "db"}),
    );
    assert!(
        error
            .to_string()
            .contains("there are none; create one first"),
        "{error}"
    );
}

#[test]
fn config_is_checked() {
    let tool = SqliteTool::new();
    assert!(tool.validate_config(&serde_json::json!({})).is_ok());
    assert!(tool
        .validate_config(&serde_json::json!({"max_databases": 2, "max_rows": 10}))
        .is_ok());
    assert!(tool
        .validate_config(&serde_json::json!({"max_rows": 0}))
        .is_err());
    assert!(tool
        .validate_config(&serde_json::json!({"max_databases": 0}))
        .is_err());
    assert!(tool
        .validate_config(&serde_json::json!({"max_row": 5}))
        .is_err());
    assert!(tool
        .validate_config(&serde_json::json!({"sqlite_timeout": 0}))
        .is_err());
}

#[test]
fn the_default_registry_offers_sqlite() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&ToolSettings {
            enabled: vec!["sqlite".to_string()],
            configs: Vec::new(),
        })
        .expect("resolve");
    assert_eq!(
        tools.names(),
        vec!["end_turn".to_string(), "sqlite".to_string()]
    );
}

/// A recursive query with no end, which only a timeout ever stops.
const ENDLESS: &str =
    "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) SELECT count(*) FROM n";

#[test]
fn a_query_that_runs_too_long_is_stopped() {
    let (context, mut state) = state(serde_json::json!({"sqlite_timeout": 1}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );

    let started = std::time::Instant::now();
    let error = err(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": ENDLESS}),
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(error, ToolError::Rejected { .. }), "{error:?}");
    assert!(error.to_string().contains("longer than 1s"), "{error}");

    // the database is still there and still usable once the call is stopped
    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT 1 AS one"}),
    );
    assert_eq!(content, "one\n1\n1 row");
}

#[test]
fn a_script_stopped_part_way_keeps_none_of_it() {
    let (context, mut state) = state(serde_json::json!({"sqlite_timeout": 1}));
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "create", "database": "db"}),
    );
    ok(
        &context,
        &mut state,
        serde_json::json!({"action": "execute", "database": "db", "sql": "CREATE TABLE t (x)"}),
    );

    let error = err(
        &context,
        &mut state,
        serde_json::json!({
            "action": "execute",
            "database": "db",
            "sql": "INSERT INTO t VALUES (1);
                    INSERT INTO t WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) SELECT x FROM n;"
        }),
    );
    assert!(error.to_string().contains("longer than 1s"), "{error}");
    assert!(
        error.to_string().contains("nothing this call did was kept"),
        "{error}"
    );

    let content = ok(
        &context,
        &mut state,
        serde_json::json!({"action": "query", "database": "db", "sql": "SELECT count(*) AS n FROM t"}),
    );
    assert_eq!(content, "n\n0\n1 row");
}
