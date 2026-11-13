use super::*;
use scraper::{ElementRef, Html, Selector};

pub(super) fn page_metadata(body: &str, base: &str, maximum: usize) -> Value {
    let page = Html::parse_document(body);
    let base_url = Url::parse(base).ok();
    let select = |pattern: &str| Selector::parse(pattern).unwrap();
    let title = page
        .select(&select("title"))
        .next()
        .map(|n| clean(&n.text().collect::<String>(), 1000));
    let effective_base = page
        .select(&select("base[href]"))
        .next()
        .and_then(|n| n.value().attr("href"))
        .and_then(|s| base_url.as_ref()?.join(s).ok())
        .filter(|u| public_url(u.as_str()).is_ok())
        .or(base_url);
    let canonical = page
        .select(&select("link[rel~='canonical'][href]"))
        .next()
        .and_then(|n| n.value().attr("href"))
        .and_then(|s| effective_base.as_ref()?.join(s).ok())
        .filter(|u| public_url(u.as_str()).is_ok())
        .map(|u| u.to_string());
    let metadata=page.select(&select("meta[content]")).filter_map(|n|{let name=n.value().attr("name").or_else(||n.value().attr("property"))?;Some(json!({"name":clean(name,100),"content":clean(n.value().attr("content").unwrap_or(""),2000)}))}).take(maximum).collect::<Vec<_>>();
    let mut content_links = vec![];
    let mut navigation_links = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let mut seen_count = 0;
    for node in page.select(&select("a[href]")) {
        let href = node.value().attr("href").unwrap_or("");
        let Some(url) = effective_base
            .as_ref()
            .and_then(|b| b.join(href).ok())
            .and_then(|u| public_url(u.as_str()).ok())
        else {
            continue;
        };
        seen_count += 1;
        if seen.insert(url.to_string()) {
            let navigation = node.ancestors().filter_map(ElementRef::wrap).any(|parent| {
                ["nav", "header", "footer"].contains(&parent.value().name())
                    || ["navigation", "banner", "contentinfo"]
                        .contains(&parent.value().attr("role").unwrap_or(""))
            });
            let links = if navigation {
                &mut navigation_links
            } else {
                &mut content_links
            };
            if links.len() < maximum {
                links.push(json!({"url":url.as_str(),"text":clean(&node.text().collect::<String>(),300),"context":if navigation{"navigation"}else{"page_content"}}));
            }
        }
    }
    let links: Vec<_> = content_links
        .into_iter()
        .chain(navigation_links)
        .take(maximum)
        .collect();
    json!({"title":title,"metadata":metadata,"canonical_url":canonical,"links":links,"links_seen":seen_count,"links_truncated":seen.len()>maximum,"scripts_executed":false})
}

// Preserve exact decoded JSON strings and their source pointers. These are
// navigation leads, not proof that the linked resource exists or is trusted.
fn json_urls(body: &[u8], eligible: bool, maximum: usize, sources: &[String]) -> Value {
    fn visit(
        value: &Value,
        pointer: &str,
        maximum: usize,
        sources: &[String],
        urls: &mut Vec<Value>,
        total: &mut usize,
        path_limit_exceeded: &mut bool,
    ) {
        if pointer.len() > 128 {
            *path_limit_exceeded = true;
            return;
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    visit(
                        value,
                        &format!("{pointer}/{key}"),
                        maximum,
                        sources,
                        urls,
                        total,
                        path_limit_exceeded,
                    );
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    visit(
                        value,
                        &format!("{pointer}/{index}"),
                        maximum,
                        sources,
                        urls,
                        total,
                        path_limit_exceeded,
                    );
                }
            }
            Value::String(url)
                if !pointer.is_empty() && url.len() <= 2048 && public_url(url).is_ok() =>
            {
                *total += 1;
                if urls.len() < maximum {
                    urls.push(json!({"url":url,"source_pointer":pointer,"source_ids":sources,"scope":"json_string_url_not_fetched_or_attributed"}));
                }
            }
            _ => {}
        }
    }
    let parsed = eligible
        .then(|| serde_json::from_slice::<Value>(body).ok())
        .flatten();
    let mut urls = vec![];
    let mut total = 0;
    let mut path_limit_exceeded = false;
    if let Some(value) = &parsed {
        visit(
            value,
            "",
            maximum,
            sources,
            &mut urls,
            &mut total,
            &mut path_limit_exceeded,
        );
    }
    json!({"urls":urls,"count":urls.len(),"total_count":total,"truncated":total>urls.len() || path_limit_exceeded,"path_limit_exceeded":path_limit_exceeded,"source_pointer_max_bytes":128,"response_eligible":eligible,"valid_json":parsed.is_some(),"scope":"json_string_url_not_fetched_or_attributed"})
}

pub(super) fn http(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let requested = public_url(target)?;
    let response = ctx.fetch(requested.as_str(), true)?;
    Ok(http_response(target, response, ctx.cfg.max_results))
}

// Kept separate from transport so response gating and source provenance can be
// tested entirely offline.
fn http_response(requested: &str, response: Response, maximum: usize) -> Payload {
    let sources = vec![response.source_id.clone()];
    let media = response
        .headers
        .get("content-type")
        .map(|v| v.split(';').next().unwrap_or(""))
        .unwrap_or("");
    let html = media.trim().eq_ignore_ascii_case("text/html")
        || media.trim().eq_ignore_ascii_case("application/xhtml+xml");
    let successful_html = (200..300).contains(&response.status) && html;
    let mut data = if html {
        page_metadata(&response.text(), &response.url, maximum)
    } else {
        json!({"links":[],"scripts_executed":false})
    };
    // Analyze ORIGINAL source bytes, not Response::text()'s lossy UTF-8 rendering.
    // Denials, failed requests and non-HTML responses cannot seed these claims.
    let mut tracking =
        crate::tracking::extract(if successful_html { &response.body } else { &[] }, maximum);
    tracking["response_eligible"] = json!(successful_html);
    data["tracking_identifiers"] = tracking;
    let media_lower = media.trim().to_ascii_lowercase();
    let successful_json = (200..300).contains(&response.status)
        && (media_lower == "application/json"
            || (media_lower.starts_with("application/") && media_lower.ends_with("+json")));
    data["json_urls"] = json_urls(&response.body, successful_json, maximum, &sources);
    data.as_object_mut().unwrap().extend(json!({"requested_url":requested,"url":response.url,"http_status":response.status,"media_type":media,"body_bytes":response.body.len(),"body_sha256":digest(&response.body),"headers":response.headers,"source_ids":sources}).as_object().unwrap().clone());
    let links = data["links"].as_array().cloned().unwrap_or_default();
    let mut p = Payload::new(data);
    p.entity("url", response.url.clone(), &sources);
    // Error/denial pages may contain links, but do not seed verified graph relationships.
    if (200..300).contains(&response.status) {
        // Bind the requested target to the successful final response. This is
        // request/result metadata, not a claim about any individual redirect hop.
        // Keep the exact target, including spelling normalized by transport, so
        // a parent JSON-string observation remains connected to this response.
        if requested != response.url {
            p.entity("url", requested, &sources);
            p.edge(
                "url",
                requested,
                "request_resolved_to",
                "url",
                response.url.clone(),
                &sources,
            );
        }
        let mut seen_json_urls = std::collections::BTreeSet::new();
        for observed in p.data["json_urls"]["urls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(url) = observed["url"].as_str()
                && seen_json_urls.insert(url.to_owned())
            {
                p.entity("url", url, &sources);
                p.edge(
                    "url",
                    response.url.clone(),
                    "contains_json_url",
                    "url",
                    url,
                    &sources,
                );
            }
        }
        for link in links {
            // Site-wide menus are retained as page metadata, but do not become
            // profile correlations or recursive collection leads.
            if link["context"] == "navigation" {
                continue;
            }
            if let Some(url) = link["url"].as_str() {
                p.entity("url", url, &sources);
                p.edge(
                    "url",
                    response.url.clone(),
                    "links_to",
                    "url",
                    url,
                    &sources,
                );
            }
        }
        if let Some(canonical) = p.data["canonical_url"].as_str().map(str::to_owned) {
            p.entity("url", canonical.clone(), &sources);
            p.edge(
                "url",
                response.url.clone(),
                "declares_canonical",
                "url",
                canonical,
                &sources,
            );
        }
        if successful_html {
            let occurrences = p.data["tracking_identifiers"]["identifiers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut seen = std::collections::BTreeSet::new();
            for occurrence in occurrences {
                let (Some(kind), Some(value)) =
                    (occurrence["kind"].as_str(), occurrence["value"].as_str())
                else {
                    continue;
                };
                // Occurrences/offsets remain in data; graph nodes/edges are unique
                // per exact canonical identifier, not an inferred account prefix.
                if seen.insert((kind.to_owned(), value.to_owned())) {
                    p.entity(kind, value, &sources);
                    p.edge(
                        "url",
                        response.url.clone(),
                        "contains_tracking_literal",
                        kind,
                        value,
                        &sources,
                    );
                }
            }
        }
    }
    if let Err(e) = response.check() {
        p.status = error_status(&e).into();
        p.errors.push(error_value(&e));
    }
    p
}

pub(super) fn search(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    if target.trim().is_empty() || target.len() > 2048 || target.chars().any(char::is_control) {
        return Err(fail(
            "invalid",
            "INVALID_QUERY",
            "Search query must contain 1–2048 printable characters",
        ));
    }
    let key = std::env::var("BRAVE_SEARCH_API_KEY").map_err(|_| {
        fail(
            "unavailable",
            "SEARCH_KEY_REQUIRED",
            "Set BRAVE_SEARCH_API_KEY to run public web search",
        )
    })?;
    let limit = opt_limit(options, 10, ctx.cfg.max_results.min(20));
    let mut url = Url::parse("https://api.search.brave.com/res/v1/web/search")?;
    url.query_pairs_mut()
        .append_pair("q", target)
        .append_pair("count", &limit.to_string());
    let response = ctx.fetch_headers(
        url.as_str(),
        false,
        &[
            ("X-Subscription-Token", &key),
            ("Accept", "application/json"),
        ],
    )?;
    response.check()?;
    let value = response.json()?;
    let results = value
        .pointer("/web/results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            fail(
                "unknown",
                "SEARCH_SCHEMA",
                "Search response lacks web.results",
            )
        })?;
    let sources = vec![response.source_id];
    let hits=results.iter().take(limit).filter_map(|r|{let url=public_url(r["url"].as_str()?).ok()?;Some(json!({"url":url.as_str(),"title":clean(r["title"].as_str().unwrap_or(""),500),"description":clean(r["description"].as_str().unwrap_or(""),2000),"source_ids":sources,"verification":"unverified_search_result"}))}).collect::<Vec<_>>();
    // Search engine snippets remain discovery leads; they do not attest target-page claims.
    let mut p = Payload::new(
        json!({"query":target,"provider":"Brave Search","hits":hits,"results":hits,"source_ids":sources,"scope":"Search index observations; result pages are unverified until collected"}),
    );
    for h in hits {
        p.entity("search_result_url", h["url"].clone(), &sources);
    }
    Ok(p)
}

pub(super) fn archive(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let original = public_url(target)?;
    let limit = opt_limit(options, 50, ctx.cfg.max_results.min(500));
    let prefix = options["prefix"].as_bool().unwrap_or(false);
    let mut url = Url::parse("https://web.archive.org/cdx/search/cdx")?;
    url.query_pairs_mut()
        .append_pair("url", original.as_str())
        .append_pair("output", "json")
        .append_pair("fl", "timestamp,original,statuscode,mimetype,digest")
        .append_pair("matchType", if prefix { "prefix" } else { "exact" })
        .append_pair("limit", &(limit + 1).to_string());
    let response = ctx.fetch(url.as_str(), false)?;
    response.check()?;
    let raw = response.json()?;
    let rows = raw.as_array().ok_or_else(|| {
        fail(
            "unknown",
            "ARCHIVE_SCHEMA",
            "Archive did not return CDX rows",
        )
    })?;
    if !rows.is_empty()
        && rows[0] != json!(["timestamp", "original", "statuscode", "mimetype", "digest"])
    {
        return Err(fail(
            "unknown",
            "ARCHIVE_SCHEMA",
            "Archive CDX header changed",
        ));
    }
    let sources = vec![response.source_id];
    let mut captures = vec![];
    let mut invalid = 0;
    for row in rows.iter().skip(1).take(limit) {
        let Some(row) = row
            .as_array()
            .filter(|r| r.len() == 5 && r.iter().all(Value::is_string))
        else {
            invalid += 1;
            continue;
        };
        let stamp = row[0].as_str().unwrap();
        let Some(date) = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S").ok() else {
            invalid += 1;
            continue;
        };
        let Ok(captured) = public_url(row[1].as_str().unwrap()) else {
            invalid += 1;
            continue;
        };
        captures.push(json!({"timestamp":stamp,"captured_at":date.and_utc().to_rfc3339(),"original_url":captured.as_str(),"http_status":row[2].as_str().unwrap().parse::<u16>().ok(),"mimetype":row[3],"archive_digest":row[4],"replay_url":format!("https://web.archive.org/web/{stamp}id_/{captured}"),"source_ids":sources}));
    }
    let mut p = Payload::new(
        json!({"url":original.as_str(),"captures":captures,"returned":captures.len(),"limit":limit,"truncated":rows.len().saturating_sub(1)>limit,"invalid_records":invalid,"replays_fetched":false,"source_ids":sources}),
    );
    for capture in captures {
        p.entity("url", capture["original_url"].clone(), &sources);
        p.entity("archive_capture", capture["replay_url"].clone(), &sources);
        p.edge(
            "url",
            capture["original_url"].clone(),
            "archived_at",
            "archive_capture",
            capture["replay_url"].clone(),
            &sources,
        );
    }
    if invalid > 0 {
        p.status = "partial".into();
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracking_response(status: u16, media: &str) -> Response {
        let mut body = vec![0xff];
        body.extend_from_slice(
            "雪<script>ga('create', 'UA-101199457-4');
            ga('create', 'UA-101199457-5');
            ga('create', 'UA-101199457-4');</script>
            <ins data-ad-client='ca-pub-8264869885899896'></ins>
            <a href='/article'>Article</a><nav><a href='/menu'>Menu</a></nav>
            <link rel='canonical' href='/canonical'>"
                .as_bytes(),
        );
        Response {
            url: "https://example.org/final".into(),
            status,
            headers: BTreeMap::from([("content-type".into(), media.into())]),
            body,
            source_id: "src_actual_final_response".into(),
        }
    }

    #[test]
    fn successful_html_tracking_preserves_source_bytes_and_existing_links() {
        let requested = Url::parse("https://example.org/requested").unwrap();
        let response = tracking_response(200, "text/html; charset=utf-8");
        let original = response.body.clone();
        let payload = http_response(requested.as_str(), response, 20);
        assert_eq!(payload.status, "ok");
        let tracking = &payload.data["tracking_identifiers"];
        assert_eq!(tracking["count"], 4);
        assert_eq!(tracking["total_count"], 4);
        assert_eq!(tracking["response_eligible"], true);
        assert_eq!(tracking["scripts_executed"], false);
        for occurrence in tracking["identifiers"].as_array().unwrap() {
            let start = occurrence["byte_start"].as_u64().unwrap() as usize;
            let end = occurrence["byte_end"].as_u64().unwrap() as usize;
            assert_eq!(
                &original[start..end],
                occurrence["raw_value"].as_str().unwrap().as_bytes()
            );
        }
        let edges: Vec<_> = payload
            .relationships
            .iter()
            .filter(|edge| edge["relation"] == "contains_tracking_literal")
            .collect();
        assert_eq!(edges.len(), 3); // duplicate occurrences do not duplicate edges
        for edge in &edges {
            assert_eq!(edge["source"]["type"], "url");
            assert_eq!(edge["source"]["value"], "https://example.org/final");
            assert_eq!(edge["source_ids"], json!(["src_actual_final_response"]));
        }
        assert_eq!(edges[0]["target"]["type"], "google_analytics_ua_property");
        assert_eq!(edges[0]["target"]["value"], "UA-101199457-4");
        assert_eq!(edges[1]["target"]["value"], "UA-101199457-5");
        assert_eq!(edges[2]["target"]["type"], "google_adsense_publisher");
        assert_eq!(edges[2]["target"]["value"], "pub-8264869885899896");
        for entity in &payload.entities {
            assert_eq!(entity["source_ids"], json!(["src_actual_final_response"]));
        }
        let links: Vec<_> = payload
            .relationships
            .iter()
            .filter(|edge| edge["relation"] == "links_to")
            .collect();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0]["target"]["value"], "https://example.org/article");
        assert!(
            payload
                .relationships
                .iter()
                .any(|edge| edge["relation"] == "declares_canonical")
        );
        assert_eq!(payload.data["links"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn failed_denied_redirect_and_non_html_responses_do_not_seed_tracking() {
        let requested = Url::parse("https://example.org/").unwrap();
        for (status, media) in [
            (401, "text/html"),
            (403, "text/html"),
            (404, "text/html"),
            (429, "text/html"),
            (500, "text/html"),
            (302, "text/html"),
            (200, "text/plain"),
            (200, "application/json"),
            (200, ""),
        ] {
            let payload = http_response(requested.as_str(), tracking_response(status, media), 20);
            assert_eq!(payload.data["tracking_identifiers"]["count"], 0);
            assert_eq!(
                payload.data["tracking_identifiers"]["response_eligible"],
                false
            );
            assert!(
                !payload
                    .relationships
                    .iter()
                    .any(|edge| edge["relation"] == "contains_tracking_literal")
            );
            assert!(
                !payload
                    .entities
                    .iter()
                    .any(|entity| entity["type"] == "google_analytics_ua_property"
                        || entity["type"] == "google_adsense_publisher")
            );
            if status >= 300 {
                assert!(payload.relationships.is_empty());
                assert_ne!(payload.status, "ok");
            }
        }
    }

    #[test]
    fn html_tracking_result_and_graph_are_bounded_together() {
        let requested = Url::parse("https://example.org/").unwrap();
        let payload = http_response(requested.as_str(), tracking_response(201, "TEXT/HTML"), 1);
        assert_eq!(payload.data["tracking_identifiers"]["count"], 1);
        assert_eq!(payload.data["tracking_identifiers"]["total_count"], 4);
        assert_eq!(payload.data["tracking_identifiers"]["truncated"], true);
        assert_eq!(
            payload
                .relationships
                .iter()
                .filter(|edge| edge["relation"] == "contains_tracking_literal")
                .count(),
            1
        );
        let zero = http_response(
            requested.as_str(),
            tracking_response(200, "application/xhtml+xml"),
            0,
        );
        assert_eq!(zero.data["tracking_identifiers"]["count"], 0);
        assert_eq!(zero.data["tracking_identifiers"]["truncated"], true);
        assert!(
            !zero
                .relationships
                .iter()
                .any(|edge| edge["relation"] == "contains_tracking_literal")
        );
    }

    fn json_response(status: u16, media: &str, body: &[u8]) -> Response {
        Response {
            url: "https://example.org/final.json".into(),
            status,
            headers: BTreeMap::from([("content-type".into(), media.into())]),
            body: body.to_vec(),
            source_id: "src_json_final".into(),
        }
    }

    #[test]
    fn json_url_observations_preserve_exact_strings_source_pointers_and_direction() {
        let raw = json!({"a/b~c":["https://example.org:443/file#section","https://example.org:443/file#section"],"other":"https://other.example/data","private":"http://127.0.0.1/","credentials":"https://user:pass@example.org/","prose":"Read https://example.org/"});
        let body = serde_json::to_vec(&raw).unwrap();
        let requested = Url::parse("https://example.org/original").unwrap();
        let p = http_response(
            requested.as_str(),
            json_response(200, "Application/Problem+Json; charset=utf-8", &body),
            10,
        );
        assert_eq!(p.data["json_urls"]["valid_json"], true);
        assert_eq!(p.data["json_urls"]["response_eligible"], true);
        assert_eq!(p.data["json_urls"]["count"], 3);
        for observed in p.data["json_urls"]["urls"].as_array().unwrap() {
            assert_eq!(
                raw.pointer(observed["source_pointer"].as_str().unwrap())
                    .unwrap(),
                &observed["url"]
            );
            assert_eq!(observed["source_ids"], json!(["src_json_final"]));
        }
        assert_eq!(
            p.data["json_urls"]["urls"][0]["source_pointer"],
            "/a~1b~0c/0"
        );
        assert_eq!(
            p.data["json_urls"]["urls"][0]["url"],
            "https://example.org:443/file#section"
        );
        assert_eq!(p.relationships.len(), 3);
        let binding = p
            .relationships
            .iter()
            .find(|edge| edge["relation"] == "request_resolved_to")
            .unwrap();
        assert_eq!(binding["source"]["value"], requested.as_str());
        assert_eq!(binding["target"]["value"], "https://example.org/final.json");
        assert_eq!(binding["source_ids"], json!(["src_json_final"]));
        for edge in p
            .relationships
            .iter()
            .filter(|edge| edge["relation"] == "contains_json_url")
        {
            assert_eq!(edge["source"]["value"], "https://example.org/final.json");
            assert_eq!(edge["relation"], "contains_json_url");
            assert_eq!(edge["source_ids"], json!(["src_json_final"]));
        }
    }

    #[test]
    fn exact_json_url_target_stays_connected_after_transport_normalization() {
        for target in [
            "https://example.org:443/file#section",
            "HTTPS://EXAMPLE.ORG:443/file",
        ] {
            let parent = http_response(
                "https://example.org/parent.json",
                json_response(
                    200,
                    "application/json",
                    &serde_json::to_vec(&json!({"url":target})).unwrap(),
                ),
                10,
            );
            let observed = parent.data["json_urls"]["urls"][0]["url"].as_str().unwrap();
            let normalized = public_url(observed).unwrap();
            assert_ne!(observed, normalized.as_str());
            let mut response = json_response(200, "application/json", b"{}");
            response.url = normalized.to_string();
            let child = http_response(observed, response, 10);
            assert_eq!(child.data["requested_url"], target);
            let parent_edge = parent
                .relationships
                .iter()
                .find(|e| e["relation"] == "contains_json_url")
                .unwrap();
            let binding = child
                .relationships
                .iter()
                .find(|e| e["relation"] == "request_resolved_to")
                .unwrap();
            assert_eq!(parent_edge["target"], binding["source"]);
            assert_eq!(binding["target"]["value"], normalized.as_str());
            assert_eq!(binding["source_ids"], json!(["src_json_final"]));
        }
        let target = "https://example.org:443/file#section";
        let mut denied = json_response(403, "text/plain", b"denied");
        denied.url = public_url(target).unwrap().to_string();
        let p = http_response(target, denied, 10);
        assert_eq!(p.data["requested_url"], target);
        assert!(p.relationships.is_empty());
    }

    #[test]
    fn json_url_discovery_fails_closed_for_denials_non_json_and_malformed_bodies() {
        let requested = Url::parse("https://example.org/requested").unwrap();
        let body = br#"{"url":"https://other.example/"}"#;
        for (status, media, raw) in [
            (403, "application/json", body.as_slice()),
            (429, "application/json", body.as_slice()),
            (500, "application/json", body.as_slice()),
            (302, "application/json", body.as_slice()),
            (200, "text/plain", body.as_slice()),
            (200, "text/html", body.as_slice()),
            (200, "application/json", b"{broken".as_slice()),
        ] {
            let p = http_response(requested.as_str(), json_response(status, media, raw), 10);
            assert_eq!(p.data["json_urls"]["count"], 0);
            assert_eq!(p.data["json_urls"]["valid_json"], false);
            assert!(
                !p.relationships
                    .iter()
                    .any(|e| e["relation"] == "contains_json_url")
            );
        }
    }

    #[test]
    fn json_url_source_paths_are_bounded_and_incomplete_coverage_is_explicit() {
        let mut raw = json!({});
        raw.as_object_mut()
            .unwrap()
            .insert("a".repeat(127), json!("https://a.example/"));
        raw.as_object_mut()
            .unwrap()
            .insert("b".repeat(128), json!(["https://b.example/"]));
        raw.as_object_mut()
            .unwrap()
            .insert("c/".repeat(64), json!("https://c.example/"));
        let extracted = json_urls(&serde_json::to_vec(&raw).unwrap(), true, 20, &[]);
        assert_eq!(extracted["count"], 1);
        assert_eq!(
            extracted["urls"][0]["source_pointer"]
                .as_str()
                .unwrap()
                .len(),
            128
        );
        assert_eq!(extracted["source_pointer_max_bytes"], 128);
        assert_eq!(extracted["path_limit_exceeded"], true);
        assert_eq!(extracted["truncated"], true);
    }

    #[test]
    fn json_url_metadata_and_graph_are_bounded_together_even_at_zero() {
        let requested = Url::parse("https://example.org/requested").unwrap();
        let body = br#"["https://a.example/","https://b.example/"]"#;
        for maximum in [0, 1] {
            let p = http_response(
                requested.as_str(),
                json_response(200, "application/json", body),
                maximum,
            );
            assert_eq!(p.data["json_urls"]["total_count"], 2);
            assert_eq!(p.data["json_urls"]["count"], maximum);
            assert_eq!(p.data["json_urls"]["truncated"], true);
            assert_eq!(
                p.relationships
                    .iter()
                    .filter(|edge| edge["relation"] == "contains_json_url")
                    .count(),
                maximum
            );
        }
    }

    #[test]
    fn page_content_precedes_navigation_without_promoting_global_menus() {
        let page = page_metadata(
            "<header><nav><a href='/help'>Help</a></nav></header><main><a href='https://project.example/'>Project</a></main><footer><a href='/terms'>Terms</a></footer>",
            "https://example.org/",
            2,
        );
        assert_eq!(page["links"][0]["url"], "https://project.example/");
        assert_eq!(page["links"][0]["context"], "page_content");
        assert_eq!(page["links"][1]["context"], "navigation");
        assert_eq!(page["links_truncated"], true);
    }
    #[test]
    fn extracts_literals_and_bounds_links() {
        let p = page_metadata(
            "<title>Example &amp; research</title><meta name='description' content='literal'><a href='/about'>About</a><a href='javascript:alert(1)'>bad</a><a href='https://other.example/x'>Other</a>",
            "https://example.org/",
            1,
        );
        assert_eq!(p["title"], "Example & research");
        assert_eq!(p["links"][0]["url"], "https://example.org/about");
        assert_eq!(p["links"].as_array().unwrap().len(), 1);
        assert_eq!(p["links_truncated"], true);
        assert_eq!(p["scripts_executed"], false);
    }
}
