use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use reqwest::blocking::Client;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::CONTENT_TYPE;
use reqwest::{redirect, Url};
use scraper::{Html, Selector};
use serde::Deserialize;

use crate::core::tools::tool::{
    optional_string, required_string, Tool, ToolContext, ToolError, ToolOutput, ToolState,
};
use crate::core::tools::webpage_view::{
    element_text, looks_like_html, outline, split_long, text_lines,
};

pub const DEFAULT_FETCH_TIMEOUT: u64 = 30;

pub const DEFAULT_MAX_PAGE_BYTES: u64 = 10 * 1024 * 1024;

pub const DEFAULT_MAX_LINES: usize = 500;

/// How many lines a read returns when the call does not say.
pub const DEFAULT_READ_LIMIT: usize = 200;

/// A line longer than this is shown as several numbered lines, so a page that
/// is one minified line can still be read a piece at a time.
pub const MAX_LINE_CHARS: usize = 200;

pub const MAX_REDIRECTS: usize = 5;

/// The most lines a search may show either side of each hit.
pub const MAX_SEARCH_CONTEXT: usize = 10;

/// The directory under the agent's own where fetched pages are kept.
pub const PAGES_DIR: &str = "webpages";

/// Page ids are at most this many digits, which keeps them short to type.
const MAX_PAGE_NUMBER: u32 = 999_999_999;

/// How many existing pages an unknown page error names.
const MAX_LISTED_PAGES: usize = 20;

/// Bytes looked at to decide whether a page is binary.
const BINARY_SNIFF_BYTES: usize = 8_192;

// Pages /////////////////////////////
//////////////////////////////////////

/// Reads `page_N` with no leading zeros, so each page has exactly one id and
/// nothing the model writes can name a path outside the pages directory.
pub fn parse_page_id(id: &str) -> Option<u32> {
    let digits = id.strip_prefix("page_")?;
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits
        .parse()
        .ok()
        .filter(|number| *number <= MAX_PAGE_NUMBER)
}

fn page_id(number: u32) -> String {
    format!("page_{}", number)
}

pub fn page_path(pages: &Path, number: u32) -> PathBuf {
    pages.join(format!("{}.dat", page_id(number)))
}

/// The first free page number at or after `from`. Pages are numbered without
/// gaps, so this gallops forward until it finds a free number and then binary
/// searches back to the first one, rather than listing the directory.
pub fn next_free_page(pages: &Path, from: u32) -> Option<u32> {
    let taken = |number: u32| page_path(pages, number).exists();
    if from > MAX_PAGE_NUMBER {
        return None;
    }
    if !taken(from) {
        return Some(from);
    }

    let mut last_taken = from;
    let mut step: u64 = 1;
    let first_free = loop {
        let probe = from as u64 + step;
        if probe > MAX_PAGE_NUMBER as u64 {
            // nothing further out to gallop to; the search below covers the rest
            if taken(MAX_PAGE_NUMBER) {
                return None;
            }
            break MAX_PAGE_NUMBER;
        }
        let probe = probe as u32;
        if !taken(probe) {
            break probe;
        }
        last_taken = probe;
        step *= 2;
    };

    let (mut low, mut high) = (last_taken, first_free);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if taken(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    Some(high)
}

/// Gives the written temp file a page number. A hard link fails rather than
/// replacing a page another writer claimed first, and then the search goes on
/// from past it.
fn claim_page(pages: &Path, temp: &Path) -> io::Result<u32> {
    let mut from = 1;
    loop {
        let number = next_free_page(pages, from).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::StorageFull,
                "there are no free page numbers left",
            )
        })?;
        match fs::hard_link(temp, page_path(pages, number)) {
            Ok(()) => return Ok(number),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => from = number + 1,
            Err(err) => return Err(err),
        }
    }
}

/// Written to a temp file first, so a fetch that fails halfway never leaves a
/// page behind that looks complete.
pub fn save_page(pages: &Path, body: &[u8]) -> io::Result<u32> {
    fs::create_dir_all(pages)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let temp = pages.join(format!(
        ".fetch-{}-{:?}-{}.tmp",
        std::process::id(),
        std::thread::current().id(),
        nanos
    ));

    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(body)?;
        file.sync_all()
    });
    let claimed = written.and_then(|_| claim_page(pages, &temp));
    let _ = fs::remove_file(&temp);
    claimed
}

/// The pages saved so far, lowest first, for telling the model what it can read.
fn existing_pages(pages: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir(pages) else {
        return Vec::new();
    };
    let mut numbers: Vec<u32> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            parse_page_id(name.strip_suffix(".dat")?)
        })
        .collect();
    numbers.sort_unstable();
    numbers
}

/// A NUL byte near the start is as good a sign as any that this is not text.
fn is_binary(body: &[u8]) -> bool {
    body[..body.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

/// The page as the model numbers it: split on `\n` with any `\r` dropped, and
/// any line over [`MAX_LINE_CHARS`] split into several, so every byte has a
/// line number it can be reached by.
pub fn display_lines(text: &str) -> Vec<&str> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for line in text.split('\n') {
        let mut rest = line.strip_suffix('\r').unwrap_or(line);
        loop {
            match rest.char_indices().nth(MAX_LINE_CHARS) {
                Some((split, _)) => {
                    lines.push(&rest[..split]);
                    rest = &rest[split..];
                }
                None => {
                    lines.push(rest);
                    break;
                }
            }
        }
    }
    lines
}

// Fetching //////////////////////////
//////////////////////////////////////

/// A plain GET of one url, saved to a page file the model reads back with
/// `read_webpage_data`. Nothing is rendered and no script runs.
pub struct FetchWebpageTool;

impl FetchWebpageTool {
    pub fn new() -> FetchWebpageTool {
        FetchWebpageTool
    }
}

/// The tool's own settings, out of the agent's `tools.configs` entry for
/// `fetch_webpage`. Unknown fields are refused so a misspelled key is reported
/// when the agent's files are read.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchWebpageSettings {
    /// How long the whole request, body included, may take, in seconds.
    #[serde(default = "default_fetch_timeout")]
    pub fetch_timeout: u64,

    /// Bytes past this are not saved, and the result says the page was cut.
    #[serde(default = "default_max_page_bytes")]
    pub max_page_bytes: u64,

    /// Off by default so a page cannot steer the agent into the local network
    /// or a cloud metadata address.
    #[serde(default)]
    pub allow_private_hosts: bool,

    /// Whether `HTTP_PROXY` and `HTTPS_PROXY` are honoured. A proxy resolves
    /// names itself, so through one only literal addresses are checked.
    #[serde(default)]
    pub use_proxy: bool,
}

fn default_fetch_timeout() -> u64 {
    DEFAULT_FETCH_TIMEOUT
}

fn default_max_page_bytes() -> u64 {
    DEFAULT_MAX_PAGE_BYTES
}

impl FetchWebpageSettings {
    fn parse(config: &serde_json::Value) -> Result<FetchWebpageSettings, String> {
        let settings = serde_json::from_value::<FetchWebpageSettings>(config.clone())
            .map_err(|err| err.to_string())?;
        if settings.fetch_timeout == 0 {
            return Err("`fetch_timeout` must be at least 1".into());
        }
        if settings.max_page_bytes == 0 {
            return Err("`max_page_bytes` must be at least 1".into());
        }
        Ok(settings)
    }
}

impl Tool for FetchWebpageTool {
    fn name(&self) -> String {
        "fetch_webpage".into()
    }

    fn description(&self) -> String {
        "Download a web page with a plain http GET, like wget. No javascript \
         runs, so pages that build themselves in the browser come back mostly \
         empty. The raw response body is saved and you get back an id such as \
         `page_3` along with the status, content type and size; read the page \
         with `read_webpage_data` using that id. Private and local addresses \
         are refused unless the agent's config allows them."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The http or https url to fetch."
                }
            },
            "required": ["url"]
        })
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        FetchWebpageSettings::parse(config).map(|_| ())
    }

    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        let settings = FetchWebpageSettings::parse(&context.config())
            .map_err(|error| ToolError::InvalidConfig { error })?;

        let allow_private = settings.allow_private_hosts;
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(settings.fetch_timeout))
            .user_agent(concat!("apila/", env!("CARGO_PKG_VERSION")))
            .redirect(redirect::Policy::custom(move |attempt| {
                // `previous` holds the original url too, so this allows exactly
                // `MAX_REDIRECTS` hops, the same way reqwest's own limit counts
                if attempt.previous().len() > MAX_REDIRECTS {
                    return attempt.error(format!("more than {} redirects", MAX_REDIRECTS));
                }
                match check_url(attempt.url(), allow_private) {
                    Ok(()) => attempt.follow(),
                    Err(refused) => attempt.error(refused),
                }
            }));
        if !allow_private {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        if !settings.use_proxy {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|err| ToolError::NotStarted {
            error: error_chain(&err),
        })?;

        Ok(Box::new(FetchWebpageState { settings, client }))
    }
}

pub struct FetchWebpageState {
    settings: FetchWebpageSettings,
    client: Client,
}

impl ToolState for FetchWebpageState {
    fn run(
        &mut self,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let raw = required_string(arguments, "url")?;
        let url = Url::parse(raw.trim()).map_err(|err| ToolError::InvalidArgument {
            argument: "url".into(),
            expected: format!("a valid url ({})", err),
        })?;
        check_url(&url, self.settings.allow_private_hosts)
            .map_err(|refused| ToolError::Rejected { error: refused.0 })?;

        let response = self.client.get(url.clone()).send().map_err(request_error)?;

        let status = response.status();
        let final_url = response.url().clone();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let announced = response.content_length();

        // the gzip feature is off, so the body arrives as the server sent it
        let mut body = Vec::new();
        response
            .take(self.settings.max_page_bytes + 1)
            .read_to_end(&mut body)
            .map_err(|err| body_error(&err))?;
        let truncated = body.len() as u64 > self.settings.max_page_bytes;
        body.truncate(self.settings.max_page_bytes as usize);

        let number =
            save_page(&context.dir().join(PAGES_DIR), &body).map_err(|err| ToolError::Failed {
                error: format!("the page could not be saved: {}", err),
            })?;

        let mut lines = vec![
            format!("saved as {}", page_id(number)),
            format!("status: {}", status),
        ];
        if final_url != url {
            lines.push(format!("redirected to: {}", final_url));
        }
        lines.push(format!(
            "content-type: {}",
            content_type.as_deref().unwrap_or("(none)")
        ));
        if truncated {
            let announced = match announced {
                Some(total) => format!("; the server sent {} bytes", total),
                None => String::new(),
            };
            lines.push(format!(
                "size: {} bytes, cut short at the configured limit{}",
                body.len(),
                announced
            ));
        } else {
            lines.push(format!("size: {} bytes", body.len()));
        }
        let mut read = format!(
            "read it with `read_webpage_data` and page `{}`",
            page_id(number)
        );
        if is_binary(&body) {
            lines.push("lines: none; the body is binary".into());
        } else {
            let text = String::from_utf8_lossy(&body);
            let raw = display_lines(&text).len();
            if looks_like_html(&text) {
                let shown = split_long(text_lines(&Html::parse_document(&text))).len();
                lines.push(format!("lines: {} raw, {} as text", raw, shown));
                read.push_str("; `view: \"outline\"` maps the page so you can pick a selector");
            } else {
                lines.push(format!("lines: {}", raw));
            }
        }
        lines.push(read);

        Ok(ToolOutput::new(lines.join("\n")))
    }
}

/// A url or address this tool will not go to. Its own type so it can be told
/// apart from a network failure once reqwest has wrapped it.
#[derive(Debug)]
pub struct Refused(pub String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Refused {}

/// Checked on the first url and every redirect. A host name is checked again
/// when it resolves, by [`PublicOnlyResolver`].
pub fn check_url(url: &Url, allow_private: bool) -> Result<(), Refused> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(Refused(format!(
            "only http and https urls can be fetched, not `{}`",
            url.scheme()
        )));
    }
    if allow_private {
        return Ok(());
    }
    let Some(host) = url.host_str() else {
        return Err(Refused("the url has no host".into()));
    };
    // ipv6 hosts keep their brackets; a name that is not an address is
    // checked once it resolves
    let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    else {
        return Ok(());
    };
    if is_private(ip) {
        return Err(Refused(format!(
            "{} is a private or local address, which this agent may not fetch",
            ip
        )));
    }
    Ok(())
}

/// Loopback, private, link local, unspecified and carrier nat ranges, in
/// either address family.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(embedded) = embedded_ipv4(ip) {
                return is_private_v4(embedded);
            }
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

/// The ipv4 address inside a mapped, nat64 or 6to4 address, any of which can
/// lead to a host on the local network.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }
    let segments = ip.segments();
    let octets = ip.octets();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return Some(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }
    if segments[0] == 0x2002 {
        return Some(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]));
    }
    None
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || (a == 100 && (64..128).contains(&b))
}

/// Drops private addresses from what a name resolves to, so a public looking
/// name cannot lead to a local one. Resolving blocks, which is fine on the
/// blocking client's own runtime thread.
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved: Vec<SocketAddr> = (host.as_str(), 0).to_socket_addrs()?.collect();
            let public: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|addr| !is_private(addr.ip()))
                .collect();
            if public.is_empty() {
                return Err(Box::new(Refused(format!(
                    "{} resolves only to private or local addresses, which this agent may not fetch",
                    host
                ))) as Box<dyn Error + Send + Sync>);
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// A refusal is the call's fault and anything else the network's, which is
/// what tells the model whether trying another url could help.
fn request_error(err: reqwest::Error) -> ToolError {
    let mut source: Option<&(dyn Error + 'static)> = Some(&err);
    while let Some(current) = source {
        if let Some(refused) = current.downcast_ref::<Refused>() {
            return ToolError::Rejected {
                error: refused.0.clone(),
            };
        }
        source = current.source();
    }
    let error = if err.is_timeout() {
        format!("the request timed out: {}", error_chain(&err))
    } else {
        error_chain(&err)
    };
    ToolError::Failed { error }
}

fn body_error(err: &io::Error) -> ToolError {
    ToolError::Failed {
        error: format!("the body could not be read: {}", error_chain(err)),
    }
}

/// reqwest's own message rarely says what went wrong; its sources do.
fn error_chain(err: &dyn Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(current) = source {
        let part = current.to_string();
        if !parts.iter().any(|seen| seen.contains(&part)) {
            parts.push(part);
        }
        source = current.source();
    }
    parts.join(": ")
}

// Reading ///////////////////////////
//////////////////////////////////////

/// Reads back a page `fetch_webpage` saved, by line range or by css selector,
/// never more than the configured number of lines at once.
pub struct ReadWebpageDataTool;

impl ReadWebpageDataTool {
    pub fn new() -> ReadWebpageDataTool {
        ReadWebpageDataTool
    }
}

/// The tool's own settings, out of the agent's `tools.configs` entry for
/// `read_webpage_data`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadWebpageDataSettings {
    /// The most lines one call returns; a larger `limit` is lowered to it.
    #[serde(default = "default_max_lines")]
    pub max_lines: usize,
}

fn default_max_lines() -> usize {
    DEFAULT_MAX_LINES
}

impl ReadWebpageDataSettings {
    fn parse(config: &serde_json::Value) -> Result<ReadWebpageDataSettings, String> {
        let settings = serde_json::from_value::<ReadWebpageDataSettings>(config.clone())
            .map_err(|err| err.to_string())?;
        if settings.max_lines == 0 {
            return Err("`max_lines` must be at least 1".into());
        }
        Ok(settings)
    }
}

impl Tool for ReadWebpageDataTool {
    fn name(&self) -> String {
        "read_webpage_data".into()
    }

    fn description(&self) -> String {
        "Read a page saved by `fetch_webpage`. Each call returns numbered \
         entries and a footer saying how many there are and where to continue. \
         Use the cheapest call that answers the question:\n\
         - `view: \"outline\"` gives a short map of an html page: title, \
         headings, elements that repeat (with a css selector and a sample of \
         the first one's text), tables, forms and embedded json. Start here on \
         a page you have not seen, then pick a selector from it.\n\
         - `selector`, a css selector such as `li.product`, `h1, h2` or \
         `table#prices tr`, returns one entry per matching element. `output` \
         sets what each entry shows: `text` (the default), `html`, or \
         `attr:NAME` such as `attr:href`.\n\
         - `search`, a regex (`(?i)` ignores case), keeps only the lines or \
         matches it finds, under their own numbers so you can read around them \
         with `start`. `context` adds that many lines either side, numbered \
         `n-` instead of `n:`.\n\
         - Otherwise you get the whole page. For html `view: \"text\"` is the \
         default: readable text without scripts, styles or markup, headings as \
         `#`, list items as `-`, table cells split by `|` and links as \
         `[text](url)`. `view: \"raw\"` gives the source lines, and is the \
         default for pages that are not html.\n\
         `start`, `end` and `limit` count lines, or matches with a selector; \
         with `search`, `limit` counts hits. Long lines and matches are split \
         over several numbered entries."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "page": {
                    "type": "string",
                    "description": "The page id `fetch_webpage` returned, such as `page_3`."
                },
                "view": {
                    "type": "string",
                    "enum": ["text", "raw", "outline"],
                    "description": "How to show the whole page when there is no selector. Defaults to `text` for html and `raw` otherwise."
                },
                "selector": {
                    "type": "string",
                    "description": "A css selector. Only the matching elements are returned, one entry each."
                },
                "output": {
                    "type": "string",
                    "description": "With a selector, what each entry shows: `text` (the default), `html`, or `attr:NAME` for one attribute, such as `attr:href`."
                },
                "search": {
                    "type": "string",
                    "description": "A regex. Only the lines, or matches with a selector, that it finds are returned."
                },
                "context": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "With `search`, how many lines to show before and after each hit."
                },
                "start": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The first line (or match) to return or search from, counting from 1. Defaults to 1."
                },
                "end": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The last line (or match) to return or search, inclusive."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The most lines (or matches, or search hits) to return."
                }
            },
            "required": ["page"]
        })
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        ReadWebpageDataSettings::parse(config).map(|_| ())
    }

    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        let settings = ReadWebpageDataSettings::parse(&context.config())
            .map_err(|error| ToolError::InvalidConfig { error })?;
        Ok(Box::new(ReadWebpageDataState { settings }))
    }
}

pub struct ReadWebpageDataState {
    settings: ReadWebpageDataSettings,
}

impl ToolState for ReadWebpageDataState {
    fn run(
        &mut self,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let id = required_string(arguments, "page")?;
        let view = optional_string(arguments, "view")?
            .map(View::parse)
            .transpose()?;
        let selector = optional_string(arguments, "selector")?;
        let output = optional_string(arguments, "output")?
            .map(Output::parse)
            .transpose()?;
        let search = optional_string(arguments, "search")?;
        let context_lines = optional_count(arguments, "context")?;
        let start = optional_count(arguments, "start")?;
        let end = optional_count(arguments, "end")?;
        let limit = optional_count(arguments, "limit")?;
        if let (Some(start), Some(end)) = (start, end) {
            if start > end {
                return Err(ToolError::InvalidArgument {
                    argument: "end".into(),
                    expected: "at least `start`".into(),
                });
            }
        }
        if selector.is_some() && view.is_some() {
            return Err(ToolError::InvalidArgument {
                argument: "view".into(),
                expected: "to be left out with `selector`; `output` sets how matches are shown"
                    .into(),
            });
        }
        if selector.is_none() && output.is_some() {
            return Err(ToolError::InvalidArgument {
                argument: "output".into(),
                expected: "to be given only with `selector`; without one use `view`".into(),
            });
        }
        if search.is_none() && context_lines.is_some() {
            return Err(ToolError::InvalidArgument {
                argument: "context".into(),
                expected: "to be given only with `search`".into(),
            });
        }
        let search = search
            .map(|pattern| Search::new(pattern, context_lines.unwrap_or(0)))
            .transpose()?;

        let pages = context.dir().join(PAGES_DIR);
        let body = read_page(&pages, id)?;
        if is_binary(&body) {
            return Ok(ToolOutput::new(format!(
                "{} is binary ({} bytes), so there are no lines to show",
                id,
                body.len()
            )));
        }
        let text = String::from_utf8_lossy(&body);

        let mut split_matches = false;
        let (entries, unit): (Vec<String>, &str) = match selector {
            Some(selector) => {
                let (entries, split) = select(&text, selector, output.unwrap_or(Output::Text))?;
                split_matches = split;
                (entries, if split { "entries" } else { "matches" })
            }
            None => {
                let view = view.unwrap_or(if looks_like_html(&text) {
                    View::Text
                } else {
                    View::Raw
                });
                let lines = match view {
                    View::Raw => display_lines(&text)
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    View::Text => split_long(text_lines(&Html::parse_document(&text))),
                    View::Outline => split_long(outline(&Html::parse_document(&text))),
                };
                (lines, "lines")
            }
        };

        let range = Range::new(start, end, limit, self.settings.max_lines);
        let mut out = match search {
            Some(search) => range.render_search(id, &entries, unit, &search),
            None => range.render(id, &entries, unit),
        };
        if split_matches {
            out.push_str("; long matches are split over several entries");
        }
        Ok(ToolOutput::new(out))
    }
}

/// How the whole page is shown when no selector is given.
enum View {
    Text,
    Raw,
    Outline,
}

impl View {
    fn parse(view: &str) -> Result<View, ToolError> {
        match view {
            "text" => Ok(View::Text),
            "raw" => Ok(View::Raw),
            "outline" => Ok(View::Outline),
            _ => Err(ToolError::InvalidArgument {
                argument: "view".into(),
                expected: "one of `text`, `raw` or `outline`".into(),
            }),
        }
    }
}

/// What each selector match is shown as.
enum Output {
    Text,
    Html,
    Attr(String),
}

impl Output {
    fn parse(output: &str) -> Result<Output, ToolError> {
        match output {
            "text" => Ok(Output::Text),
            "html" => Ok(Output::Html),
            _ => match output.strip_prefix("attr:").map(str::trim) {
                Some(name) if !name.is_empty() => Ok(Output::Attr(name.to_string())),
                _ => Err(ToolError::InvalidArgument {
                    argument: "output".into(),
                    expected: "`text`, `html` or `attr:NAME`, such as `attr:href`".into(),
                }),
            },
        }
    }
}

/// The page's bytes, or a rejection naming the pages that do exist.
fn read_page(pages: &Path, id: &str) -> Result<Vec<u8>, ToolError> {
    let Some(number) = parse_page_id(id) else {
        return Err(ToolError::InvalidArgument {
            argument: "page".into(),
            expected: "a page id from `fetch_webpage`, such as `page_3`".into(),
        });
    };
    match fs::read(page_path(pages, number)) {
        Ok(body) => Ok(body),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let existing = existing_pages(pages);
            let known = if existing.is_empty() {
                "no pages have been fetched yet".to_string()
            } else {
                let mut listed: Vec<String> = existing
                    .iter()
                    .take(MAX_LISTED_PAGES)
                    .map(|number| page_id(*number))
                    .collect();
                if existing.len() > MAX_LISTED_PAGES {
                    listed.push(format!("and {} more", existing.len() - MAX_LISTED_PAGES));
                }
                format!("the pages are: {}", listed.join(", "))
            };
            Err(ToolError::Rejected {
                error: format!("there is no page `{}`; {}", id, known),
            })
        }
        Err(err) => Err(ToolError::Failed {
            error: format!("the page could not be read: {}", err),
        }),
    }
}

/// Each matching element as one entry, or several when it is too long for one,
/// and whether any was. `Html` is not `Send`, so the page is parsed per call.
fn select(text: &str, selector: &str, output: Output) -> Result<(Vec<String>, bool), ToolError> {
    let parsed = Selector::parse(selector).map_err(|err| ToolError::Rejected {
        error: format!(
            "`selector` is not a css selector this tool understands: {}",
            err
        ),
    })?;
    let document = Html::parse_document(text);
    let mut entries = Vec::new();
    let mut split = false;
    for element in document.select(&parsed) {
        let entry = match &output {
            Output::Text => element_text(element),
            Output::Html => element
                .html()
                .split(['\r', '\n'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            Output::Attr(name) => match element.attr(name) {
                Some(value) => value.trim().to_string(),
                None => format!("(no {})", name),
            },
        };
        let parts = display_lines(&entry);
        split |= parts.len() > 1;
        if parts.is_empty() {
            entries.push(String::new());
        } else {
            entries.extend(parts.into_iter().map(str::to_string));
        }
    }
    Ok((entries, split))
}

/// A `search` regex and how many lines to show around each hit.
struct Search {
    regex: Regex,
    context: usize,
}

impl Search {
    fn new(pattern: &str, context: usize) -> Result<Search, ToolError> {
        if context > MAX_SEARCH_CONTEXT {
            return Err(ToolError::InvalidArgument {
                argument: "context".into(),
                expected: format!("at most {}", MAX_SEARCH_CONTEXT),
            });
        }
        let regex = Regex::new(pattern).map_err(|err| ToolError::Rejected {
            error: format!("`search` is not a regex this tool understands: {}", err),
        })?;
        Ok(Search { regex, context })
    }
}

/// Which entries one call returns: from `start`, ending at the earliest of
/// `end`, `start + limit - 1` and the last entry.
struct Range {
    start: usize,
    end: Option<usize>,
    limit: usize,
    lowered_from: Option<usize>,
    max_lines: usize,
}

impl Range {
    fn new(
        start: Option<usize>,
        end: Option<usize>,
        limit: Option<usize>,
        max_lines: usize,
    ) -> Range {
        let requested = limit.unwrap_or(DEFAULT_READ_LIMIT.min(max_lines));
        Range {
            start: start.unwrap_or(1),
            end,
            limit: requested.min(max_lines),
            lowered_from: (requested > max_lines).then_some(requested),
            max_lines,
        }
    }

    /// Nothing to show, or `None` when `start` is inside the entries.
    fn out_of_range(&self, id: &str, total: usize, unit: &str) -> Option<String> {
        if total == 0 {
            return Some(format!("{} has no {}", id, unit));
        }
        if self.start > total {
            return Some(format!(
                "{} has {} {}, so there is nothing from {}",
                id, total, unit, self.start
            ));
        }
        None
    }

    fn lowered_note(&self) -> String {
        match self.lowered_from {
            Some(requested) => format!(
                "; limit {} was lowered to the configured most of {}",
                requested, self.limit
            ),
            None => String::new(),
        }
    }

    fn render(&self, id: &str, entries: &[String], unit: &str) -> String {
        let total = entries.len();
        if let Some(message) = self.out_of_range(id, total, unit) {
            return message;
        }

        let mut last = total.min(self.start.saturating_add(self.limit - 1));
        if let Some(end) = self.end {
            last = last.min(end);
        }

        let mut out: Vec<String> = (self.start..=last)
            .map(|number| format!("{}: {}", number, entries[number - 1]))
            .collect();

        let mut footer = format!("-- {} {}–{} of {}", unit, self.start, last, total);
        if last < total {
            footer.push_str(&format!("; continue with start={}", last + 1));
        }
        footer.push_str(&self.lowered_note());
        out.push(footer);
        out.join("\n")
    }

    /// The entries from `start` to `end` that the regex finds, at most `limit`
    /// of them, each with its context. Gaps between hits are shown as `…`.
    fn render_search(&self, id: &str, entries: &[String], unit: &str, search: &Search) -> String {
        let total = entries.len();
        if let Some(message) = self.out_of_range(id, total, unit) {
            return message;
        }
        let last = self.end.map_or(total, |end| end.min(total));
        let hits: Vec<usize> = (self.start..=last)
            .filter(|number| search.regex.is_match(&entries[number - 1]))
            .collect();
        let searched = format!("searched {} {}–{} of {}", unit, self.start, last, total);
        if hits.is_empty() {
            return format!("-- no hits; {}", searched);
        }

        let mut out = Vec::new();
        let mut printed = 0;
        for (index, &hit) in hits.iter().take(self.limit).enumerate() {
            let from = hit.saturating_sub(search.context).max(printed + 1).max(1);
            let to = (hit + search.context).min(total);
            let gap = printed > 0 && from > printed + 1;
            let needed = (to + 1).saturating_sub(from) + usize::from(gap);
            if index > 0 && out.len() + needed > self.max_lines {
                break;
            }
            if gap {
                out.push("…".to_string());
            }
            for number in from..=to {
                let mark = if hits.binary_search(&number).is_ok() {
                    ':'
                } else {
                    '-'
                };
                out.push(format!("{}{} {}", number, mark, entries[number - 1]));
            }
            printed = printed.max(to);
        }

        // hits that only fell inside another hit's context were shown too
        let shown = hits.partition_point(|&hit| hit <= printed);
        let mut footer = format!("-- {} of {} hits shown; {}", shown, hits.len(), searched);
        if shown < hits.len() {
            footer.push_str(&format!("; continue with start={}", hits[shown - 1] + 1));
        }
        footer.push_str(&self.lowered_note());
        out.push(footer);
        out.join("\n")
    }
}

// Arguments /////////////////////////
//////////////////////////////////////

/// A whole number of at least 1, or nothing when the call left it out.
fn optional_count(
    arguments: &serde_json::Value,
    argument: &str,
) -> Result<Option<usize>, ToolError> {
    match arguments.get(argument) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            Some(count) if count >= 1 => Ok(Some(count as usize)),
            _ => Err(ToolError::InvalidArgument {
                argument: argument.into(),
                expected: "a whole number of at least 1".into(),
            }),
        },
    }
}
