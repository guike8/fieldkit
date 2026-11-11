use super::*;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

fn normalize(target: &str, post_required: bool) -> Result<(String, Option<u64>, String)> {
    if target.len() > 2048 || target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(fail(
            "invalid",
            "INVALID_TELEGRAM_TARGET",
            "Supply a public Telegram handle or HTTPS t.me link",
        ));
    }
    let (mut handle, mut post) = (target.trim_start_matches('@').to_owned(), None);
    if target.contains("://") {
        let url = public_url(target)?;
        if url.scheme() != "https"
            || !["t.me", "www.t.me", "telegram.me", "www.telegram.me"]
                .contains(&url.host_str().unwrap_or(""))
            || url.port().is_some_and(|p| p != 443)
        {
            return Err(fail(
                "invalid",
                "INVALID_TELEGRAM_TARGET",
                "Only public HTTPS t.me links are supported",
            ));
        }
        let mut parts = url
            .path_segments()
            .unwrap()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if parts.first() == Some(&"s") {
            parts.remove(0);
        }
        if parts.is_empty() || parts.len() > 2 {
            return Err(fail(
                "invalid",
                "INVALID_TELEGRAM_TARGET",
                "Expected public channel or numeric message link",
            ));
        }
        handle = parts[0].into();
        if parts.len() == 2 {
            post = Some(parts[1].parse::<u64>().map_err(|_| {
                fail(
                    "invalid",
                    "INVALID_TELEGRAM_POST",
                    "Message ID must be numeric",
                )
            })?);
        }
    }
    if !Regex::new(r"^[A-Za-z][A-Za-z0-9_]{3,31}$")?.is_match(&handle)
        || [
            "joinchat",
            "share",
            "proxy",
            "socks",
            "login",
            "confirmphone",
            "addstickers",
            "addemoji",
            "iv",
        ]
        .contains(&handle.to_ascii_lowercase().as_str())
        || post.is_some_and(|n| n == 0 || n > 2147483647)
    {
        return Err(fail(
            "invalid",
            "INVALID_TELEGRAM_TARGET",
            "Unsupported or non-public Telegram target",
        ));
    }
    if post_required && post.is_none() {
        return Err(fail(
            "invalid",
            "TELEGRAM_POST_REQUIRED",
            "Supply an exact public message URL",
        ));
    }
    let canonical = format!(
        "https://t.me/{handle}{}",
        post.map(|p| format!("/{p}")).unwrap_or_default()
    );
    Ok((handle, post, canonical))
}
fn selector(value: &str) -> Selector {
    Selector::parse(value).unwrap()
}
fn text(node: ElementRef<'_>, pattern: &str, max: usize) -> Option<String> {
    node.select(&selector(pattern))
        .next()
        .map(|n| clean(&n.text().collect::<String>(), max))
}
fn links(node: ElementRef<'_>, base: &str, max: usize) -> Vec<String> {
    let mut found = vec![];
    let Ok(base) = Url::parse(base) else {
        return found;
    };
    for anchor in node.select(&selector("a[href]")) {
        if let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|s| base.join(s).ok())
            .and_then(|u| public_url(u.as_str()).ok())
        {
            if !found.contains(&url.to_string()) {
                found.push(url.to_string());
            }
            if found.len() >= max {
                break;
            }
        }
    }
    found
}
fn channel_evidence(page: &Html, handle: &str) -> Option<Value> {
    if let Some(name) = page
        .select(&selector(".tgme_channel_info_header_username"))
        .next()
        .map(|n| clean(&n.text().collect::<String>(), 200))
        && name.trim_start_matches('@').eq_ignore_ascii_case(handle)
    {
        return Some(json!({"field":"public_channel_username","expected":handle,"actual":name}));
    }
    page.select(&selector(".tgme_page_context_link[href]")).filter_map(|n|n.value().attr("href")).filter_map(|s|Url::parse(s).ok()).find(|u|u.scheme()=="https"&&u.host_str()==Some("t.me")&&u.path().trim_end_matches('/').eq_ignore_ascii_case(&format!("/s/{handle}"))).map(|u|json!({"field":"public_preview_link","expected":format!("https://t.me/s/{handle}"),"actual":u.as_str()}))
}
fn parse_message(node: ElementRef<'_>, handle: &str, source: &str, max: usize) -> Option<Value> {
    let raw = node.value().attr("data-post")?;
    let (actual, id) = raw.split_once('/')?;
    let identifier = id.parse::<u64>().ok()?;
    if !actual.eq_ignore_ascii_case(handle) || identifier == 0 || identifier > 2147483647 {
        return None;
    }
    let canonical = format!("https://t.me/{handle}/{identifier}");
    let text_node = node.select(&selector(".tgme_widget_message_text")).next();
    let full = text_node
        .map(|n| clean(&n.text().collect::<String>(), 10001))
        .unwrap_or_default();
    let content = full.chars().take(10000).collect::<String>();
    let timestamp = node
        .select(&selector("time[datetime]"))
        .next()
        .and_then(|n| n.value().attr("datetime"))
        .map(|s| clean(s, 128));
    let mut media = vec![];
    let css_url = Regex::new(r#"url\(['"]?([^)'"\s]+)"#).unwrap();
    for item in node.select(&selector(
        "video[src],audio[src],.tgme_widget_message_photo_wrap[style]",
    )) {
        let (kind, url) = if let Some(src) = item.value().attr("src") {
            (item.value().name(), Some(src.to_owned()))
        } else {
            (
                "image",
                css_url
                    .captures(item.value().attr("style").unwrap_or(""))
                    .map(|c| c[1].to_owned()),
            )
        };
        if let Some(url) = url
            .and_then(|u| Url::parse(&canonical).ok()?.join(&u).ok())
            .and_then(|u| public_url(u.as_str()).ok())
            && media.len() < max
            && !media.iter().any(|m: &Value| m["url"] == url.as_str())
        {
            media.push(json!({"kind":kind,"url":url.as_str(),"downloaded":false}));
        }
    }
    Some(
        json!({"message_id":identifier,"url":canonical,"channel":handle,"text":content,"text_truncated":full.chars().count()>10000,"timestamp_observed":timestamp,"views_observed":text(node,".tgme_widget_message_views",100),"outbound_links":links(text_node.unwrap_or(node),&canonical,max),"media":media,"source_ids":[source]}),
    )
}
fn graph(p: &mut Payload, messages: &[Value], sources: &[String]) {
    for message in messages {
        p.entity("telegram_post", message["url"].clone(), sources);
        for link in message["outbound_links"].as_array().into_iter().flatten() {
            p.entity("url", link.clone(), sources);
            p.edge(
                "telegram_post",
                message["url"].clone(),
                "links_to",
                "url",
                link.clone(),
                sources,
            );
        }
    }
}
fn html(ctx: &mut Context<'_>, url: &str) -> Result<(Response, Html)> {
    let response = ctx.fetch_headers(url, false, &[("Accept", "text/html")])?;
    response.check()?;
    if !response
        .headers
        .get("content-type")
        .is_some_and(|s| s.starts_with("text/html") || s.starts_with("application/xhtml+xml"))
    {
        return Err(fail(
            "unknown",
            "TELEGRAM_CONTENT",
            "Telegram did not return expected public HTML",
        ));
    }
    let page = Html::parse_document(&response.text());
    Ok((response, page))
}

pub(super) fn collect(
    ctx: &mut Context<'_>,
    command: &str,
    target: &str,
    options: &Value,
) -> Result<Payload> {
    if command == "telegram-export" {
        return export(ctx, target, options);
    }
    let (handle, post, canonical) = normalize(
        target,
        matches!(command, "telegram-post" | "bellingcat-telegram"),
    )?;
    if command == "telegram-profile" {
        let url = format!("https://t.me/{handle}");
        let (response, page) = html(ctx, &url)?;
        let evidence = channel_evidence(&page, &handle);
        let root = page.root_element();
        let sources = vec![response.source_id];
        let mut p = Payload::new(
            json!({"username":handle,"url":url,"title":text(root,".tgme_page_title,.tgme_channel_info_header_title",500),"description":text(root,".tgme_page_description,.tgme_channel_info_description",2000),"audience_observed":text(root,".tgme_page_extra",300),"evidence":evidence.clone().into_iter().collect::<Vec<_>>(),"profile_status":if evidence.is_some(){"verified_public_channel"}else{"unverified"},"source_ids":sources,"visibility":"public_web_preview","identity_attribution":"not_established"}),
        );
        if evidence.is_some() {
            p.entity("telegram_channel", url.clone(), &sources);
            if let Some(description) = root
                .select(&selector(
                    ".tgme_page_description,.tgme_channel_info_description",
                ))
                .next()
            {
                for link in links(description, &url, ctx.cfg.max_results) {
                    p.entity("url", link.clone(), &sources);
                    p.edge(
                        "telegram_channel",
                        url.clone(),
                        "links_to",
                        "url",
                        link,
                        &sources,
                    );
                }
            }
        } else {
            p.status = "unknown".into();
            p.errors.push(json!({"code":"TELEGRAM_PROFILE_UNVERIFIED","message":"Generic contact landing pages do not establish account existence"}));
        }
        return Ok(p);
    }
    let before = options["before"].as_u64().or(post);
    let limit = opt_limit(options, 20, ctx.cfg.max_results.min(100));
    let exact = command != "telegram-messages";
    let url = if exact {
        format!("{canonical}?embed=1")
    } else {
        format!(
            "https://t.me/s/{handle}{}",
            before.map(|b| format!("?before={b}")).unwrap_or_default()
        )
    };
    let (response, page) = html(ctx, &url)?;
    let sources = vec![response.source_id.clone()];
    let query = options["query"].as_str().map(str::to_lowercase);
    let mut messages = page
        .select(&selector("[data-post]"))
        .filter_map(|n| parse_message(n, &handle, &response.source_id, ctx.cfg.max_results))
        .filter(|m| {
            if exact {
                m["message_id"] == post.unwrap()
            } else {
                before.is_none_or(|b| m["message_id"].as_u64().unwrap() < b)
            }
        })
        .filter(|m| {
            query
                .as_ref()
                .is_none_or(|q| m["text"].as_str().unwrap_or("").to_lowercase().contains(q))
        })
        .collect::<Vec<_>>();
    messages.sort_by_key(|m| std::cmp::Reverse(m["message_id"].as_u64().unwrap()));
    messages.dedup_by_key(|m| m["message_id"].as_u64());
    if exact && messages.is_empty() {
        let missing = page
            .select(&selector(".tgme_widget_message_error"))
            .any(|n| {
                ["post not found", "message not found"].contains(
                    &clean(&n.text().collect::<String>(), 200)
                        .to_lowercase()
                        .as_str(),
                )
            });
        return Err(fail(
            if missing { "not_found" } else { "unknown" },
            "TELEGRAM_POST_UNVERIFIED",
            "Public embed did not identify the exact requested channel/message",
        ));
    }
    if !exact && messages.is_empty() && channel_evidence(&page, &handle).is_none() {
        return Err(fail(
            "unknown",
            "TELEGRAM_PREVIEW_UNVERIFIED",
            "Public preview did not match the requested channel",
        ));
    }
    let count = messages.len();
    messages.truncate(if exact { 1 } else { limit });
    let mut p = if exact {
        Payload::new(
            json!({"url":canonical,"message":messages[0],"visibility":"public_web_embed","source_ids":sources}),
        )
    } else {
        Payload::new(
            json!({"username":handle,"url":url,"messages":messages,"returned":messages.len(),"matching_in_page":count,"limit":limit,"before":before,"query":options["query"],"query_scope":"returned_preview_page_only","truncated":count>limit,"history_complete":false,"source_ids":sources,"scope":"One public preview page; no account access, group join, or full-history collection"}),
        )
    };
    graph(&mut p, &messages, &sources);
    if !exact && messages.is_empty() {
        p.status = "not_found".into();
    }
    if command == "bellingcat-telegram" {
        p.data["integration"] = json!({"id":"bellingcat_auto_archiver","upstream":"https://github.com/bellingcat/auto-archiver","license":"MIT","implementation":"Native Rust port of public Telegram post extraction","scope":"One exact public embed; media only when explicitly requested"});
        let mut media = messages[0]["media"].as_array().cloned().unwrap_or_default();
        if options["download_media"] == true {
            let max = options["max_media"].as_u64().unwrap_or(3) as usize;
            for record in media.iter_mut().take(max) {
                let url = record["url"].as_str().unwrap_or("").to_owned();
                match ctx.fetch(&url, true) {
                    Ok(response) => {
                        if let Err(e) = response.check() {
                            p.errors.push(error_value(&e));
                            p.status = "partial".into();
                            continue;
                        }
                        record["downloaded"] = true.into();
                        record["sha256"] = digest(&response.body).into();
                        record["bytes"] = response.body.len().into();
                        record["source_id"] = response.source_id.into();
                    }
                    Err(e) => {
                        p.errors.push(error_value(&e));
                        p.status = "partial".into();
                    }
                }
            }
        }
        p.data["message"]["media"] = json!(media);
    }
    Ok(p)
}

fn export_text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        s.into()
    } else {
        value
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().or(v["text"].as_str()).unwrap_or(""))
                    .collect::<String>()
            })
            .unwrap_or_default()
    }
}
fn export(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let (metadata, body) = local::read_file(ctx, target)?;
    let content = std::str::from_utf8(&body)?.trim_start_matches('\u{feff}');
    let raw: Value = serde_json::from_str(content)?;
    let chats = if raw["messages"].is_array() {
        vec![&raw]
    } else {
        raw.pointer("/chats/list")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect::<Vec<_>>())
            .ok_or_else(|| {
                fail(
                    "invalid",
                    "INVALID_TELEGRAM_EXPORT",
                    "Expected messages[] or chats.list[].messages[]",
                )
            })?
    };
    if chats.len() > 1000 {
        return Err(fail(
            "invalid",
            "TELEGRAM_EXPORT_LIMIT",
            "Export exceeds 1000 chats",
        ));
    }
    let limit = opt_limit(options, 50, ctx.cfg.max_results.min(500));
    let query = options["query"].as_str().map(str::to_lowercase);
    let mut messages = vec![];
    let (mut scanned, mut matched, mut invalid) = (0, 0, 0);
    for (index, chat) in chats.iter().enumerate() {
        let rows = chat["messages"].as_array().ok_or_else(|| {
            fail(
                "invalid",
                "INVALID_TELEGRAM_EXPORT",
                "Export chat lacks messages array",
            )
        })?;
        for row in rows {
            scanned += 1;
            if row["id"]
                .as_u64()
                .is_none_or(|n| n == 0 || n > i64::MAX as u64)
            {
                invalid += 1;
                continue;
            }
            if row["type"] != "message" {
                continue;
            }
            let text = export_text(&row["text"]);
            if query
                .as_ref()
                .is_some_and(|q| !text.to_lowercase().contains(q))
            {
                continue;
            }
            matched += 1;
            if messages.len() >= limit {
                continue;
            }
            messages.push(json!({"message_id":row["id"],"chat_index":index,"chat_id_observed":chat["id"],"chat_name_observed":chat["name"],"chat_type_observed":chat["type"],"text":text.chars().take(10000).collect::<String>(),"text_truncated":text.chars().count()>10000,"timestamp_observed":row["date"],"sender_label_observed":row["from"],"reply_to_message_id":row["reply_to_message_id"],"media_present":row.get("photo").is_some()||row.get("file").is_some(),"source_ids":[metadata["source_id"]]}));
        }
    }
    let sources = vec![metadata["source_id"].as_str().unwrap().to_owned()];
    let mut p = Payload::new(
        json!({"file":metadata,"messages":messages,"returned":messages.len(),"messages_scanned":scanned,"matching_messages":matched,"chat_count":chats.len(),"limit":limit,"query":options["query"],"truncated":matched>limit,"invalid_records":invalid,"network_requests":0,"scope":"Local export observations; no remote verification, uploads, or identity inference"}),
    );
    p.entity("file_sha256", metadata["sha256"].clone(), &sources);
    if invalid > 0 {
        p.status = "partial".into();
        p.errors.push(json!({"code":"TELEGRAM_EXPORT_RECORDS","message":format!("Skipped {invalid} malformed records")}));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_contact_echo_is_not_profile_evidence() {
        let page = Html::parse_document(
            "<div class='tgme_page_title'>alice</div><a href='tg://resolve?domain=alice'>Send message</a>",
        );
        assert!(channel_evidence(&page, "alice").is_none());
    }
    #[test]
    fn only_exact_channel_messages_are_accepted() {
        let page = Html::parse_document(
            "<div data-post='alice/42'><div class='tgme_widget_message_text'>Report <a href='https://example.org/'>Source</a></div><time datetime='2026-11-11T10:00:00Z'></time></div>",
        );
        let node = page.select(&selector("[data-post]")).next().unwrap();
        assert!(parse_message(node, "other", "src_1", 10).is_none());
        let m = parse_message(node, "alice", "src_1", 10).unwrap();
        assert_eq!(m["message_id"], 42);
        assert_eq!(m["outbound_links"][0], "https://example.org/");
        assert_eq!(m["source_ids"][0], "src_1");
    }
    #[test]
    fn excludes_invite_and_non_public_operations() {
        for target in [
            "https://t.me/+invite",
            "https://t.me/joinchat/test",
            "https://t.me/share/url",
            "http://t.me/alice/42",
        ] {
            assert!(normalize(target, false).is_err());
        }
        assert_eq!(
            normalize("https://t.me/s/alice/42", true).unwrap().2,
            "https://t.me/alice/42"
        );
    }
}
