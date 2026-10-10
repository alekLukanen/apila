use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{env, fs, thread};

use reqwest::Url;

use super::tool::{Tool, ToolContext, ToolError, ToolState};
use super::webpage::{
    check_url, display_lines, is_private, next_free_page, page_path, parse_page_id, save_page,
    FetchWebpageTool, ReadWebpageDataTool, MAX_LINE_CHARS, MAX_REDIRECTS, PAGES_DIR,
};
use crate::core::runtime::agent_config::ToolSettings;
use crate::core::runtime::tool_registry::ToolRegistry;

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Creates a unique temp directory for a single test to work in.
fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "apila-test-webpage-{}-{}-{}",
        name,
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn pages(dir: &Path) -> PathBuf {
    dir.join(PAGES_DIR)
}

/// Every file in the pages directory, temp files included, sorted by name.
fn files_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(pages(dir)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("utf8")
        })
        .collect();
    names.sort();
    names
}

/// Answers each connection with the next raw response, repeating the last.
fn serve(responses: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let addr = listener.local_addr().expect("stub address");
    thread::spawn(move || {
        for (served, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { break };
            drain_request(&stream);
            let response = &responses[served.min(responses.len() - 1)];
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{}", addr)
}

/// The request has to be read before replying or the client may see a reset.
fn drain_request(stream: &std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
    }
}

fn response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    )
}

fn html(body: &str) -> String {
    response("200 OK", "text/html; charset=utf-8", body)
}

/// The stub server is on loopback, which the tool refuses unless told not to.
fn local() -> serde_json::Value {
    serde_json::json!({"allow_private_hosts": true})
}

fn fetcher(dir: &Path, config: serde_json::Value) -> (ToolContext, Box<dyn ToolState>) {
    let context = ToolContext::new(dir.to_path_buf(), config);
    let state = FetchWebpageTool::new()
        .new_state(&context)
        .expect("start the tool");
    (context, state)
}

fn fetch(dir: &Path, config: serde_json::Value, url: &str) -> Result<String, ToolError> {
    let (context, mut state) = fetcher(dir, config);
    state
        .run(&context, &serde_json::json!({ "url": url }))
        .map(|output| output.content())
}

fn read(
    dir: &Path,
    config: serde_json::Value,
    arguments: serde_json::Value,
) -> Result<String, ToolError> {
    let context = ToolContext::new(dir.to_path_buf(), config);
    let mut state = ReadWebpageDataTool::new()
        .new_state(&context)
        .expect("start the tool");
    state
        .run(&context, &arguments)
        .map(|output| output.content())
}

/// Saves `body` as the next page in `dir`, the way a fetch would.
fn saved(dir: &Path, body: &[u8]) -> String {
    format!("page_{}", save_page(&pages(dir), body).expect("save page"))
}

fn numbered(count: usize) -> String {
    (1..=count).map(|line| format!("line {}\n", line)).collect()
}

// Page ids //////////////////////////
//////////////////////////////////////

#[test]
fn only_well_formed_page_ids_are_accepted() {
    assert_eq!(parse_page_id("page_1"), Some(1));
    assert_eq!(parse_page_id("page_42"), Some(42));
    assert_eq!(parse_page_id("page_999999999"), Some(999_999_999));

    for id in [
        "",
        "page_",
        "page_0",
        "page_01",
        "page_abc",
        "page_1.dat",
        "../page_1",
        "page_-1",
        "page_1/../x",
        "Page_1",
        "page_1000000000",
        "page_99999999999999999999",
    ] {
        assert_eq!(parse_page_id(id), None, "{}", id);
    }
}

// Numbering /////////////////////////
//////////////////////////////////////

/// Around each power of two is where a galloping search goes wrong.
#[test]
fn the_next_page_follows_the_last_one_however_many_there_are() {
    for count in [0u32, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 100] {
        let dir = temp_dir("numbering");
        fs::create_dir_all(pages(&dir)).expect("pages dir");
        for number in 1..=count {
            fs::write(page_path(&pages(&dir), number), "x").expect("write page");
        }
        assert_eq!(
            next_free_page(&pages(&dir), 1),
            Some(count + 1),
            "after {} pages",
            count
        );
    }
}

#[test]
fn saving_numbers_pages_in_order_and_leaves_no_temp_files() {
    let dir = temp_dir("save-order");
    assert_eq!(saved(&dir, b"one"), "page_1");
    assert_eq!(saved(&dir, b"two"), "page_2");
    assert_eq!(saved(&dir, b"three"), "page_3");

    assert_eq!(
        files_in(&dir),
        vec!["page_1.dat", "page_2.dat", "page_3.dat"]
    );
    assert_eq!(
        fs::read(page_path(&pages(&dir), 2)).expect("read page"),
        b"two"
    );
}

/// Writers racing for the same number is what the claim's retry is for.
#[test]
fn pages_saved_at_once_never_share_a_number() {
    let dir = temp_dir("race");
    let writers: Vec<_> = (0..16)
        .map(|writer| {
            let pages = pages(&dir);
            thread::spawn(move || {
                save_page(&pages, format!("writer {}", writer).as_bytes()).expect("save")
            })
        })
        .collect();
    let mut numbers: Vec<u32> = writers
        .into_iter()
        .map(|writer| writer.join().expect("join"))
        .collect();
    numbers.sort();

    assert_eq!(numbers, (1..=16).collect::<Vec<u32>>());
    assert_eq!(files_in(&dir).len(), 16);
}

// Display lines /////////////////////
//////////////////////////////////////

#[test]
fn lines_drop_carriage_returns_and_the_final_newline() {
    assert_eq!(display_lines("a\r\nb\r\n"), vec!["a", "b"]);
    assert_eq!(display_lines("a\n\nb"), vec!["a", "", "b"]);
    assert!(display_lines("").is_empty());
}

#[test]
fn a_long_line_is_split_so_every_part_has_a_number() {
    let long = "é".repeat(MAX_LINE_CHARS * 2 + 7);
    let lines = display_lines(&long);

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].chars().count(), MAX_LINE_CHARS);
    assert_eq!(lines[2].chars().count(), 7);
    assert_eq!(lines.concat(), long);
}

// Private hosts /////////////////////
//////////////////////////////////////

#[test]
fn local_and_private_addresses_are_private() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "100.64.0.1",
        "::1",
        "::",
        "fd00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "64:ff9b::7f00:1",
        "64:ff9b::a9fe:a9fe",
        "2002:c0a8:101::1",
    ] {
        assert!(is_private(ip.parse::<IpAddr>().expect("ip")), "{}", ip);
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700::1111",
        "64:ff9b::808:808",
        "2002:808:808::1",
    ] {
        assert!(!is_private(ip.parse::<IpAddr>().expect("ip")), "{}", ip);
    }
}

#[test]
fn urls_are_checked_for_scheme_and_private_addresses() {
    let url = |raw: &str| Url::parse(raw).expect("url");

    assert!(check_url(&url("https://example.com/"), false).is_ok());
    assert!(check_url(&url("http://8.8.8.8/"), false).is_ok());
    assert!(check_url(&url("http://127.0.0.1:8080/"), false).is_err());
    assert!(check_url(&url("http://[::1]/"), false).is_err());
    assert!(check_url(&url("http://169.254.169.254/latest"), false).is_err());
    assert!(check_url(&url("http://127.0.0.1:8080/"), true).is_ok());
    assert!(check_url(&url("ftp://example.com/"), true).is_err());
    assert!(check_url(&url("file:///etc/passwd"), true).is_err());
}

// Fetching //////////////////////////
//////////////////////////////////////

#[test]
fn a_fetched_page_is_saved_byte_for_byte_and_named() {
    let dir = temp_dir("fetch");
    let body = "<html>\n<body>\n<h1>hello</h1>\n</body>\n</html>\n";
    let base = serve(vec![html(body)]);

    let content = fetch(&dir, local(), &base).expect("fetch");

    assert!(content.contains("saved as page_1"), "{}", content);
    assert!(content.contains("status: 200 OK"), "{}", content);
    assert!(content.contains("text/html; charset=utf-8"), "{}", content);
    assert!(
        content.contains(&format!("size: {} bytes", body.len())),
        "{}",
        content
    );
    assert!(content.contains("lines: 5 raw, 1 as text"), "{}", content);
    assert!(content.contains("view: \"outline\""), "{}", content);
    assert!(
        !content.contains("hello"),
        "the body is not returned: {}",
        content
    );
    assert_eq!(
        fs::read_to_string(page_path(&pages(&dir), 1)).expect("read page"),
        body
    );
}

#[test]
fn each_fetch_gets_the_next_page() {
    let dir = temp_dir("fetch-twice");
    let base = serve(vec![html("first"), html("second")]);

    assert!(fetch(&dir, local(), &base)
        .expect("fetch")
        .contains("page_1"));
    assert!(fetch(&dir, local(), &base)
        .expect("fetch")
        .contains("page_2"));
    assert_eq!(
        fs::read_to_string(page_path(&pages(&dir), 2)).expect("read page"),
        "second"
    );
}

/// An error page is still what the server said, and worth reading.
#[test]
fn an_error_status_is_saved_and_reported() {
    let dir = temp_dir("not-found");
    let base = serve(vec![response("404 Not Found", "text/html", "gone")]);

    let content = fetch(&dir, local(), &base).expect("fetch");

    assert!(content.contains("status: 404 Not Found"), "{}", content);
    assert_eq!(files_in(&dir), vec!["page_1.dat"]);
}

#[test]
fn a_redirect_is_followed_and_the_final_url_reported() {
    let dir = temp_dir("redirect");
    let base = serve(vec![redirect("/landed"), html("here")]);

    let content = fetch(&dir, local(), &base).expect("fetch");

    assert!(
        content.contains(&format!("redirected to: {}/landed", base)),
        "{}",
        content
    );
}

fn redirect(location: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nlocation: {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        location
    )
}

#[test]
fn up_to_the_redirect_limit_is_followed() {
    let dir = temp_dir("redirect-limit");
    let mut responses = vec![redirect("/again"); MAX_REDIRECTS];
    responses.push(html("here"));
    let base = serve(responses);

    let content = fetch(&dir, local(), &base).expect("fetch");

    assert!(content.contains("saved as page_1"), "{}", content);
}

#[test]
fn too_many_redirects_fail_without_saving() {
    let dir = temp_dir("redirect-loop");
    let base = serve(vec![redirect("/again")]);

    let err = fetch(&dir, local(), &base).expect_err("too many redirects");

    assert!(matches!(err, ToolError::Failed { .. }), "{}", err);
    assert!(err.to_string().contains("redirects"), "{}", err);
    assert!(files_in(&dir).is_empty());
}

#[test]
fn a_page_over_the_limit_is_cut_and_says_so() {
    let dir = temp_dir("oversize");
    let base = serve(vec![html(&"x".repeat(100))]);

    let content = fetch(
        &dir,
        serde_json::json!({"allow_private_hosts": true, "max_page_bytes": 10}),
        &base,
    )
    .expect("fetch");

    assert!(content.contains("size: 10 bytes, cut short"), "{}", content);
    assert!(content.contains("the server sent 100 bytes"), "{}", content);
    assert_eq!(
        fs::read(page_path(&pages(&dir), 1))
            .expect("read page")
            .len(),
        10
    );
}

#[test]
fn a_binary_body_is_saved_and_reported_as_binary() {
    let dir = temp_dir("binary-fetch");
    let base = serve(vec![response(
        "200 OK",
        "application/octet-stream",
        "ab\0cd",
    )]);

    let content = fetch(&dir, local(), &base).expect("fetch");

    assert!(content.contains("the body is binary"), "{}", content);
}

#[test]
fn a_bad_url_is_the_calls_fault() {
    let dir = temp_dir("bad-url");
    let (context, mut state) = fetcher(&dir, local());

    let missing = state
        .run(&context, &serde_json::json!({}))
        .expect_err("missing");
    assert!(
        matches!(missing, ToolError::MissingArgument { .. }),
        "{}",
        missing
    );

    let garbled = state
        .run(&context, &serde_json::json!({"url": "not a url"}))
        .expect_err("garbled");
    assert!(
        matches!(garbled, ToolError::InvalidArgument { .. }),
        "{}",
        garbled
    );

    let scheme = state
        .run(&context, &serde_json::json!({"url": "ftp://example.com/x"}))
        .expect_err("scheme");
    assert!(matches!(scheme, ToolError::Rejected { .. }), "{}", scheme);
}

#[test]
fn a_private_address_is_refused_by_default() {
    let dir = temp_dir("private");
    let base = serve(vec![html("secret")]);

    let err = fetch(&dir, serde_json::json!({}), &base).expect_err("refused");

    assert!(matches!(err, ToolError::Rejected { .. }), "{}", err);
    assert!(files_in(&dir).is_empty());
}

/// A name is only known to be local once it resolves, so this is the
/// resolver's refusal rather than the url check's.
#[test]
fn a_name_that_resolves_to_a_private_address_is_refused() {
    let dir = temp_dir("private-name");
    let base = serve(vec![html("secret")]);
    let port = base.rsplit(':').next().expect("port");

    let err = fetch(
        &dir,
        serde_json::json!({}),
        &format!("http://localhost:{}/", port),
    )
    .expect_err("refused");

    assert!(matches!(err, ToolError::Rejected { .. }), "{}", err);
    assert!(err.to_string().contains("localhost"), "{}", err);
    assert!(files_in(&dir).is_empty());
}

#[test]
fn an_unreachable_server_fails_without_saving() {
    let dir = temp_dir("unreachable");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("address");
    drop(listener);

    let err = fetch(&dir, local(), &format!("http://{}/", addr)).expect_err("unreachable");

    assert!(matches!(err, ToolError::Failed { .. }), "{}", err);
    assert!(files_in(&dir).is_empty());
}

#[test]
fn a_slow_server_times_out_without_saving() {
    let dir = temp_dir("slow");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("address");
    thread::spawn(move || {
        let held: Vec<_> = listener.incoming().take(1).collect();
        thread::sleep(Duration::from_secs(5));
        drop(held);
    });

    let err = fetch(
        &dir,
        serde_json::json!({"allow_private_hosts": true, "fetch_timeout": 1}),
        &format!("http://{}/", addr),
    )
    .expect_err("timed out");

    assert!(matches!(err, ToolError::Failed { .. }), "{}", err);
    assert!(err.to_string().contains("timed out"), "{}", err);
    assert!(files_in(&dir).is_empty());
}

// Reading ///////////////////////////
//////////////////////////////////////

#[test]
fn lines_come_back_numbered_with_a_footer() {
    let dir = temp_dir("read");
    let page = saved(&dir, numbered(3).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert_eq!(
        content,
        "1: line 1\n2: line 2\n3: line 3\n-- lines 1–3 of 3"
    );
}

#[test]
fn a_range_returns_only_those_lines_and_where_to_continue() {
    let dir = temp_dir("range");
    let page = saved(&dir, numbered(10).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "start": 4, "end": 6}),
    )
    .expect("read");

    assert_eq!(
        content,
        "4: line 4\n5: line 5\n6: line 6\n-- lines 4–6 of 10; continue with start=7"
    );
}

#[test]
fn the_range_ends_at_whichever_of_end_and_limit_comes_first() {
    let dir = temp_dir("limit");
    let page = saved(&dir, numbered(10).as_bytes());

    let by_limit = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "start": 2, "end": 9, "limit": 2}),
    )
    .expect("read");
    assert!(
        by_limit.ends_with("-- lines 2–3 of 10; continue with start=4"),
        "{}",
        by_limit
    );

    let by_end = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "start": 2, "end": 3, "limit": 5}),
    )
    .expect("read");
    assert!(
        by_end.ends_with("-- lines 2–3 of 10; continue with start=4"),
        "{}",
        by_end
    );
}

#[test]
fn a_limit_over_the_configured_most_is_lowered_and_noted() {
    let dir = temp_dir("max-lines");
    let page = saved(&dir, numbered(10).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({"max_lines": 3}),
        serde_json::json!({"page": page, "limit": 50}),
    )
    .expect("read");

    assert!(content.contains("3: line 3"), "{}", content);
    assert!(!content.contains("4: line 4"), "{}", content);
    assert!(content.contains("limit 50 was lowered"), "{}", content);
}

#[test]
fn without_a_limit_the_configured_most_still_applies() {
    let dir = temp_dir("default-limit");
    let page = saved(&dir, numbered(10).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({"max_lines": 4}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert!(
        content.ends_with("-- lines 1–4 of 10; continue with start=5"),
        "{}",
        content
    );
}

/// A minified page is one line, and none of it may be out of reach.
#[test]
fn every_part_of_a_minified_page_can_be_reached() {
    let dir = temp_dir("minified");
    let mut body = "a".repeat(MAX_LINE_CHARS * 2);
    body.push_str("THE END");
    let page = saved(&dir, body.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "start": 3}),
    )
    .expect("read");

    assert_eq!(content, "3: THE END\n-- lines 3–3 of 3");
}

#[test]
fn carriage_returns_and_invalid_utf8_do_not_get_in_the_way() {
    let dir = temp_dir("encoding");
    let page = saved(&dir, b"one\r\ntw\xffo\r\n");

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert_eq!(content, "1: one\n2: tw\u{fffd}o\n-- lines 1–2 of 2");
}

#[test]
fn a_binary_page_shows_no_lines() {
    let dir = temp_dir("binary-read");
    let page = saved(&dir, b"PNG\0\0garbage");

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert!(content.contains("is binary"), "{}", content);
    assert!(!content.contains("garbage"), "{}", content);
}

#[test]
fn reading_past_the_end_says_how_long_the_page_is() {
    let dir = temp_dir("past-end");
    let page = saved(&dir, numbered(2).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "start": 5}),
    )
    .expect("read");

    assert_eq!(content, "page_1 has 2 lines, so there is nothing from 5");
}

const DOCUMENT: &str = "<html><head><title>t</title></head><body>
<h1>Title</h1>
<p>intro <a href=\"/one\">one</a></p>
<h2>Section</h2>
<div class=\"price\">$5</div>
<div class=\"other\">no</div>
<a href=\"/two\">two</a>
<a href=\"/three\">three</a>
</body></html>";

#[test]
fn a_selector_returns_one_entry_per_matching_element() {
    let dir = temp_dir("selector");
    let page = saved(&dir, DOCUMENT.as_bytes());

    let links = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "a", "output": "html"}),
    )
    .expect("read");
    assert_eq!(
        links,
        "1: <a href=\"/one\">one</a>\n2: <a href=\"/two\">two</a>\n3: <a href=\"/three\">three</a>\n-- matches 1–3 of 3"
    );

    let headings = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "h1, h2"}),
    )
    .expect("read");
    assert!(headings.contains("1: Title"), "{}", headings);
    assert!(headings.contains("2: Section"), "{}", headings);

    let price = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "div.price"}),
    )
    .expect("read");
    assert_eq!(price, "1: $5\n-- matches 1–1 of 1");
}

#[test]
fn with_a_selector_the_range_counts_matches() {
    let dir = temp_dir("selector-range");
    let page = saved(&dir, DOCUMENT.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "a", "start": 2, "limit": 1}),
    )
    .expect("read");

    assert_eq!(
        content,
        "2: [two](/two)\n-- matches 2–2 of 3; continue with start=3"
    );
}

#[test]
fn a_match_longer_than_a_line_is_split_over_several_entries() {
    let dir = temp_dir("big-match");
    let body = format!("<html><body><p>{}</p></body></html>", "word ".repeat(200));
    let page = saved(&dir, body.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "body", "start": 2, "limit": 2}),
    )
    .expect("read");

    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3, "{}", content);
    assert!(lines[0].starts_with("2: "), "{}", content);
    assert_eq!(lines[0].chars().count(), "2: ".len() + MAX_LINE_CHARS);
    assert!(lines[1].starts_with("3: "), "{}", content);
    assert_eq!(
        lines[2],
        "-- entries 2–3 of 5; continue with start=4; long matches are split over several entries"
    );
}

#[test]
fn a_long_scripts_json_can_be_read_completely() {
    let dir = temp_dir("long-json");
    let items: Vec<String> = (0..100).map(|n| format!("{{\"id\": {}}}", n)).collect();
    let json = format!("{{\"items\": [{}]}}", items.join(", "));
    let body = format!(
        "<html><head><script id=\"data\" type=\"application/json\">{}</script></head>\
         <body><p>hi</p></body></html>",
        json
    );
    let page = saved(&dir, body.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "script#data"}),
    )
    .expect("read");

    let (entries, footer) = content.rsplit_once('\n').expect("a footer");
    let read_back: String = entries
        .lines()
        .enumerate()
        .map(|(index, line)| {
            line.strip_prefix(&format!("{}: ", index + 1))
                .expect("numbered entry")
        })
        .collect();
    assert_eq!(read_back, json);
    assert!(footer.starts_with("-- entries 1–"), "{}", footer);
}

#[test]
fn a_selector_that_does_not_parse_is_rejected() {
    let dir = temp_dir("bad-selector");
    let page = saved(&dir, DOCUMENT.as_bytes());

    let err = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "a[[["}),
    )
    .expect_err("bad selector");

    assert!(matches!(err, ToolError::Rejected { .. }), "{}", err);
}

const STORE: &str = "<!DOCTYPE html><html><head><title>The Store</title>
<style>body { color: red }</style>
<script>var tracking = 1;</script>
<script id=\"__NEXT_DATA__\" type=\"application/json\">{\"items\": [1, 2]}</script>
</head><body>
<nav><a href=\"/\">Home</a> <a href=\"#top\">Top</a> <a href=\"javascript:void(0)\">Menu</a></nav>
<h1>All   products</h1>
<p>Prices <b>today</b>, see <a href=\"/terms\">the terms</a>.</p>
<ul class=\"products\">
<li class=\"product card\"><h3>Widget</h3><span class=\"price\">$5</span></li>
<li class=\"product card\"><h3>Gadget</h3><span class=\"price\">$7</span></li>
<li class=\"product card\"><h3>Gizmo</h3><span class=\"price\">$9</span></li>
</ul>
<h2>Shipping</h2>
<table id=\"rates\"><tr><th>Zone</th><th>Cost</th></tr><tr><td>US</td><td>$1</td></tr></table>
<form id=\"search\" action=\"/find\"><input name=\"q\"><select name=\"sort\"></select></form>
<a class=\"promo\" href=\"/sale\"><div>Big sale</div><div>Today only</div></a>
<pre>keep
  this</pre>
<img src=\"/x.png\" alt=\"ignored\"><br>
</body></html>";

#[test]
fn an_html_page_reads_as_text_by_default() {
    let dir = temp_dir("text-view");
    let page = saved(&dir, STORE.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert_eq!(
        content,
        "1: [Home](/) Top Menu
2: # All products
3: Prices today, see [the terms](/terms).
4: - ### Widget
5: $5
6: - ### Gadget
7: $7
8: - ### Gizmo
9: $9
10: ## Shipping
11: Zone | Cost
12: US | $1
13: Big sale
14: Today only
15: (link: /sale)
16: keep
17:   this
-- lines 1–17 of 17"
    );
}

#[test]
fn the_raw_view_shows_the_source() {
    let dir = temp_dir("raw-view");
    let page = saved(&dir, STORE.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "view": "raw", "limit": 3}),
    )
    .expect("read");

    assert!(
        content.starts_with("1: <!DOCTYPE html><html><head><title>The Store</title>\n2: <style>"),
        "{}",
        content
    );
}

#[test]
fn a_selector_can_show_an_attribute_or_a_scripts_source() {
    let dir = temp_dir("attr-output");
    let page = saved(&dir, STORE.as_bytes());

    let hrefs = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "nav a", "output": "attr:href"}),
    )
    .expect("read");
    assert_eq!(
        hrefs,
        "1: /\n2: #top\n3: javascript:void(0)\n-- matches 1–3 of 3"
    );

    let missing = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "h1", "output": "attr:href"}),
    )
    .expect("read");
    assert_eq!(missing, "1: (no href)\n-- matches 1–1 of 1");

    let data = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "script#__NEXT_DATA__"}),
    )
    .expect("read");
    assert_eq!(data, "1: {\"items\": [1, 2]}\n-- matches 1–1 of 1");

    let rows = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "li.product"}),
    )
    .expect("read");
    assert_eq!(
        rows,
        "1: ### Widget / $5\n2: ### Gadget / $7\n3: ### Gizmo / $9\n-- matches 1–3 of 3"
    );
}

#[test]
fn the_outline_maps_the_page() {
    let dir = temp_dir("outline");
    let page = saved(&dir, STORE.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "view": "outline"}),
    )
    .expect("read");

    assert_eq!(
        content,
        "1: title: The Store
2: text view: 17 lines; links: 5; images: 1; tables: 1; forms: 1
3: headings:
4:   h1 All products
5:       h3 Widget
6:       h3 Gadget
7:       h3 Gizmo
8:     h2 Shipping
9: repeated elements (selector ×count: first one's text):
10:   li.product.card ×3: ### Widget / $5
11:   span.price ×3: $5
12: tables:
13:   table#rates (2 rows): Zone | Cost / US | $1
14: forms:
15:   form#search get /find fields: q, sort
16: embedded json:
17:   script#__NEXT_DATA__ (17 chars)
-- lines 1–17 of 17"
    );
}

#[test]
fn a_page_that_is_not_html_reads_raw_by_default() {
    let dir = temp_dir("not-html");
    let page = saved(&dir, b"{\n  \"a\":   1\n}\n");

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert_eq!(content, "1: {\n2:   \"a\":   1\n3: }\n-- lines 1–3 of 3");
}

#[test]
fn a_search_returns_only_the_hits_under_their_own_numbers() {
    let dir = temp_dir("search");
    let page = saved(&dir, numbered(20).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "search": "^line (3|4|9)$", "context": 1}),
    )
    .expect("read");
    assert_eq!(
        content,
        "2- line 2\n3: line 3\n4: line 4\n5- line 5\n…\n8- line 8\n9: line 9\n10- line 10\n\
         -- 3 of 3 hits shown; searched lines 1–20 of 20"
    );

    let limited = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "search": "1", "start": 5, "limit": 2}),
    )
    .expect("read");
    assert_eq!(
        limited,
        "10: line 10\n11: line 11\n-- 2 of 10 hits shown; searched lines 5–20 of 20; continue with start=12"
    );

    let none = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "search": "(?i)LINE 99", "end": 15}),
    )
    .expect("read");
    assert_eq!(none, "-- no hits; searched lines 1–15 of 20");
}

#[test]
fn a_hit_past_the_limit_is_still_marked_when_it_shows_as_context() {
    let dir = temp_dir("search-hit-in-context");
    let page = saved(&dir, numbered(20).as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "search": "^line (3|4|9)$", "context": 1, "limit": 1}),
    )
    .expect("read");

    assert_eq!(
        content,
        "2- line 2\n3: line 3\n4: line 4\n\
         -- 2 of 3 hits shown; searched lines 1–20 of 20; continue with start=5"
    );
}

#[test]
fn a_search_prints_at_most_the_configured_lines() {
    let dir = temp_dir("search-cap");
    let page = saved(&dir, numbered(20).as_bytes());
    let arguments = serde_json::json!({"page": page, "search": "^line (2|6|10|14)$", "context": 1});

    let capped = read(&dir, serde_json::json!({"max_lines": 7}), arguments.clone()).expect("read");
    assert_eq!(
        capped,
        "1- line 1\n2: line 2\n3- line 3\n…\n5- line 5\n6: line 6\n7- line 7\n\
         -- 2 of 4 hits shown; searched lines 1–20 of 20; continue with start=7"
    );

    let first_only = read(&dir, serde_json::json!({"max_lines": 2}), arguments).expect("read");
    assert_eq!(
        first_only,
        "1- line 1\n2: line 2\n3- line 3\n\
         -- 1 of 4 hits shown; searched lines 1–20 of 20; continue with start=3"
    );
}

#[test]
fn a_search_with_a_selector_counts_matches() {
    let dir = temp_dir("search-selector");
    let page = saved(&dir, STORE.as_bytes());

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "selector": "li", "search": "\\$[79]"}),
    )
    .expect("read");

    assert_eq!(
        content,
        "2: ### Gadget / $7\n3: ### Gizmo / $9\n-- 2 of 2 hits shown; searched matches 1–3 of 3"
    );
}

#[test]
fn a_search_that_does_not_parse_is_rejected() {
    let dir = temp_dir("bad-search");
    let page = saved(&dir, numbered(3).as_bytes());

    let err = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page, "search": "line (("}),
    )
    .expect_err("bad regex");

    assert!(matches!(err, ToolError::Rejected { .. }), "{}", err);
}

#[test]
fn a_list_item_marks_its_preformatted_text() {
    let dir = temp_dir("pre-in-li");
    let page = saved(
        &dir,
        b"<html><body><ul><li><pre>first\n  second</pre></li><li>next</li></ul></body></html>",
    );

    let content = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": page}),
    )
    .expect("read");

    assert_eq!(
        content,
        "1: - first\n2:   second\n3: - next\n-- lines 1–3 of 3"
    );
}

#[test]
fn a_deeply_nested_page_does_not_overflow_the_stack() {
    let dir = temp_dir("deep");
    let depth = 2_000;
    let body = format!(
        "<html><body>{}deep{}</body></html>",
        "<div><span>".repeat(depth),
        "</span></div>".repeat(depth)
    );
    let page = saved(&dir, body.as_bytes());

    for (arguments, expected) in [
        (serde_json::json!({"page": page}), "1: deep"),
        (
            serde_json::json!({"page": page, "view": "outline"}),
            "1: text view: 1 lines",
        ),
        (
            serde_json::json!({"page": page, "selector": "body"}),
            "1: deep",
        ),
    ] {
        let content = read(&dir, serde_json::json!({}), arguments).expect("read");
        assert!(content.starts_with(expected), "{}", content);
    }
}

#[test]
fn a_page_that_is_not_there_names_the_ones_that_are() {
    let dir = temp_dir("unknown-page");

    let none = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": "page_1"}),
    )
    .expect_err("no pages");
    assert!(
        none.to_string().contains("no pages have been fetched"),
        "{}",
        none
    );

    saved(&dir, b"one");
    saved(&dir, b"two");
    let err = read(
        &dir,
        serde_json::json!({}),
        serde_json::json!({"page": "page_9"}),
    )
    .expect_err("unknown page");
    assert!(matches!(err, ToolError::Rejected { .. }), "{}", err);
    assert!(err.to_string().contains("page_1, page_2"), "{}", err);
}

#[test]
fn bad_arguments_are_the_calls_fault() {
    let dir = temp_dir("bad-arguments");
    let page = saved(&dir, numbered(3).as_bytes());
    let config = serde_json::json!({});

    for arguments in [
        serde_json::json!({"page": "../secret"}),
        serde_json::json!({"page": "page_01"}),
        serde_json::json!({"page": page, "start": 0}),
        serde_json::json!({"page": page, "limit": -1}),
        serde_json::json!({"page": page, "end": "2"}),
        serde_json::json!({"page": page, "start": 3, "end": 2}),
        serde_json::json!({"page": page, "selector": 5}),
        serde_json::json!({"page": page, "view": "markdown"}),
        serde_json::json!({"page": page, "view": "text", "selector": "a"}),
        serde_json::json!({"page": page, "output": "text"}),
        serde_json::json!({"page": page, "selector": "a", "output": "attr:"}),
        serde_json::json!({"page": page, "selector": "a", "output": "json"}),
        serde_json::json!({"page": page, "context": 2}),
        serde_json::json!({"page": page, "search": "x", "context": 11}),
    ] {
        let err = read(&dir, config.clone(), arguments.clone()).expect_err("bad call");
        assert!(
            matches!(err, ToolError::InvalidArgument { .. }),
            "{}: {}",
            arguments,
            err
        );
    }

    let missing = read(&dir, config, serde_json::json!({})).expect_err("missing page");
    assert!(
        matches!(missing, ToolError::MissingArgument { .. }),
        "{}",
        missing
    );
}

// Settings //////////////////////////
//////////////////////////////////////

#[test]
fn unknown_or_unusable_settings_are_refused() {
    let fetch = FetchWebpageTool::new();
    assert!(fetch.validate_config(&serde_json::json!({})).is_ok());
    assert!(fetch
        .validate_config(&serde_json::json!({"fetch_timeout": 5, "max_page_bytes": 100, "allow_private_hosts": true, "use_proxy": true}))
        .is_ok());
    assert!(fetch
        .validate_config(&serde_json::json!({"timeout": 5}))
        .is_err());
    assert!(fetch
        .validate_config(&serde_json::json!({"fetch_timeout": 0}))
        .is_err());
    assert!(fetch
        .validate_config(&serde_json::json!({"max_page_bytes": 0}))
        .is_err());

    let read = ReadWebpageDataTool::new();
    assert!(read.validate_config(&serde_json::json!({})).is_ok());
    assert!(read
        .validate_config(&serde_json::json!({"max_lines": 10}))
        .is_ok());
    assert!(read
        .validate_config(&serde_json::json!({"max_line": 10}))
        .is_err());
    assert!(read
        .validate_config(&serde_json::json!({"max_lines": 0}))
        .is_err());
}

#[test]
fn the_default_registry_offers_both_webpage_tools() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&ToolSettings {
            enabled: vec!["fetch_webpage".to_string(), "read_webpage_data".to_string()],
            configs: Vec::new(),
        })
        .expect("resolve");
    assert_eq!(
        tools.names(),
        vec![
            "end_turn".to_string(),
            "fetch_webpage".to_string(),
            "read_webpage_data".to_string()
        ]
    );
}
