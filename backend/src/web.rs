//! Web search and documentation fetching implementation for the Spin backend.
//!
//! Retains web research HTTP endpoints backed by Spin WASI outbound HTTP.
//! Agent web tools execute through installed SDK plugins.
//!
//! Combines multi-source search providers (DuckDuckGo Instant Answer, StackOverflow,
//! Crates.io, GitHub, Wikipedia) with proper User-Agent headers, redirect following,
//! and clean Markdown extraction so developer queries return actionable results
//! without requiring paid API keys or triggering bot blockers.

use crate::url::url_encode;

use bytes::{Buf, Bytes};
use http_body_util::BodyExt;
use openwebide_core::{WebSearchResult, html_to_markdown};
use spin_sdk::http::{self, FullBody, Request, Response, Uri, box_body};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 OpenWebIDE/0.1 (https://github.com/openwebide/openwebide)";

/// Validate that a URL target uses http/https scheme and is not a cloud metadata endpoint.
///
/// Strips trailing dots and IPv6 brackets from the lowercased host before checking.
/// Refuses cloud metadata addresses (`169.254.169.254`, `fd00:ec2::254`, their IPv4-mapped forms,
/// and `metadata.google.internal`), while allowing all other destinations (LAN, loopback, internet).
pub fn check_fetch_target(url: &str) -> Result<Uri, String> {
    let uri: Uri = url
        .parse::<Uri>()
        .map_err(|e| format!("invalid URL '{url}': {e}"))?;

    let scheme = uri
        .scheme_str()
        .ok_or_else(|| format!("invalid URL '{url}': missing scheme"))?;

    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!(
            "unsupported scheme '{scheme}': only http and https are allowed"
        ));
    }

    let raw_host = uri
        .host()
        .ok_or_else(|| format!("invalid URL '{url}': missing host"))?;

    openwebide_core::network::check_http_host(raw_host)?;

    Ok(uri)
}

/// Resolve a target redirect `Location` against a base `Uri` according to RFC 3986 §5.2.
fn resolve_redirect(base: &Uri, location: &str) -> Result<String, String> {
    let location = location.trim();
    let base_scheme = base
        .scheme_str()
        .ok_or_else(|| "base URI missing scheme".to_string())?;
    let base_authority = base.authority().map(AsRef::<str>::as_ref);
    let base_path = base.path();

    let (loc_without_frag, frag) = match location.split_once('#') {
        Some((head, tail)) => (head, Some(tail)),
        None => (location, None),
    };

    let (loc_without_query, loc_query) = match loc_without_frag.split_once('?') {
        Some((head, tail)) => (head, Some(tail)),
        None => (loc_without_frag, None),
    };

    let target_scheme: &str;
    let target_authority: Option<&str>;
    let target_path: String;
    let target_query: Option<&str>;

    if let Some((scheme, rest)) = extract_scheme(loc_without_query) {
        target_scheme = scheme;
        if let Some(rest_after_slashes) = rest.strip_prefix("//") {
            let (auth, path) = match rest_after_slashes.find('/') {
                Some(idx) => (&rest_after_slashes[..idx], &rest_after_slashes[idx..]),
                None => (rest_after_slashes, ""),
            };
            target_authority = Some(auth);
            target_path = remove_dot_segments(path);
        } else {
            target_authority = None;
            target_path = remove_dot_segments(rest);
        }
        target_query = loc_query;
    } else {
        target_scheme = base_scheme;
        if let Some(rest) = loc_without_query.strip_prefix("//") {
            let (auth, path) = match rest.find('/') {
                Some(idx) => (&rest[..idx], &rest[idx..]),
                None => (rest, ""),
            };
            target_authority = Some(auth);
            target_path = remove_dot_segments(path);
            target_query = loc_query;
        } else {
            target_authority = base_authority;
            if loc_without_query.is_empty() {
                target_path = base_path.to_string();
                target_query = loc_query.or_else(|| base.query());
            } else if loc_without_query.starts_with('/') {
                target_path = remove_dot_segments(loc_without_query);
                target_query = loc_query;
            } else {
                let merged = merge_paths(base_path, loc_without_query, base_authority.is_some());
                target_path = remove_dot_segments(&merged);
                target_query = loc_query;
            }
        }
    }

    let mut result = String::new();
    result.push_str(target_scheme);
    result.push(':');
    if let Some(auth) = target_authority {
        result.push_str("//");
        result.push_str(auth);
    }
    if !target_path.is_empty() {
        if target_authority.is_some() && !target_path.starts_with('/') {
            result.push('/');
        }
        result.push_str(&target_path);
    }
    if let Some(q) = target_query {
        result.push('?');
        result.push_str(q);
    }
    if let Some(f) = frag {
        result.push('#');
        result.push_str(f);
    }

    Ok(result)
}

fn extract_scheme(s: &str) -> Option<(&str, &str)> {
    let colon_idx = s.find(':')?;
    if let Some(slash_idx) = s.find('/')
        && slash_idx < colon_idx
    {
        return None;
    }
    let potential_scheme = &s[..colon_idx];
    let mut chars = potential_scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    Some((potential_scheme, &s[colon_idx + 1..]))
}

fn merge_paths(base_path: &str, ref_path: &str, has_base_authority: bool) -> String {
    if has_base_authority && base_path.is_empty() {
        format!("/{ref_path}")
    } else if let Some(pos) = base_path.rfind('/') {
        format!("{}{ref_path}", &base_path[..=pos])
    } else {
        ref_path.to_string()
    }
}

fn remove_dot_segments(path: &str) -> String {
    let mut input = path;
    let mut output = String::new();

    while !input.is_empty() {
        if let Some(rest) = input.strip_prefix("../") {
            input = rest;
        } else if let Some(rest) = input.strip_prefix("./") {
            input = rest;
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") {
            input = &input[3..];
            pop_last_segment(&mut output);
        } else if input == "/.." {
            input = "/";
            pop_last_segment(&mut output);
        } else if input == "." || input == ".." {
            input = "";
        } else {
            let start = if input.starts_with('/') { 1 } else { 0 };
            let seg_end = match input[start..].find('/') {
                Some(pos) => start + pos,
                None => input.len(),
            };
            output.push_str(&input[..seg_end]);
            input = &input[seg_end..];
        }
    }

    output
}

fn pop_last_segment(output: &mut String) {
    if let Some(pos) = output.rfind('/') {
        output.truncate(pos);
    } else {
        output.clear();
    }
}

/// Send an outbound HTTP GET request with standard browser headers and a descriptive User-Agent.
async fn http_get(url: &str) -> Result<Response, String> {
    let req = Request::builder()
        .method("GET")
        .uri(url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json, text/html, */*")
        .header("accept-language", "en-US,en;q=0.9")
        .body(box_body(FullBody::new(Bytes::new())))
        .map_err(|e| format!("build request to {url}: {e}"))?;

    http::send(req)
        .await
        .map_err(|e| format!("HTTP request to {url} failed: {e}"))
}

/// Send an HTTP GET request and follow redirects (up to `max_redirects`).
async fn http_get_follow_redirects(
    mut url: String,
    max_redirects: usize,
) -> Result<Response, String> {
    let mut current_uri = check_fetch_target(&url)?;
    for _ in 0..=max_redirects {
        let res = http_get(&url).await?;
        let status = res.status().as_u16();
        if (status == 301 || status == 302 || status == 303 || status == 307 || status == 308)
            && let Some(loc) = res.headers().get("location").and_then(|v| v.to_str().ok())
        {
            let next_url = resolve_redirect(&current_uri, loc)?;
            current_uri = check_fetch_target(&next_url)?;
            url = next_url;
            continue;
        }
        return Ok(res);
    }
    Err("too many redirects".to_string())
}

/// Read body frames until `cap` bytes are collected, then drop the remaining body stream.
async fn read_body_capped<B>(body: B, cap: usize) -> Result<Bytes, String>
where
    B: http_body::Body,
    B::Error: std::fmt::Display,
{
    let mut body = std::pin::pin!(body);
    let mut buf = Vec::with_capacity(cap.min(64 * 1024));

    while buf.len() < cap {
        match body.as_mut().frame().await {
            Some(Ok(frame)) => {
                if let Ok(mut data) = frame.into_data() {
                    let to_take = (cap - buf.len()).min(data.remaining());
                    let chunk = data.copy_to_bytes(to_take);
                    buf.extend_from_slice(&chunk);
                }
            }
            Some(Err(e)) => return Err(format!("read response body: {e}")),
            None => break,
        }
    }

    Ok(Bytes::from(buf))
}

/// Perform HTTP GET and parse the response body as JSON.
async fn fetch_json(url: &str) -> Result<serde_json::Value, String> {
    let res = http_get(url).await?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    let bytes = read_body_capped(res.into_body(), 2 * 1024 * 1024).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse JSON from {url}: {e}"))
}

/// Perform web search by querying DuckDuckGo, StackOverflow, Crates.io, GitHub, and Wikipedia.
pub async fn search_web_internal(
    query: &str,
    limit: usize,
) -> Result<Vec<WebSearchResult>, String> {
    let limit = limit.clamp(1, 10);
    let mut results = Vec::new();
    let q_lower = query.to_lowercase();
    let dev_keywords = [
        "rust",
        "python",
        "javascript",
        "typescript",
        "html",
        "css",
        "sql",
        "c++",
        "go",
        "error",
        "exception",
        "failed",
        "bug",
        "crash",
        "undefined",
        "cannot",
        "panic",
        "how to",
        "function",
        "api",
        "struct",
        "class",
        "async",
        "await",
        "trait",
        "impl",
        "cargo",
        "npm",
        "pip",
        "git",
        "crate",
        "package",
        "library",
        "framework",
        "repo",
        "leptos",
        "react",
        "vue",
        "tokio",
        "axum",
        "actix",
        "serde",
        "tailwind",
        "vite",
        "wasm",
        "docker",
        "podman",
        "sqlite",
        "postgres",
        "redis",
        "compile",
        "build",
    ];
    let is_dev_query = dev_keywords.iter().any(|k| q_lower.contains(k));
    let is_rust_query = q_lower.contains("rust")
        || q_lower.contains("cargo")
        || q_lower.contains("crate")
        || q_lower.contains("leptos")
        || q_lower.contains("tokio")
        || q_lower.contains("axum")
        || q_lower.contains("serde")
        || q_lower.contains("wasm");

    // 1. DuckDuckGo Instant Answer
    if let Ok(ddg_results) = search_duckduckgo(query, limit).await {
        for res in ddg_results {
            if !results.iter().any(|r: &WebSearchResult| r.url == res.url) {
                results.push(res);
            }
        }
    }

    if is_dev_query {
        // Dev flow: StackOverflow -> Crates.io -> GitHub -> Wikipedia

        // 2. StackOverflow (essential for developer queries, compiler errors, framework solutions)
        if results.len() < limit
            && let Ok(so_results) = search_stackoverflow(query, limit).await
        {
            extend_unique(&mut results, so_results, limit);
        }

        // 3. Crates.io (for Rust/crates ecosystem queries)
        if results.len() < limit
            && is_rust_query
            && let Ok(crate_results) = search_crates(query, limit).await
        {
            extend_unique(&mut results, crate_results, limit);
        }

        // 4. GitHub Repositories (for finding official packages, libraries, tools)
        if results.len() < limit
            && let Ok(gh_results) = search_github(query, limit).await
        {
            extend_unique(&mut results, gh_results, limit);
        }

        // 5. Wikipedia Opensearch fallback
        if results.len() < limit
            && let Ok(wiki_results) = search_wikipedia_opensearch(query, limit).await
        {
            extend_unique(&mut results, wiki_results, limit);
        }
    } else {
        // General flow: Wikipedia Opensearch -> StackOverflow -> GitHub -> Wikipedia Full-Text

        // 2. Wikipedia Opensearch (exact article title matches, e.g. "Alan Turing", "Quicksort")
        if results.len() < limit
            && let Ok(wiki_results) = search_wikipedia_opensearch(query, limit).await
        {
            extend_unique(&mut results, wiki_results, limit);
        }

        // 3. StackOverflow
        if results.len() < limit
            && let Ok(so_results) = search_stackoverflow(query, limit).await
        {
            extend_unique(&mut results, so_results, limit);
        }

        // 4. GitHub Repositories
        if results.len() < limit
            && let Ok(gh_results) = search_github(query, limit).await
        {
            extend_unique(&mut results, gh_results, limit);
        }
    }

    // Fallback: Wikipedia Full-Text search
    if results.len() < limit {
        let remaining = limit - results.len();
        if let Ok(wiki_results) = search_wikipedia_fulltext(query, remaining).await {
            extend_unique(&mut results, wiki_results, limit);
        }
    }

    Ok(results)
}

fn extend_unique(results: &mut Vec<WebSearchResult>, incoming: Vec<WebSearchResult>, limit: usize) {
    for result in incoming {
        if results.len() >= limit {
            break;
        }
        if !results.iter().any(|existing| existing.url == result.url) {
            results.push(result);
        }
    }
}

/// Search DuckDuckGo Instant Answer JSON API.
async fn search_duckduckgo(query: &str, limit: usize) -> Result<Vec<WebSearchResult>, String> {
    let encoded = url_encode(query);
    let url =
        format!("https://api.duckduckgo.com/?q={encoded}&format=json&no_html=1&skip_disambig=0");

    let json = fetch_json(&url).await?;
    let mut results = Vec::new();

    // Check primary Abstract
    let heading = json["Heading"].as_str().unwrap_or(query);
    let abstract_text = json["AbstractText"].as_str().unwrap_or("");
    let abstract_url = json["AbstractURL"].as_str().unwrap_or("");

    if !abstract_text.is_empty() && !abstract_url.is_empty() {
        results.push(WebSearchResult {
            title: heading.to_string(),
            url: abstract_url.to_string(),
            snippet: abstract_text.to_string(),
        });
    }

    // Check direct Results array (often contains official sites)
    if let Some(res_arr) = json["Results"].as_array() {
        for item in res_arr {
            if results.len() >= limit {
                break;
            }
            if let (Some(url), Some(text)) = (item["FirstURL"].as_str(), item["Text"].as_str())
                && !url.is_empty()
                && !text.is_empty()
            {
                let title = match text.split_once(" - ") {
                    Some((prefix, _)) => prefix.trim().to_string(),
                    None => text.chars().take(60).collect(),
                };
                if !results.iter().any(|r| r.url == url) {
                    results.push(WebSearchResult {
                        title,
                        url: url.to_string(),
                        snippet: text.to_string(),
                    });
                }
            }
        }
    }

    // Extract RelatedTopics
    if let Some(topics) = json["RelatedTopics"].as_array() {
        extract_topics(topics, &mut results, limit);
    }

    Ok(results)
}

fn extract_topics(topics: &[serde_json::Value], results: &mut Vec<WebSearchResult>, limit: usize) {
    for item in topics {
        if results.len() >= limit {
            break;
        }

        // Direct topic
        if let (Some(text), Some(url)) = (item["Text"].as_str(), item["FirstURL"].as_str())
            && !text.is_empty()
            && !url.is_empty()
        {
            let title = match text.split_once(" - ") {
                Some((prefix, _)) => prefix.trim().to_string(),
                None => text.chars().take(60).collect(),
            };
            if !results.iter().any(|r| r.url == url) {
                results.push(WebSearchResult {
                    title,
                    url: url.to_string(),
                    snippet: text.to_string(),
                });
            }
        }

        // Nested category topic group
        if let Some(sub_topics) = item["Topics"].as_array() {
            extract_topics(sub_topics, results, limit);
        }
    }
}

/// Search StackOverflow via StackExchange API.
async fn search_stackoverflow(query: &str, limit: usize) -> Result<Vec<WebSearchResult>, String> {
    let encoded = url_encode(query);
    let url = format!(
        "https://api.stackexchange.com/2.3/search/advanced?order=desc&sort=relevance&q={encoded}&pagesize={limit}&site=stackoverflow"
    );

    let json = fetch_json(&url).await?;
    let mut results = Vec::new();

    if let Some(items) = json["items"].as_array() {
        for item in items {
            if results.len() >= limit {
                break;
            }
            let title_raw = item["title"].as_str().unwrap_or("");
            let link = item["link"].as_str().unwrap_or("");
            if title_raw.is_empty() || link.is_empty() {
                continue;
            }

            let title = decode_html_entities(title_raw);
            let score = item["score"].as_i64().unwrap_or(0);
            let answer_count = item["answer_count"].as_i64().unwrap_or(0);
            let is_answered = item["is_answered"].as_bool().unwrap_or(false);

            let mut tags_str = String::new();
            if let Some(tags) = item["tags"].as_array() {
                let tag_names: Vec<&str> = tags.iter().filter_map(|t| t.as_str()).take(5).collect();
                if !tag_names.is_empty() {
                    tags_str = format!("[{}] ", tag_names.join(", "));
                }
            }

            let status = if is_answered { "solved" } else { "open" };
            let snippet = format!("{tags_str}Score: {score} | Answers: {answer_count} ({status})");

            results.push(WebSearchResult {
                title,
                url: link.to_string(),
                snippet,
            });
        }
    }

    Ok(results)
}

/// Search Crates.io API for Rust crates.
async fn search_crates(query: &str, limit: usize) -> Result<Vec<WebSearchResult>, String> {
    let encoded = url_encode(query);
    let url = format!("https://crates.io/api/v1/crates?q={encoded}&per_page={limit}");

    let json = fetch_json(&url).await?;
    let mut results = Vec::new();

    if let Some(crates) = json["crates"].as_array() {
        for c in crates {
            if results.len() >= limit {
                break;
            }
            let name = c["name"].as_str().unwrap_or("");
            let desc = c["description"].as_str().unwrap_or("");
            let max_ver = c["max_version"].as_str().unwrap_or("");
            let doc_url = c["documentation"].as_str();

            if !name.is_empty() {
                let url = doc_url
                    .filter(|u| !u.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| format!("https://crates.io/crates/{name}"));

                let snippet = if !desc.is_empty() {
                    format!("v{max_ver}: {desc}")
                } else {
                    format!("Rust crate '{name}' v{max_ver}")
                };

                results.push(WebSearchResult {
                    title: format!("crates.io: {name}"),
                    url,
                    snippet,
                });
            }
        }
    }

    Ok(results)
}

/// Search GitHub repositories API.
async fn search_github(query: &str, limit: usize) -> Result<Vec<WebSearchResult>, String> {
    let encoded = url_encode(query);
    let url = format!("https://api.github.com/search/repositories?q={encoded}&per_page={limit}");

    let json = fetch_json(&url).await?;
    let mut results = Vec::new();

    if let Some(items) = json["items"].as_array() {
        for item in items {
            if results.len() >= limit {
                break;
            }
            let name = item["full_name"].as_str().unwrap_or("");
            let html_url = item["html_url"].as_str().unwrap_or("");
            let desc = item["description"].as_str().unwrap_or("");
            let stars = item["stargazers_count"].as_u64().unwrap_or(0);
            let lang = item["language"].as_str().unwrap_or("");

            if !name.is_empty() && !html_url.is_empty() {
                let mut snippet = String::new();
                if !lang.is_empty() {
                    snippet.push_str(&format!("[{lang}] "));
                }
                if !desc.is_empty() {
                    snippet.push_str(desc);
                    snippet.push(' ');
                }
                snippet.push_str(&format!("(★ {stars})"));

                results.push(WebSearchResult {
                    title: format!("GitHub - {name}"),
                    url: html_url.to_string(),
                    snippet,
                });
            }
        }
    }

    Ok(results)
}

/// Search Wikipedia Opensearch for exact article title matches.
async fn search_wikipedia_opensearch(
    query: &str,
    limit: usize,
) -> Result<Vec<WebSearchResult>, String> {
    let mut results = Vec::new();
    let encoded = url_encode(query);
    let open_url = format!(
        "https://en.wikipedia.org/w/api.php?action=opensearch&search={encoded}&limit={limit}&format=json"
    );

    let json = fetch_json(&open_url).await?;
    let titles = json.get(1).and_then(|v| v.as_array());
    let snippets = json.get(2).and_then(|v| v.as_array());
    let urls = json.get(3).and_then(|v| v.as_array());

    if let (Some(titles), Some(urls)) = (titles, urls) {
        for (i, title_val) in titles.iter().enumerate() {
            if results.len() >= limit {
                break;
            }
            let title = title_val.as_str().unwrap_or("").to_string();
            let url = urls
                .get(i)
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = snippets
                .and_then(|s| s.get(i))
                .and_then(|sn| sn.as_str())
                .unwrap_or("")
                .to_string();

            let snippet = if snippet.is_empty() {
                title.clone()
            } else {
                snippet
            };

            if !title.is_empty() && !url.is_empty() {
                results.push(WebSearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }
    }

    Ok(results)
}

/// Search Wikipedia Full-Text Search API for topic content matches.
async fn search_wikipedia_fulltext(
    query: &str,
    limit: usize,
) -> Result<Vec<WebSearchResult>, String> {
    let mut results = Vec::new();
    let encoded = url_encode(query);
    let srch_url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={encoded}&srlimit={limit}&format=json"
    );

    let json = fetch_json(&srch_url).await?;
    if let Some(items) = json["query"]["search"].as_array() {
        for item in items {
            if results.len() >= limit {
                break;
            }
            let title = item["title"].as_str().unwrap_or("");
            let snippet_raw = item["snippet"].as_str().unwrap_or("");
            if !title.is_empty() {
                let page_url = format!(
                    "https://en.wikipedia.org/wiki/{}",
                    url_encode(&title.replace(' ', "_"))
                );
                let snippet = strip_html_tags(snippet_raw);
                results.push(WebSearchResult {
                    title: title.to_string(),
                    url: page_url,
                    snippet: if snippet.is_empty() {
                        title.to_string()
                    } else {
                        snippet
                    },
                });
            }
        }
    }

    Ok(results)
}

/// Fetch a web page URL and convert HTML to clean Markdown.
pub async fn fetch_page_internal(url: &str) -> Result<String, String> {
    check_fetch_target(url)?;

    // StackOverflow blocks automated HTML scrapers via Cloudflare; use their official API instead
    if let Some(pos) = url.find("stackoverflow.com/questions/") {
        let sub = &url[pos + "stackoverflow.com/questions/".len()..];
        let id_str = sub.split(['/', '?', '#']).next().unwrap_or("");
        if let Ok(id) = id_str.parse::<u64>()
            && let Ok(content) = fetch_stackoverflow_question(id).await
        {
            return Ok(content);
        }
    }

    let res = http_get_follow_redirects(url.to_string(), 3).await?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("fetch {url} returned HTTP {status}"));
    }

    let raw_bytes = read_body_capped(res.into_body(), 512 * 1024).await?;
    let html_content = String::from_utf8_lossy(&raw_bytes);

    // Convert HTML to Markdown bounded to 16KB (~4,000 tokens)
    let markdown = html_to_markdown(&html_content, 16_384);
    if markdown.trim().is_empty() {
        Ok(format!(
            "(Web page {url} returned no readable text content)"
        ))
    } else {
        Ok(markdown)
    }
}

/// Fetch a StackOverflow question and top answers via the StackExchange API.
async fn fetch_stackoverflow_question(id: u64) -> Result<String, String> {
    let q_url = format!(
        "https://api.stackexchange.com/2.3/questions/{id}?site=stackoverflow&filter=withbody"
    );
    let q_json = fetch_json(&q_url).await?;
    let item = q_json["items"]
        .as_array()
        .and_then(|arr| arr.first())
        .ok_or("question not found")?;

    let title = decode_html_entities(item["title"].as_str().unwrap_or(""));
    let body_html = item["body"].as_str().unwrap_or("");
    let body_md = html_to_markdown(body_html, 8_192);

    let mut out = format!("# {title}\n\n{body_md}\n\n## Answers\n\n");

    let a_url = format!(
        "https://api.stackexchange.com/2.3/questions/{id}/answers?order=desc&sort=votes&pagesize=3&site=stackoverflow&filter=withbody"
    );
    if let Ok(a_json) = fetch_json(&a_url).await
        && let Some(answers) = a_json["items"].as_array()
    {
        for (i, ans) in answers.iter().enumerate() {
            let score = ans["score"].as_i64().unwrap_or(0);
            let is_accepted = ans["is_accepted"].as_bool().unwrap_or(false);
            let ans_html = ans["body"].as_str().unwrap_or("");
            let ans_md = html_to_markdown(ans_html, 8_192);
            let badge = if is_accepted { " (Accepted)" } else { "" };
            let num = i + 1;
            out.push_str(&format!(
                "### Answer {num} (Score: {score}{badge})\n\n{ans_md}\n\n---\n\n"
            ));
        }
    }

    Ok(out.trim_end_matches("\n---\n\n").to_string())
}

/// Strip basic HTML tags from a text string.
fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(ch);
        }
    }
    decode_html_entities(out.trim())
}

/// Minimal HTML entity decoder.
fn decode_html_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_fetch_target() {
        assert!(check_fetch_target("https://docs.rs/serde").is_ok());
        assert!(check_fetch_target("http://192.168.1.20:8080/").is_ok());
        assert!(check_fetch_target("http://localhost:11434/").is_ok());

        let err = check_fetch_target("http://169.254.169.254/latest/meta-data/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch 169.254.169.254: cloud metadata endpoint"
        );

        let err = check_fetch_target("http://[::ffff:169.254.169.254]/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch ::ffff:169.254.169.254: cloud metadata endpoint"
        );

        let err = check_fetch_target("http://metadata.google.internal/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch metadata.google.internal: cloud metadata endpoint"
        );

        let err = check_fetch_target("http://[fd00:ec2::254]/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch fd00:ec2::254: cloud metadata endpoint"
        );

        assert!(check_fetch_target("ftp://x.com/").is_err());
        assert!(check_fetch_target("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_check_fetch_target_blocks_alternate_ip_encodings() {
        // Dword decimal.
        let err = check_fetch_target("http://2852039166/").unwrap_err();
        assert_eq!(err, "refusing to fetch 2852039166: cloud metadata endpoint");
        // Full hex.
        let err = check_fetch_target("http://0xA9FEA9FE/").unwrap_err();
        assert_eq!(err, "refusing to fetch 0xa9fea9fe: cloud metadata endpoint");
        // Dotted hex per-octet.
        let err = check_fetch_target("http://0xA9.0xFE.0xA9.0xFE/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch 0xa9.0xfe.0xa9.0xfe: cloud metadata endpoint"
        );
        // Dotted octal.
        let err = check_fetch_target("http://0251.0376.0251.0376/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch 0251.0376.0251.0376: cloud metadata endpoint"
        );
        // 3-part short form.
        let err = check_fetch_target("http://169.254.43518/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch 169.254.43518: cloud metadata endpoint"
        );
        // 2-part short form.
        let err = check_fetch_target("http://169.16689662/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch 169.16689662: cloud metadata endpoint"
        );
        // IPv4-mapped IPv6, dotted-quad form.
        let err = check_fetch_target("http://[::ffff:169.254.169.254]/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch ::ffff:169.254.169.254: cloud metadata endpoint"
        );
        // IPv4-mapped IPv6, pure-hex form.
        let err = check_fetch_target("http://[::ffff:a9fe:a9fe]/").unwrap_err();
        assert_eq!(
            err,
            "refusing to fetch ::ffff:a9fe:a9fe: cloud metadata endpoint"
        );

        // A legitimate hostname and a legitimate LAN IP still pass through unaffected.
        assert!(check_fetch_target("https://docs.rs/serde").is_ok());
        assert!(check_fetch_target("http://192.168.1.20:8080/").is_ok());
    }

    #[test]
    fn test_resolve_redirect() {
        let base = "https://a.com:8443/docs/x?q".parse::<Uri>().unwrap();

        assert_eq!(
            resolve_redirect(&base, "/y").unwrap(),
            "https://a.com:8443/y"
        );
        assert_eq!(
            resolve_redirect(&base, "y").unwrap(),
            "https://a.com:8443/docs/y"
        );
        assert_eq!(
            resolve_redirect(&base, "../z").unwrap(),
            "https://a.com:8443/z"
        );
        assert_eq!(
            resolve_redirect(&base, "//b.com/p").unwrap(),
            "https://b.com/p"
        );
        assert_eq!(
            resolve_redirect(&base, "https://c.com/").unwrap(),
            "https://c.com/"
        );
        assert_eq!(
            resolve_redirect(&base, "?r=1").unwrap(),
            "https://a.com:8443/docs/x?r=1"
        );
    }

    #[test]
    fn test_read_body_capped() {
        let cap = 512 * 1024;
        let data = Bytes::from(vec![b'x'; 1024 * 1024]);
        let body = http_body_util::Full::new(data);
        let result = futures::executor::block_on(read_body_capped(body, cap)).unwrap();
        assert_eq!(result.len(), cap);
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("&amp;lt;b&amp;gt;"), "&lt;b&gt;");
    }
}
