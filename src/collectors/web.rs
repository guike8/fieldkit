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

pub(super) fn http(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let requested = public_url(target)?;
    let response = ctx.fetch(requested.as_str(), true)?;
    let sources = vec![response.source_id.clone()];
    let media = response
        .headers
        .get("content-type")
        .map(|v| v.split(';').next().unwrap_or(""))
        .unwrap_or("");
    let mut data = if matches!(media, "text/html" | "application/xhtml+xml") {
        page_metadata(&response.text(), &response.url, ctx.cfg.max_results)
    } else {
        json!({"links":[],"scripts_executed":false})
    };
    data.as_object_mut().unwrap().extend(json!({"requested_url":requested.as_str(),"url":response.url,"http_status":response.status,"media_type":media,"body_bytes":response.body.len(),"body_sha256":digest(&response.body),"headers":response.headers,"source_ids":sources}).as_object().unwrap().clone());
    let links = data["links"].as_array().cloned().unwrap_or_default();
    let mut p = Payload::new(data);
    p.entity("url", response.url.clone(), &sources);
    // Error/denial pages may contain links, but do not seed verified graph relationships.
    if (200..300).contains(&response.status) {
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
    }
    if let Err(e) = response.check() {
        p.status = error_status(&e).into();
        p.errors.push(error_value(&e));
    }
    Ok(p)
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
