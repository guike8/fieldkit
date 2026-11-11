use super::*;
use regex::Regex;
use scraper::{Html, Selector};

const SITES: &[&str] = &[
    "github",
    "gitlab",
    "codeberg",
    "devto",
    "lichess",
    "chess",
    "huggingface",
    "hackernews",
    "reddit",
    "bluesky",
    "mastodon",
    "steam",
    "youtube",
];
const CANDIDATES: &[(&str, &str)] = &[
    ("instagram", "https://www.instagram.com/{u}/"),
    ("x", "https://x.com/{u}"),
    ("tiktok", "https://www.tiktok.com/@{u}"),
    ("threads", "https://www.threads.net/@{u}"),
    ("pinterest", "https://www.pinterest.com/{u}/"),
    ("facebook", "https://www.facebook.com/{u}"),
    ("twitch", "https://www.twitch.tv/{u}"),
    ("medium", "https://medium.com/@{u}"),
    ("linkedin", "https://www.linkedin.com/in/{u}/"),
    ("telegram", "https://t.me/{u}"),
    ("keybase", "https://keybase.io/{u}"),
];

pub(super) fn catalog() -> Value {
    json!({"verified_adapters":SITES.iter().map(|s|json!({"id":s,"engine":"rust","verification":"identity_schema_and_negative_control"})).collect::<Vec<_>>(),"candidate_only_platforms":CANDIDATES.iter().map(|(s,t)|json!({"id":s,"url_template":t,"status":"unverified"})).collect::<Vec<_>>(),"identity_attribution":"Shared handles do not establish a shared owner"})
}
fn matches(pattern: &str, value: &str) -> bool {
    Regex::new(&format!("^(?:{pattern})$"))
        .unwrap()
        .is_match(value)
}
fn normalize(site: &str, handle: &str) -> Result<(String, String, String)> {
    let mut handle = handle.trim_start_matches('@').to_owned();
    if site == "mastodon" {
        let (user, instance) = handle.split_once('@').ok_or_else(|| {
            fail(
                "invalid",
                "MASTODON_INSTANCE_REQUIRED",
                "Mastodon requires @user@instance.domain",
            )
        })?;
        let instance = domain_name(instance)?;
        if !matches("[A-Za-z0-9_]{1,64}", user) {
            bail!("Invalid Mastodon user");
        }
        return Ok((
            format!("{user}@{instance}"),
            format!("https://{instance}/@{user}"),
            format!("https://{instance}/api/v1/accounts/lookup?acct={user}"),
        ));
    }
    if site == "bluesky" && !handle.contains('.') {
        handle.push_str(".bsky.social");
    }
    let (pattern, profile, endpoint) = match site {
        "github" => (
            "[A-Za-z0-9][A-Za-z0-9-]{0,38}",
            format!("https://github.com/{handle}"),
            format!("https://api.github.com/users/{handle}"),
        ),
        "gitlab" => (
            "[A-Za-z0-9_][A-Za-z0-9_.-]{0,254}",
            format!("https://gitlab.com/{handle}"),
            format!("https://gitlab.com/api/v4/users?username={handle}"),
        ),
        "codeberg" => (
            "[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}",
            format!("https://codeberg.org/{handle}"),
            format!("https://codeberg.org/api/v1/users/{handle}"),
        ),
        "devto" => (
            "[A-Za-z0-9_]{1,30}",
            format!("https://dev.to/{handle}"),
            format!("https://dev.to/api/users/by_username?url={handle}"),
        ),
        "lichess" => (
            "[A-Za-z0-9_-]{2,30}",
            format!("https://lichess.org/@/{handle}"),
            format!("https://lichess.org/api/user/{handle}"),
        ),
        "chess" => (
            "[A-Za-z0-9_-]{3,25}",
            format!("https://www.chess.com/member/{handle}"),
            format!("https://api.chess.com/pub/player/{handle}"),
        ),
        "huggingface" => (
            "[A-Za-z0-9][A-Za-z0-9_-]{0,41}",
            format!("https://huggingface.co/{handle}"),
            format!("https://huggingface.co/api/users/{handle}/overview"),
        ),
        "hackernews" => (
            "[A-Za-z0-9_-]{1,20}",
            format!("https://news.ycombinator.com/user?id={handle}"),
            format!("https://hacker-news.firebaseio.com/v0/user/{handle}.json"),
        ),
        "reddit" => (
            "[A-Za-z0-9_-]{3,20}",
            format!("https://www.reddit.com/user/{handle}/"),
            format!("https://www.reddit.com/user/{handle}/about.json?raw_json=1"),
        ),
        "bluesky" => {
            domain_name(&handle)?;
            (
                "[a-zA-Z0-9.-]{3,253}",
                format!("https://bsky.app/profile/{handle}"),
                format!(
                    "https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={handle}"
                ),
            )
        }
        "steam" => (
            "[A-Za-z0-9_-]{2,32}",
            format!("https://steamcommunity.com/id/{handle}/"),
            format!("https://steamcommunity.com/id/{handle}/"),
        ),
        "youtube" => (
            "[A-Za-z0-9][A-Za-z0-9_.-]{1,28}[A-Za-z0-9]",
            format!("https://www.youtube.com/@{handle}"),
            format!("https://www.youtube.com/@{handle}/about?hl=en"),
        ),
        _ => {
            return Err(fail(
                "invalid",
                "UNKNOWN_SITE",
                "Unsupported profile adapter",
            ));
        }
    };
    if !matches(pattern, &handle) {
        return Err(fail(
            "invalid",
            "INVALID_HANDLE",
            format!("Handle does not match {site}'s public lookup format"),
        ));
    }
    Ok((handle, profile, endpoint))
}
fn record(site: &str, handle: &str, status: &str, reason: &str) -> Value {
    json!({"site":site,"username":handle,"status":status,"reason":reason,"evidence":[],"source_ids":[],"control":"not_run","checked_at":now()})
}
fn set(profile: &mut Value, status: &str, reason: &str) {
    profile["status"] = status.into();
    profile["reason"] = reason.into();
}
fn positive(value: &Value) -> bool {
    value.as_u64().is_some_and(|n| n > 0)
}
fn same(site: &str, actual: &str, expected: &str) -> bool {
    if site == "hackernews" {
        actual == expected
    } else {
        actual.eq_ignore_ascii_case(expected)
    }
}
fn plain_links(content: &str, max: usize) -> Vec<String> {
    Regex::new(r#"https?://[^\s<>"'()]+"#)
        .unwrap()
        .find_iter(content)
        .filter_map(|m| {
            public_url(m.as_str().trim_end_matches(['.', ',', ';', ':', '!']))
                .ok()
                .map(|u| u.to_string())
        })
        .take(max)
        .collect()
}

fn inspect(ctx: &mut Context<'_>, site: &str, target: &str) -> Value {
    let (handle, profile_url, endpoint) = match normalize(site, target) {
        Ok(x) => x,
        Err(e) => return record(site, target, "invalid", &e.to_string()),
    };
    let mut profile = record(
        site,
        &handle,
        "unknown",
        "Expected public profile schema was absent or changed",
    );
    profile["profile_url"] = profile_url.into();
    profile["request_url"] = endpoint.clone().into();
    let response =
        match ctx.fetch_headers(&endpoint, false, &[("Accept-Language", "en-US,en;q=0.9")]) {
            Ok(r) => r,
            Err(e) => {
                set(&mut profile, error_status(&e), &e.to_string());
                profile["error"] = error_value(&e);
                return profile;
            }
        };
    profile["source_ids"] = json!([response.source_id]);
    profile["http_status"] = response.status.into();
    classify(site, &handle, &response, &mut profile, ctx.cfg.max_results);
    profile
}

fn classify(site: &str, handle: &str, response: &Response, profile: &mut Value, max: usize) {
    if matches!(response.status, 401 | 403 | 429)
        || response.status >= 500
        || matches!(response.status, 300..=399)
    {
        let e = response.check().unwrap_err();
        set(profile, error_status(&e), &e.to_string());
        return;
    }
    if matches!(site, "steam" | "youtube") {
        classify_html(site, handle, response, profile, max);
        return;
    }
    let media = response
        .headers
        .get("content-type")
        .map(|s| s.split(';').next().unwrap_or("").trim())
        .unwrap_or("");
    if media != "application/json" && !media.ends_with("+json") {
        return;
    }
    let Ok(raw) = response.json() else {
        return;
    };
    let message = raw["message"].as_str().unwrap_or("").to_ascii_lowercase();
    let error = raw["error"].as_str().unwrap_or("").to_ascii_lowercase();
    let missing = match site {
        "gitlab" => response.status == 200 && raw.as_array().is_some_and(Vec::is_empty),
        "hackernews" => response.status == 200 && raw.is_null(),
        "github" => response.status == 404 && message == "not found",
        "codeberg" => {
            response.status == 404
                && raw["url"] == "https://codeberg.org/api/swagger"
                && (message
                    == format!(
                        "user redirect does not exist [name: {}]",
                        handle.to_lowercase()
                    )
                    || message == format!("user does not exist [name: {}]", handle.to_lowercase()))
        }
        "devto" => {
            response.status == 404
                && raw["status"] == 404
                && ["not found", "user not found"].contains(&error.as_str())
        }
        "lichess" => {
            response.status == 404
                && ["not found", "not found.", "user not found"].contains(&error.as_str())
        }
        "chess" => {
            response.status == 404
                && raw["code"].is_number()
                && (["user not found", "user not found."].contains(&message.as_str())
                    || message.trim_end_matches('.')
                        == format!("user \"{}\" not found", handle.to_lowercase()))
        }
        "huggingface" => {
            response.status == 404
                && [
                    "user not found",
                    "user not found.",
                    "this user does not exist",
                    "this user does not exist.",
                ]
                .contains(&error.as_str())
        }
        "reddit" => response.status == 404 && raw["error"] == 404 && raw["message"] == "Not Found",
        "bluesky" => {
            matches!(response.status, 400 | 404)
                && ["InvalidRequest", "NotFound", "ProfileNotFound"]
                    .contains(&raw["error"].as_str().unwrap_or(""))
                && raw["message"] == "Profile not found"
        }
        "mastodon" => response.status == 404 && raw["error"] == "Record not found",
        _ => false,
    };
    if missing {
        set(
            profile,
            "not_found",
            "Provider returned its explicit missing-public-profile schema; visibility scope only",
        );
        return;
    }
    if site == "bluesky"
        && ["AccountTakedown", "AccountDeactivated"].contains(&raw["error"].as_str().unwrap_or(""))
    {
        set(profile, "blocked", "Account was taken down or deactivated");
        return;
    }
    if response.status != 200 {
        return;
    }
    let body = if site == "gitlab" {
        let Some(rows) = raw.as_array().filter(|r| r.len() == 1) else {
            return;
        };
        &rows[0]
    } else if site == "reddit" {
        if raw["kind"] != "t2" {
            return;
        }
        &raw["data"]
    } else {
        &raw
    };
    if !body.is_object() {
        return;
    }
    if site == "reddit" && body["is_suspended"] == true {
        set(
            profile,
            "blocked",
            "Reddit indicates a suspended account; public identity could not be verified",
        );
        return;
    }
    let identity_key = match site {
        "github" | "codeberg" => "login",
        "huggingface" => "user",
        "hackernews" => "id",
        "reddit" => "name",
        "bluesky" => "handle",
        _ => "username",
    };
    let Some(actual) = body[identity_key].as_str() else {
        return;
    };
    let expected = if site == "mastodon" {
        handle.split('@').next().unwrap_or(handle)
    } else {
        handle
    };
    if !same(site, actual, expected) {
        set(profile, "unknown", "Provider returned a different identity");
        return;
    }
    let stable = match site {
        "github" => {
            positive(&body["id"])
                && ["User", "Organization", "Bot"].contains(&body["type"].as_str().unwrap_or(""))
        }
        "gitlab" | "codeberg" => positive(&body["id"]),
        "devto" => positive(&body["id"]) && body["type_of"] == "user",
        "lichess" => {
            body["id"].as_str() == Some(&handle.to_lowercase()) && positive(&body["createdAt"])
        }
        "chess" => positive(&body["player_id"]),
        "huggingface" => {
            matches("[A-Fa-f0-9]{24}", body["_id"].as_str().unwrap_or("")) && body["type"] == "user"
        }
        "hackernews" => positive(&body["created"]) && body["karma"].is_i64(),
        "reddit" => {
            matches("[a-z0-9]{1,20}", body["id"].as_str().unwrap_or(""))
                && body["is_suspended"] != true
        }
        "bluesky" => matches(
            "did:[a-z0-9]+:[A-Za-z0-9._:%-]{3,512}",
            body["did"].as_str().unwrap_or(""),
        ),
        "mastodon" => {
            matches("[0-9]{1,30}", body["id"].as_str().unwrap_or(""))
                && body["id"]
                    .as_str()
                    .is_some_and(|s| s.parse::<u128>().is_ok_and(|n| n > 0))
                && body["acct"]
                    .as_str()
                    .is_some_and(|a| same(site, a, expected) || same(site, a, handle))
                && body["url"]
                    .as_str()
                    .and_then(|u| public_url(u).ok())
                    .is_some_and(|u| {
                        u.as_str()
                            .trim_end_matches('/')
                            .eq_ignore_ascii_case(profile["profile_url"].as_str().unwrap_or(""))
                    })
        }
        _ => false,
    };
    if !stable {
        return;
    }
    let stable_fields: &[&str] = match site {
        "github" => &["id", "type"],
        "gitlab" | "codeberg" | "reddit" => &["id"],
        "devto" => &["id", "type_of"],
        "lichess" => &["id", "createdAt"],
        "chess" => &["player_id"],
        "huggingface" => &["_id", "type"],
        "hackernews" => &["created", "karma"],
        "bluesky" => &["did"],
        "mastodon" => &["id", "acct", "url"],
        _ => &[],
    };
    let mut evidence = vec![json!({"field":identity_key,"expected":expected,"actual":actual})];
    evidence.extend(
        stable_fields
            .iter()
            .map(|key| json!({"field":key,"actual":body[*key],"validated":true})),
    );
    profile["evidence"] = json!(evidence);
    let bio = match site {
        "github" | "codeberg" | "gitlab" => body["bio"].as_str(),
        "reddit" => body
            .pointer("/subreddit/public_description")
            .and_then(Value::as_str),
        "bluesky" => body["description"].as_str(),
        "mastodon" => body["note"].as_str(),
        "devto" => body["summary"].as_str(),
        "hackernews" => body["about"].as_str(),
        _ => None,
    }
    .unwrap_or("");
    let mut links = plain_links(bio, max);
    for key in ["blog", "website_url", "website", "web_url"] {
        if let Some(link) = body[key].as_str().and_then(|u| public_url(u).ok())
            && links.len() < max
            && !links.contains(&link.to_string())
        {
            links.push(link.to_string());
        }
    }
    if site == "mastodon" {
        let metadata = web::page_metadata(bio, profile["profile_url"].as_str().unwrap_or(""), max);
        for link in metadata["links"].as_array().into_iter().flatten() {
            if let Some(link) = link["url"].as_str()
                && !links.contains(&link.to_owned())
                && links.len() < max
            {
                links.push(link.to_owned());
            }
        }
    }
    profile["public_metadata"] = json!({"display_name":body["name"].as_str().or(body["displayName"].as_str()).or(body["display_name"].as_str()),"bio":clean(&Html::parse_fragment(bio).root_element().text().collect::<String>(),2000),"outbound_links":links});
    set(
        profile,
        "found",
        "Public API matched the requested identity and validated a stable account schema",
    );
}

fn embedded(script: &str, name: &str) -> Vec<Value> {
    let re = Regex::new(&format!(
        r"(?:^|[;\r\n])\s*(?:var\s+)?{}\s*=\s*",
        regex::escape(name)
    ))
    .unwrap();
    re.find_iter(script)
        .filter_map(|m| {
            serde_json::Deserializer::from_str(&script[m.end()..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
        })
        .collect()
}
fn classify_html(site: &str, handle: &str, response: &Response, profile: &mut Value, max: usize) {
    let media = response
        .headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("");
    if !media.starts_with("text/html") && !media.starts_with("application/xhtml+xml") {
        return;
    }
    let page = Html::parse_document(&response.text());
    let scripts = page
        .select(&Selector::parse("script").unwrap())
        .flat_map(|n| {
            embedded(
                &n.text().collect::<String>(),
                if site == "steam" {
                    "g_rgProfileData"
                } else {
                    "ytInitialData"
                },
            )
        })
        .collect::<Vec<_>>();
    if scripts.is_empty() {
        if site == "steam"
            && page
                .select(&Selector::parse(".error_ctn h3").unwrap())
                .any(|n| {
                    clean(&n.text().collect::<String>(), 200)
                        == "The specified profile could not be found."
                })
        {
            set(
                profile,
                "not_found",
                "Steam returned its structured missing-public-profile page",
            );
        }
        let missing_title = page
            .select(&Selector::parse("title").unwrap())
            .any(|n| clean(&n.text().collect::<String>(), 100) == "404 Not Found");
        let missing_frame = page
            .select(&Selector::parse("iframe[src]").unwrap())
            .any(|n| {
                n.value()
                    .attr("src")
                    .and_then(|s| Url::parse("https://www.youtube.com/").ok()?.join(s).ok())
                    .is_some_and(|u| {
                        u.scheme() == "https"
                            && u.host_str() == Some("www.youtube.com")
                            && u.path() == "/error"
                            && u.query_pairs()
                                .filter(|(k, v)| k == "src" && v == "404")
                                .count()
                                == 1
                            && u.query_pairs()
                                .filter(|(k, v)| k == "ifr" && v == "1")
                                .count()
                                == 1
                    })
            });
        let missing_icon = page
            .select(&Selector::parse("link[rel~='icon'][href]").unwrap())
            .any(|n| {
                n.value()
                    .attr("href")
                    .and_then(|s| Url::parse(s).ok())
                    .is_some_and(|u| {
                        u.scheme() == "https"
                            && u.host_str() == Some("www.youtube.com")
                            && matches(r"/img/favicon(?:_[0-9]+)?\.(?:ico|png)", u.path())
                    })
            });
        if site == "youtube"
            && response.status == 404
            && missing_title
            && missing_frame
            && missing_icon
        {
            set(
                profile,
                "not_found",
                "YouTube returned its branded missing-handle page",
            );
        }
        return;
    }
    if scripts.len() != 1 || response.status != 200 {
        return;
    }
    let (valid, stable, bio, name) = if site == "steam" {
        let body = &scripts[0];
        let stable = body["steamid"].as_str().unwrap_or("");
        let valid = matches("[0-9]{17}", stable)
            && stable
                .parse::<u64>()
                .is_ok_and(|s| (76561197960265728..=76561202255233023).contains(&s))
            && body["url"]
                .as_str()
                .and_then(|s| Url::parse(s).ok())
                .is_some_and(|u| {
                    u.host_str() == Some("steamcommunity.com")
                        && u.path()
                            .trim_end_matches('/')
                            .eq_ignore_ascii_case(&format!("/id/{handle}"))
                });
        (
            valid,
            stable,
            "",
            body["personaname"].as_str().unwrap_or(""),
        )
    } else {
        let body = &scripts[0]["metadata"]["channelMetadataRenderer"];
        let stable = body["externalId"].as_str().unwrap_or("");
        let mut owners = body["ownerUrls"].as_array().cloned().unwrap_or_default();
        if body["vanityChannelUrl"].is_string() {
            owners.push(body["vanityChannelUrl"].clone());
        }
        let valid = matches("UC[A-Za-z0-9_-]{22}", stable)
            && owners
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|s| Url::parse(s).ok())
                .any(|u| {
                    ["www.youtube.com", "youtube.com", "m.youtube.com"]
                        .contains(&u.host_str().unwrap_or(""))
                        && u.path()
                            .trim_end_matches('/')
                            .eq_ignore_ascii_case(&format!("/@{handle}"))
                });
        (
            valid,
            stable,
            body["description"].as_str().unwrap_or(""),
            body["title"].as_str().unwrap_or(""),
        )
    };
    if valid {
        profile["evidence"] =
            json!([{"field":"embedded_profile_identity","expected":handle,"stable_id":stable}]);
        profile["public_metadata"] = json!({"display_name":clean(name,300),"bio":clean(bio,2000),"outbound_links":plain_links(bio,max)});
        set(
            profile,
            "found",
            "Public embedded profile JSON matched the exact requested handle and stable account ID",
        );
    }
}

pub(super) fn username(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let handle = target.trim_start_matches('@');
    if target.len() > 255
        || target
            .chars()
            .any(|c| !c.is_ascii() || c.is_whitespace() || c.is_control())
        || (!handle.contains('@') && !matches("[A-Za-z0-9_.-]{1,64}", handle))
    {
        return Err(fail(
            "invalid",
            "INVALID_HANDLE",
            "Supply a supported public handle",
        ));
    }
    let selected = options["sites"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            if handle.contains('@') {
                vec!["mastodon".into()]
            } else {
                SITES
                    .iter()
                    .filter(|s| **s != "mastodon")
                    .map(|s| s.to_string())
                    .collect()
            }
        });
    if selected
        .iter()
        .any(|s| !SITES.contains(&s.as_str()) && !CANDIDATES.iter().any(|(c, _)| c == s))
    {
        return Err(fail(
            "invalid",
            "UNKNOWN_SITE",
            "Unknown site; consult capabilities",
        ));
    }
    let mut profiles = vec![];
    for site in &selected {
        if let Some((_, pattern)) = CANDIDATES.iter().find(|(s, _)| s == site) {
            let mut p = record(
                site,
                handle,
                "unverified",
                "Constructed candidate URL; existence and ownership have not been checked",
            );
            p["profile_url"] = pattern.replace("{u}", handle).into();
            profiles.push(p);
            continue;
        }
        let mut profile = inspect(ctx, site, handle);
        if profile["status"] == "found" {
            let mut control = format!("hc{}", &uuid::Uuid::new_v4().simple().to_string()[..14]);
            if site == "mastodon" {
                if let Some((_, instance)) = handle.split_once('@') {
                    control.push('@');
                    control.push_str(instance);
                }
            } else if site == "bluesky" {
                control.push_str(".bsky.social");
            }
            let negative = inspect(ctx, site, &control);
            apply_control(&mut profile, negative);
        }
        profiles.push(profile);
    }
    let found = profiles.iter().filter(|p| p["status"] == "found").count();
    let unresolved = profiles
        .iter()
        .filter(|p| {
            !matches!(
                p["status"].as_str(),
                Some("found" | "not_found" | "invalid")
            )
        })
        .count();
    let candidates = if options["include_candidates"].as_bool().unwrap_or(true)
        && matches("[A-Za-z0-9_.-]{1,64}", handle)
    {
        CANDIDATES.iter().map(|(s,p)|json!({"site":s,"url":p.replace("{u}",handle),"status":"unverified","reason":"Constructed URL only; profile existence and ownership have not been checked"})).collect::<Vec<_>>()
    } else {
        vec![]
    };
    let mut p = Payload::new(
        json!({"handle":handle,"profiles":profiles,"confirmed_count":found,"unresolved_count":unresolved,"checked_sites":selected,"candidate_links":candidates,"identity_attribution":"not_established"}),
    );
    for profile in profiles {
        if profile["status"] != "found" {
            if !matches!(profile["status"].as_str(), Some("not_found" | "invalid")) {
                p.errors.push(json!({"code":"PROFILE_UNRESOLVED","message":profile["reason"],"site":profile["site"],"status":profile["status"]}));
            }
            continue;
        }
        let sources = crate::model::strings(&profile["source_ids"]);
        let url = profile["profile_url"].clone();
        let scoped = format!(
            "{}:{}",
            profile["site"].as_str().unwrap(),
            profile["username"].as_str().unwrap()
        );
        p.entity("profile_url", url.clone(), &sources);
        p.entity("username", scoped.clone(), &sources);
        p.edge(
            "username",
            scoped,
            "public_profile_at",
            "profile_url",
            url.clone(),
            &sources,
        );
        for link in profile["public_metadata"]["outbound_links"]
            .as_array()
            .into_iter()
            .flatten()
        {
            p.entity("url", link.clone(), &sources);
            p.edge(
                "profile_url",
                url.clone(),
                "links_to",
                "url",
                link.clone(),
                &sources,
            );
        }
    }
    if unresolved > 0 {
        p.status = "partial".into();
    } else if found == 0
        && p.data["profiles"]
            .as_array()
            .is_some_and(|v| v.iter().all(|p| p["status"] == "invalid"))
    {
        p.status = "invalid".into();
    }
    Ok(p)
}

fn apply_control(profile: &mut Value, negative: Value) {
    if negative["status"] == "not_found"
        && negative["site"] == profile["site"]
        && negative["username"] != profile["username"]
    {
        profile["control"] = "passed".into();
    } else {
        profile["control"] = "failed".into();
        set(
            profile,
            "unknown",
            &format!(
                "Match withheld: independent random negative control was {}",
                negative["status"]
            ),
        );
    }
    profile["negative_control"] = negative;
}

pub(super) fn profile(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let url = public_url(target)?;
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let segments = url
        .path_segments()
        .map(|s| s.filter(|v| !v.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    let direct = match host {
        "github.com" => Some("github"),
        "gitlab.com" => Some("gitlab"),
        "codeberg.org" => Some("codeberg"),
        "dev.to" => Some("devto"),
        "huggingface.co" => Some("huggingface"),
        _ => None,
    };
    let selected = if let Some(site) = direct.filter(|_| segments.len() == 1) {
        Some((site, segments[0].to_owned()))
    } else {
        match (host, segments.as_slice()) {
            ("reddit.com", ["user" | "u", handle]) => Some(("reddit", handle.to_string())),
            ("bsky.app", ["profile", handle]) => Some(("bluesky", handle.to_string())),
            ("steamcommunity.com", ["id", handle]) => Some(("steam", handle.to_string())),
            ("lichess.org", ["@", handle]) => Some(("lichess", handle.to_string())),
            ("chess.com", ["member", handle]) => Some(("chess", handle.to_string())),
            ("youtube.com", [handle]) if handle.starts_with('@') => {
                Some(("youtube", handle.trim_start_matches('@').to_string()))
            }
            ("news.ycombinator.com", ["user"]) => url
                .query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| ("hackernews", v.into_owned())),
            (instance, [handle])
                if handle.starts_with('@')
                    && !matches!(instance, "instagram.com" | "tiktok.com" | "threads.net") =>
            {
                Some(("mastodon", format!("{handle}@{instance}")))
            }
            _ => None,
        }
    };
    if let Some((site, handle)) = selected {
        return username(
            ctx,
            &handle,
            &json!({"sites":[site],"include_candidates":false}),
        );
    }
    let mut p = web::http(ctx, target)?;
    p.data["identity_status"] = "unverified".into();
    p.data["interpretation"] =
        "Page metadata is URL evidence; account ownership is not established".into();
    Ok(p)
}

pub(super) fn person(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    if target.trim().is_empty() || target.len() > 120 || target.chars().any(char::is_control) {
        return Err(fail(
            "invalid",
            "INVALID_NAME",
            "Supply a public name of 1–120 printable characters",
        ));
    }
    let context = options["context"].as_str().unwrap_or("");
    let base = format!("\"{}\" {}", target.replace('"', " "), context)
        .trim()
        .to_owned();
    let queries = vec![
        base.clone(),
        format!("{base} (site:linkedin.com/in OR site:github.com)"),
        format!("{base} (site:instagram.com OR site:x.com OR site:bsky.app)"),
        format!("{base} (site:youtube.com OR site:tiktok.com)"),
    ];
    let mut p = Payload::new(
        json!({"name":target,"context":context,"queries":queries,"hits":[],"identity_attribution":"not_established","search_requested":options["search"].as_bool().unwrap_or(false),"search_executed":false}),
    );
    if options["search"] == true {
        for query in queries {
            match web::search(ctx, &query, &json!({"limit":10})) {
                Ok(found) => {
                    p.data["search_executed"] = true.into();
                    p.data["hits"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"query":query,"result":found.data}));
                    p.entities.extend(found.entities);
                }
                Err(e) => {
                    p.errors.push(error_value(&e));
                    p.status = "partial".into();
                    break;
                }
            }
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(status: u16, value: Value) -> Response {
        Response {
            url: "https://api.github.com/users/alice".into(),
            status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: serde_json::to_vec(&value).unwrap(),
            source_id: "src_fixture".into(),
        }
    }
    #[test]
    fn matching_identity_needs_stable_schema() {
        let mut p = record("github", "alice", "unknown", "");
        classify(
            "github",
            "alice",
            &response(200, json!({"login":"alice"})),
            &mut p,
            10,
        );
        assert_eq!(p["status"], "unknown");
        classify(
            "github",
            "alice",
            &response(200, json!({"login":"alice","id":123,"type":"User"})),
            &mut p,
            10,
        );
        assert_eq!(p["status"], "found");
    }
    #[test]
    fn generic_404_never_proves_absence() {
        let mut p = record("github", "alice", "unknown", "");
        classify(
            "github",
            "alice",
            &response(404, json!({"error":"no"})),
            &mut p,
            10,
        );
        assert_eq!(p["status"], "unknown");
        classify(
            "github",
            "alice",
            &response(404, json!({"message":"Not Found"})),
            &mut p,
            10,
        );
        assert_eq!(p["status"], "not_found");
    }
    #[test]
    fn wrong_identity_never_qualifies() {
        let mut p = record("github", "alice", "unknown", "");
        classify(
            "github",
            "alice",
            &response(200, json!({"login":"bob","id":123,"type":"User"})),
            &mut p,
            10,
        );
        assert_eq!(p["status"], "unknown");
    }
    #[test]
    fn qualified_inputs_preserve_instance() {
        assert!(normalize("mastodon", "alice").is_err());
        assert_eq!(
            normalize("mastodon", "@alice@mastodon.social").unwrap().0,
            "alice@mastodon.social"
        );
        assert_eq!(
            normalize("bluesky", "alice").unwrap().0,
            "alice.bsky.social"
        );
    }
    #[test]
    fn embedded_json_is_data_only() {
        assert_eq!(
            embedded(
                "var g_rgProfileData = {\"url\":\"x\",\"steamid\":\"1\"}; alert('no');",
                "g_rgProfileData"
            )
            .len(),
            1
        );
    }
    #[test]
    fn a_failed_negative_control_withholds_a_matching_profile() {
        let mut p = record("github", "alice", "found", "");
        apply_control(&mut p, record("github", "hcrandom", "blocked", "Denied"));
        assert_eq!(p["status"], "unknown");
        assert_eq!(p["control"], "failed");
        apply_control(
            &mut p,
            record("github", "hcrandom2", "not_found", "Missing"),
        );
        assert_eq!(p["control"], "passed");
    }
    #[test]
    fn json_under_html_content_type_cannot_establish_account() {
        let mut p = record("github", "alice", "unknown", "");
        let mut r = response(200, json!({"login":"alice","id":123,"type":"User"}));
        r.headers.insert("content-type".into(), "text/html".into());
        classify("github", "alice", &r, &mut p, 10);
        assert_eq!(p["status"], "unknown");
    }
    #[test]
    fn youtube_branded_404_supports_relative_error_frame() {
        let mut p = record("youtube", "alice", "unknown", "");
        let r=Response{url:"https://www.youtube.com/@alice/about?hl=en".into(),status:404,headers:BTreeMap::from([("content-type".into(),"text/html".into())]),body:br#"<title>404 Not Found</title><link rel="shortcut icon" href="https://www.youtube.com/img/favicon.ico"><iframe src="/error?src=404&amp;ifr=1&amp;error="></iframe>"#.to_vec(),source_id:"src_fixture".into()};
        classify_html("youtube", "alice", &r, &mut p, 10);
        assert_eq!(p["status"], "not_found");
    }
}
