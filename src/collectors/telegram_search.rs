use super::*;
use chrono::{DateTime, Duration, Months, NaiveDate, SecondsFormat, Utc};

const ENDPOINT: &str = "https://api.openmeasures.io/content";
const GUIDE: &str = "https://docs.openmeasures.io/docs/guides/public-api";

struct Search {
    url: Url,
    query_type: String,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    limit: usize,
}

fn date(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|v| v.and_utc())
        })
}

fn date_option(options: &Value, key: &str, default: DateTime<Utc>) -> Result<DateTime<Utc>> {
    match options.get(key) {
        None => Ok(default),
        Some(value) => value.as_str().and_then(date).ok_or_else(|| {
            fail(
                "invalid",
                "INVALID_ARCHIVE_DATE",
                format!("{key} must be YYYY-MM-DD or an RFC3339 timestamp"),
            )
        }),
    }
}

fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

impl Search {
    fn new(target: &str, options: &Value, maximum: usize, now: DateTime<Utc>) -> Result<Self> {
        if target.trim().is_empty() || target.len() > 2048 || target.chars().any(char::is_control) {
            return Err(fail(
                "invalid",
                "INVALID_QUERY",
                "Telegram archive query must contain 1–2048 UTF-8 bytes without control characters",
            ));
        }
        let query_type = options["query_type"].as_str().unwrap_or("content");
        if !["content", "boolean_content"].contains(&query_type) {
            return Err(fail(
                "invalid",
                "INVALID_QUERY_TYPE",
                "Use content or boolean_content",
            ));
        }
        let cutoff = now
            .checked_sub_months(Months::new(6))
            .context("Invalid archive cutoff")?;
        let since = date_option(
            options,
            "since",
            now.checked_sub_months(Months::new(12))
                .context("Invalid archive start")?,
        )?;
        // Leave a day of margin for the public service's historical-data boundary.
        let until = date_option(options, "until", cutoff - Duration::days(1))?;
        if since >= until || until > cutoff {
            return Err(fail(
                "invalid",
                "INVALID_ARCHIVE_WINDOW",
                "since must precede until; the public archive only exposes data at least six months old",
            ));
        }
        let limit = opt_limit(options, 20, maximum.min(100));
        if limit == 0 {
            return Err(fail(
                "invalid",
                "INVALID_LIMIT",
                "Allow at least one archive result",
            ));
        }
        let mut url = Url::parse(ENDPOINT)?;
        url.query_pairs_mut()
            .append_pair("site", "telegram")
            .append_pair("term", target)
            .append_pair("querytype", query_type)
            .append_pair("since", &stamp(since))
            .append_pair("until", &stamp(until))
            .append_pair("sortdesc", "true")
            .append_pair("limit", &limit.to_string());
        Ok(Self {
            url,
            query_type: query_type.into(),
            since,
            until,
            limit,
        })
    }

    fn payload(&self, target: &str, source_id: &str) -> Payload {
        Payload::new(json!({
            "query":target,"query_type":self.query_type,"provider":"Open Measures",
            "since":stamp(self.since),"until":stamp(self.until),"until_exclusive":true,
            "messages":[],"returned":0,"limit":self.limit,"history_complete":false,
            "directly_verified":false,"identity_attribution":"not_established",
            "coverage":{"kind":"third_party_historical_index","live":false,
                "minimum_age_months":6,"public_requests_per_day":39,
                "daily_quota_enforced_by":"provider","documentation":GUIDE},
            "source_ids":[source_id],
            "scope":"One bounded Open Measures query; archived provider observations, not direct Telegram verification or complete platform coverage"
        }))
    }
}

pub(super) fn validate(target: &str, options: &Value) -> Result<()> {
    Search::new(target, options, 100, Utc::now()).map(|_| ())
}

pub(super) fn collect(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let search = Search::new(target, options, ctx.cfg.max_results, Utc::now())?;
    let response = ctx.fetch_headers(
        search.url.as_str(),
        false,
        &[("Accept", "application/json")],
    )?;
    parse(&search, target, &response)
}

fn observed_time(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .and_then(|n| DateTime::from_timestamp(n, 0))
        .or_else(|| value.as_str().and_then(date))
}

fn message(row: &Value, index: usize, search: &Search, source_id: &str) -> Option<Value> {
    let source = row.get("_source")?.as_object()?;
    let text = source.get("message")?.as_str()?;
    let text = text
        .chars()
        .filter(|c| !c.is_control() || matches!(*c, '\n' | '\r' | '\t'))
        .take(10001)
        .collect::<String>();
    let published = observed_time(source.get("date")?)?;
    if published < search.since || published >= search.until {
        return None;
    }
    let url = source.get("url").and_then(Value::as_str).and_then(|value| {
        let value = if value.starts_with("t.me/") {
            format!("https://{value}")
        } else {
            value.into()
        };
        telegram::normalize(&value, true).ok()
    });
    if let Some((handle, post, _)) = &url
        && (source
            .get("channelusername")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.eq_ignore_ascii_case(handle))
            || source
                .get("id")
                .and_then(Value::as_u64)
                .is_some_and(|id| Some(id) != *post))
    {
        return None;
    }
    Some(json!({
        "provider_record_id":row["_id"].as_str().map(|v| clean(v,200)),
        "url":url.map(|(_,_,url)|url),
        "message_id_observed":source.get("id").and_then(Value::as_u64),
        "channel_username_observed":source.get("channelusername").and_then(Value::as_str).map(|v|clean(v,100)),
        "channel_title_observed":source.get("channeltitle").and_then(Value::as_str).map(|v|clean(v,500)),
        "text":text.chars().take(10000).collect::<String>(),"text_truncated":text.chars().count()>10000,
        "timestamp_observed":stamp(published),
        "provider_archived_at_observed":source.get("archivedon").and_then(observed_time).map(stamp),
        "verification":"provider_archive_observation","directly_verified":false,
        "source_ids":[source_id],"source_pointer":format!("/hits/hits/{index}/_source")
    }))
}

fn parse(search: &Search, target: &str, response: &Response) -> Result<Payload> {
    let mut p = search.payload(target, &response.source_id);
    if let Err(error) = response.check() {
        p.status = error_status(&error).into();
        p.errors.push(error_value(&error));
        p.data["retry_after"] = json!(response.headers.get("retry-after"));
        return Ok(p);
    }
    let raw = response.json()?;
    let rows = raw
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            fail(
                "unknown",
                "TELEGRAM_SEARCH_SCHEMA",
                "Open Measures response lacks hits.hits",
            )
        })?;
    if raw.get("content_key").is_some_and(|v| v != "message")
        || raw.get("created_key").is_some_and(|v| v != "date")
    {
        return Err(fail(
            "unknown",
            "TELEGRAM_SEARCH_SCHEMA",
            "Open Measures Telegram field mapping changed",
        ));
    }
    let messages = rows
        .iter()
        .take(search.limit)
        .enumerate()
        .filter_map(|(index, row)| message(row, index, search, &response.source_id))
        .collect::<Vec<_>>();
    let invalid = rows.len().min(search.limit) - messages.len();
    let total = raw.pointer("/hits/total");
    if raw.get("timed_out").is_some_and(|v| !v.is_boolean())
        || raw
            .pointer("/_shards/failed")
            .is_some_and(|v| v.as_u64().is_none())
        || total.is_some_and(|v| {
            v.as_u64().is_none()
                && (v["value"].as_u64().is_none()
                    || !["eq", "gte"].contains(&v["relation"].as_str().unwrap_or("")))
        })
    {
        return Err(fail(
            "unknown",
            "TELEGRAM_SEARCH_SCHEMA",
            "Open Measures returned invalid search-completion metadata",
        ));
    }
    let total_value = total.and_then(|v| v.as_u64().or_else(|| v["value"].as_u64()));
    let lower_bound = total.is_some_and(|v| v["relation"] == "gte");
    let timed_out = raw["timed_out"] == true;
    let failed_shards = raw
        .pointer("/_shards/failed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    p.data["provider_total"] = json!(total_value);
    p.data["provider_total_is_lower_bound"] = json!(lower_bound);
    p.data["returned"] = json!(messages.len());
    p.data["truncated"] = json!(
        rows.len() > search.limit
            || lower_bound
            || total_value.is_some_and(|n| n > messages.len() as u64)
    );
    p.data["invalid_records"] = json!(invalid);
    p.data["provider_timed_out"] = json!(timed_out);
    p.data["provider_failed_shards"] = json!(failed_shards);
    for record in &messages {
        if let Some(url) = record["url"].as_str() {
            // Index references are leads, never verified Telegram posts or channel ownership.
            p.entity(
                "search_result_url",
                url,
                std::slice::from_ref(&response.source_id),
            );
        }
    }
    p.data["messages"] = json!(messages);
    if invalid > 0 || timed_out || failed_shards > 0 {
        p.status = "partial".into();
        p.errors.push(json!({"code":"TELEGRAM_SEARCH_INCOMPLETE","message":"Provider returned invalid records, a timeout, or failed search shards","retryable":false}));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(options: Value, max: usize) -> Search {
        Search::new(
            "research & reports",
            &options,
            max,
            date("2025-03-31T12:00:00Z").unwrap(),
        )
        .unwrap()
    }
    fn window() -> Value {
        json!({"since":"2024-01-01","until":"2024-02-01","limit":2})
    }
    fn row() -> Value {
        json!({"_id":"fixture-1","_source":{"message":"A public research report","date":1704112089,"archivedon":1709886248,"channelusername":"examplechannel","channeltitle":"Example channel","id":42,"url":"t.me/examplechannel/42"}})
    }
    fn response(ctx: &mut Context<'_>, raw: Value) -> Response {
        let body = serde_json::to_vec(&raw).unwrap();
        let source_id = ctx.source(
            "Open Measures fixture",
            Some(ENDPOINT),
            &body,
            "http",
            json!({"http_status":200}),
        );
        Response {
            url: ENDPOINT.into(),
            status: 200,
            headers: BTreeMap::new(),
            body,
            source_id,
        }
    }
    #[test]
    fn request_is_fixed_bounded_encoded_and_historical() {
        let s = search(window(), 1);
        assert_eq!(s.url.host_str(), Some("api.openmeasures.io"));
        let pairs = s.url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
        assert_eq!(pairs["term"], "research & reports");
        assert_eq!(pairs["site"], "telegram");
        assert_eq!(pairs["querytype"], "content");
        assert_eq!(pairs["limit"], "1");
        assert_eq!(pairs["until"], "2024-02-01T00:00:00Z");
        let defaults = search(json!({}), 100);
        assert_eq!(stamp(defaults.until), "2024-09-29T12:00:00Z");
    }
    #[test]
    fn rejects_bad_queries_dates_and_recent_or_reversed_windows() {
        for options in [
            json!({"since":"bad"}),
            json!({"until":"2025-01-01"}),
            json!({"since":"2024-02-01","until":"2024-01-01"}),
            json!({"query_type":"query_string"}),
        ] {
            assert!(
                Search::new(
                    "research",
                    &options,
                    100,
                    date("2025-03-31T12:00:00Z").unwrap()
                )
                .is_err()
            );
        }
        for query in [" ", "research\nother"] {
            assert!(validate(query, &json!({})).is_err());
        }
    }
    #[test]
    fn retains_archive_provenance_without_promoting_identity_or_direct_observations() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        let raw = json!({"created_key":"date","content_key":"message","hits":{"total":{"value":5,"relation":"eq"},"hits":[row()]}});
        let response = response(&mut ctx, raw.clone());
        let p = parse(&search(window(), 100), "research", &response).unwrap();
        assert_eq!(p.status, "ok");
        assert_eq!(
            p.data["messages"][0]["url"],
            "https://t.me/examplechannel/42"
        );
        assert_eq!(
            p.data["messages"][0]["source_ids"],
            json!([response.source_id])
        );
        assert_eq!(
            p.data["messages"][0]["source_pointer"],
            "/hits/hits/0/_source"
        );
        assert_eq!(p.data["messages"][0]["directly_verified"], false);
        assert_eq!(p.data["history_complete"], false);
        assert_eq!(p.data["truncated"], true);
        assert_eq!(p.entities[0]["type"], "search_result_url");
        assert!(p.relationships.is_empty());
        let bytes = &ctx.blobs[ctx.sources[0]["sha256"].as_str().unwrap()];
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), raw);
    }
    #[test]
    fn handles_empty_results_and_schema_changes_without_claiming_platform_absence() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        let empty = response(
            &mut ctx,
            json!({"hits":{"total":{"value":0,"relation":"eq"},"hits":[]}}),
        );
        let p = parse(&search(window(), 100), "research", &empty).unwrap();
        assert_eq!(p.status, "ok");
        assert_eq!(p.data["returned"], 0);
        assert_eq!(p.data["history_complete"], false);
        for raw in [
            json!({}),
            json!({"content_key":"new_field","hits":{"hits":[]}}),
            json!({"timed_out":"true","hits":{"hits":[]}}),
            json!({"hits":{"total":{"value":0,"relation":"changed"},"hits":[]}}),
        ] {
            assert!(parse(&search(window(), 100), "research", &response(&mut ctx, raw)).is_err());
        }
    }
    #[test]
    fn partial_results_do_not_hide_invalid_records_timeouts_or_shard_failures() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        let mut bad = row();
        bad["_source"]["id"] = json!(99);
        let response = response(
            &mut ctx,
            json!({"timed_out":true,"_shards":{"failed":1},"hits":{"hits":[row(),bad]}}),
        );
        let p = parse(&search(window(), 100), "research", &response).unwrap();
        assert_eq!(p.status, "partial");
        assert_eq!(p.data["returned"], 1);
        assert_eq!(p.data["invalid_records"], 1);
        assert_eq!(p.data["provider_timed_out"], true);
    }
    #[test]
    fn unsafe_urls_never_become_graph_leads_and_mismatched_posts_are_rejected() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        for url in [
            "https://evil.example/examplechannel/42",
            "https://t.me/+invite",
            "https://t.me/c/123/42",
            "http://t.me/examplechannel/42",
            "https://user:pass@t.me/examplechannel/42",
        ] {
            let mut row = row();
            row["_source"]["url"] = json!(url);
            let output = parse(
                &search(window(), 100),
                "research",
                &response(&mut ctx, json!({"hits":{"hits":[row]}})),
            )
            .unwrap();
            assert!(output.entities.is_empty());
            assert!(output.data["messages"][0]["url"].is_null());
        }
        let mut mismatch = row();
        mismatch["_source"]["channelusername"] = json!("otherchannel");
        assert!(message(&mismatch, 0, &search(window(), 100), "src_fixture").is_none());
    }
    #[test]
    fn result_bounds_timestamps_and_text_limits_are_explicit() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        let mut long = row();
        long["_source"]["message"] = json!(format!("\u{001b}{}", "x".repeat(10001)));
        let output = parse(
            &search(window(), 1),
            "research",
            &response(
                &mut ctx,
                json!({"hits":{"total":{"value":2,"relation":"gte"},"hits":[long,row()]}}),
            ),
        )
        .unwrap();
        assert_eq!(output.data["returned"], 1);
        assert_eq!(output.data["provider_total_is_lower_bound"], true);
        assert_eq!(output.data["truncated"], true);
        assert_eq!(
            output.data["messages"][0]["text"].as_str().unwrap().len(),
            10000
        );
        assert_eq!(output.data["messages"][0]["text_truncated"], true);
        for timestamp in [json!("2024-02-01T00:00:00Z"), json!("bad"), json!(null)] {
            let mut row = row();
            row["_source"]["date"] = timestamp;
            assert!(message(&row, 0, &search(window(), 100), "src_fixture").is_none());
        }
        let fractional = search(
            json!({"since":"2024-01-01T00:00:00.500Z","until":"2024-02-01"}),
            100,
        );
        assert!(
            fractional
                .url
                .query_pairs()
                .any(|(key, value)| key == "since" && value == "2024-01-01T00:00:00.500Z")
        );
    }
    #[test]
    fn rate_limits_keep_coverage_and_retry_after_without_retrying() {
        let config = Config::default();
        let mut ctx = Context::new(&config);
        let mut response = response(&mut ctx, json!({"detail":"Rate limited"}));
        response.status = 429;
        response
            .headers
            .insert("retry-after".into(), "86400".into());
        let p = parse(&search(window(), 100), "research", &response).unwrap();
        assert_eq!(p.status, "rate_limited");
        assert_eq!(p.data["coverage"]["public_requests_per_day"], 39);
        assert_eq!(p.data["retry_after"], "86400");
        assert!(p.entities.is_empty());
        assert_eq!(ctx.requests, 0);
    }
}
