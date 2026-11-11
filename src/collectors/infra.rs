use super::*;
use std::net::TcpStream;
use std::sync::Arc;

const RR: &[(&str, u64)] = &[
    ("A", 1),
    ("NS", 2),
    ("CNAME", 5),
    ("SOA", 6),
    ("PTR", 12),
    ("MX", 15),
    ("TXT", 16),
    ("AAAA", 28),
    ("SRV", 33),
    ("CAA", 257),
];
pub(super) fn dns(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let types = options["types"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_uppercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            vec![
                "A".into(),
                "AAAA".into(),
                "MX".into(),
                "NS".into(),
                "TXT".into(),
                "CAA".into(),
            ]
        });
    if types.is_empty() || types.iter().any(|v| !RR.iter().any(|(k, _)| k == v)) {
        return Err(fail(
            "invalid",
            "INVALID_DNS_TYPE",
            "Unsupported DNS record type",
        ));
    }
    let name = if types.iter().any(|v| v == "PTR") {
        match target.parse::<IpAddr>() {
            Ok(ip) => {
                if !public_ip(ip) {
                    return Err(fail(
                        "invalid",
                        "PRIVATE_TARGET",
                        "Reverse DNS requires a public IP",
                    ));
                }
                match ip {
                    IpAddr::V4(ip) => format!(
                        "{}.in-addr.arpa",
                        ip.octets()
                            .iter()
                            .rev()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(".")
                    ),
                    IpAddr::V6(ip) => format!(
                        "{}.ip6.arpa",
                        format!("{:032x}", u128::from(ip))
                            .chars()
                            .rev()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(".")
                    ),
                }
            }
            Err(_) => domain_name(target)?,
        }
    } else {
        domain_name(target)?
    };
    let mut p = Payload::new(
        json!({"name":name,"queries":[],"records":[],"scope":"Public resolver observations at collection time"}),
    );
    let mut records = vec![];
    let mut queries = vec![];
    for kind in types {
        let code = RR.iter().find(|(k, _)| *k == kind).unwrap().1;
        let mut url = Url::parse("https://dns.google/resolve")?;
        url.query_pairs_mut()
            .append_pair("name", &name)
            .append_pair("type", &kind)
            .append_pair("do", "true")
            .append_pair("cd", "false")
            .append_pair("edns_client_subnet", "0.0.0.0/0");
        let result = (|| -> Result<()> {
            let response = ctx.fetch(url.as_str(), false)?;
            response.check()?;
            let body = response.json()?;
            let rcode = body["Status"].as_u64().ok_or_else(|| {
                fail("unknown", "DNS_SCHEMA", "DNS response lacks numeric Status")
            })?;
            if !body["Question"].as_array().is_some_and(|a| {
                a.iter().any(|q| {
                    q["name"]
                        .as_str()
                        .is_some_and(|n| n.trim_end_matches('.').eq_ignore_ascii_case(&name))
                        && q["type"] == code
                })
            }) {
                return Err(fail(
                    "unknown",
                    "DNS_QUESTION",
                    "DNS response does not match query name/type",
                ));
            }
            if rcode != 0 && rcode != 3 {
                return Err(fail(
                    "error",
                    "DNS_RCODE",
                    format!("Resolver returned DNS status {rcode}"),
                ));
            }
            let answers = body["Answer"].as_array().cloned().unwrap_or_default();
            let sources = vec![response.source_id.clone()];
            for row in &answers {
                if records.len() >= ctx.cfg.max_results {
                    break;
                }
                let (Some(owner), Some(value), Some(ttl), Some(typ)) = (
                    row["name"].as_str(),
                    row["data"].as_str(),
                    row["TTL"].as_u64(),
                    row["type"].as_u64(),
                ) else {
                    continue;
                };
                let rrname = RR
                    .iter()
                    .find(|(_, n)| *n == typ)
                    .map(|(n, _)| *n)
                    .unwrap_or("UNKNOWN");
                records.push(json!({"name":owner.trim_end_matches('.'),"type":rrname,"type_code":typ,"ttl":ttl,"value":value,"query_type":kind,"source_id":response.source_id}));
                if matches!(typ, 1 | 28) {
                    if let Ok(ip) = value.parse::<IpAddr>() {
                        p.entity("domain", owner.trim_end_matches('.'), &sources);
                        p.entity("ip", ip.to_string(), &sources);
                        p.edge(
                            "domain",
                            owner.trim_end_matches('.'),
                            "resolves_to",
                            "ip",
                            ip.to_string(),
                            &sources,
                        );
                    }
                } else if matches!(typ, 2 | 5 | 12 | 15)
                    && let Some(dest) = value
                        .split_whitespace()
                        .last()
                        .and_then(|s| domain_name(s).ok())
                {
                    let relation = match typ {
                        2 => "nameserver",
                        5 => "cname_target",
                        12 => "ptr_target",
                        _ => "mail_exchanger",
                    };
                    p.entity("domain", dest.clone(), &sources);
                    p.edge(
                        "domain",
                        owner.trim_end_matches('.'),
                        relation,
                        "domain",
                        dest,
                        &sources,
                    );
                }
            }
            queries.push(json!({"type":kind,"status":if rcode==3{"not_found"}else{"ok"},"rcode":rcode,"answer_count":answers.len(),"dnssec_authenticated":body["AD"]==true,"source_id":response.source_id}));
            Ok(())
        })();
        if let Err(e) = result {
            p.errors.push(error_value(&e));
            queries.push(json!({"type":kind,"status":error_status(&e)}));
        }
    }
    p.data["queries"] = json!(queries);
    p.data["records"] = json!(records);
    if !p.errors.is_empty() {
        p.status = "partial".into();
    } else if queries.iter().all(|q| q["status"] == "not_found") {
        p.status = "not_found".into();
    }
    Ok(p)
}

fn ip_number(ip: IpAddr) -> (u8, u128) {
    match ip {
        IpAddr::V4(a) => (4, u32::from(a) as u128),
        IpAddr::V6(a) => (6, u128::from(a)),
    }
}
fn cidr_contains(cidr: &str, target: IpAddr) -> Option<u8> {
    let (base, prefix) = cidr.split_once('/')?;
    let base = base.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    let (ver, value) = ip_number(target);
    let (bver, bvalue) = ip_number(base);
    let width = if ver == 4 { 32 } else { 128 };
    if ver != bver || prefix > width {
        return None;
    }
    let shift = width - prefix;
    let valid = if shift == 128 {
        true
    } else {
        (value >> shift) == (bvalue >> shift)
    };
    valid.then_some(prefix)
}
fn bootstrap_service(body: &Value, target: &str, kind: &str) -> Result<String> {
    let services = body["services"].as_array().ok_or_else(|| {
        fail(
            "unknown",
            "RDAP_BOOTSTRAP",
            "IANA bootstrap services schema changed",
        )
    })?;
    let mut candidates = vec![];
    for service in services {
        let (Some(keys), Some(urls)) = (service[0].as_array(), service[1].as_array()) else {
            continue;
        };
        for key in keys.iter().filter_map(Value::as_str) {
            let rank = match kind {
                "domain" => (target.rsplit('.').next() == Some(key)).then_some(1),
                "ip" => target
                    .parse::<IpAddr>()
                    .ok()
                    .and_then(|ip| cidr_contains(key, ip))
                    .map(usize::from),
                "asn" => key
                    .split_once('-')
                    .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)))
                    .filter(|(s, e)| target.parse::<u64>().is_ok_and(|n| n >= *s && n <= *e))
                    .map(|_| 1),
                _ => None,
            };
            if let Some(rank) = rank {
                for base in urls
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|u| u.starts_with("https://"))
                {
                    if public_url(base).is_ok() {
                        candidates.push((rank, base.to_owned()));
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates.pop().map(|(_, u)| u).ok_or_else(|| {
        fail(
            "unavailable",
            "RDAP_SERVICE",
            "No authoritative RDAP service in IANA bootstrap",
        )
    })
}
pub(super) fn rdap(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let requested = options["kind"].as_str().unwrap_or("auto");
    let kind = if requested == "auto" {
        if target.parse::<IpAddr>().is_ok() {
            "ip"
        } else if target.to_ascii_uppercase().starts_with("AS")
            || target.chars().all(|c| c.is_ascii_digit())
        {
            "asn"
        } else {
            "domain"
        }
    } else {
        requested
    };
    let normalized = match kind {
        "domain" => domain_name(target)?,
        "ip" => {
            let ip = target.parse::<IpAddr>()?;
            if !public_ip(ip) {
                return Err(fail(
                    "invalid",
                    "PRIVATE_TARGET",
                    "RDAP requires public IP address",
                ));
            }
            ip.to_string()
        }
        "asn" => {
            let n = target
                .trim_start_matches("AS")
                .trim_start_matches("as")
                .parse::<u32>()?;
            if n == 0 {
                bail!("ASN must be positive");
            }
            n.to_string()
        }
        _ => bail!("Invalid RDAP kind"),
    };
    let bootstrap_file = match kind {
        "domain" => "dns",
        "asn" => "asn",
        _ => {
            if normalized.parse::<IpAddr>()?.is_ipv4() {
                "ipv4"
            } else {
                "ipv6"
            }
        }
    };
    let bootstrap = ctx.fetch(
        &format!("https://data.iana.org/rdap/{bootstrap_file}.json"),
        true,
    )?;
    bootstrap.check()?;
    let base = bootstrap_service(&bootstrap.json()?, &normalized, kind)?;
    let endpoint = format!(
        "{}/{}/{}",
        base.trim_end_matches('/'),
        if kind == "asn" { "autnum" } else { kind },
        normalized
    );
    let response = ctx.fetch(&endpoint, true)?;
    let body = response.json()?;
    if response.status == 404 && body["errorCode"] == 404 {
        return Ok(Payload::new(json!({"target":normalized,"kind":kind,"found":false,"source_id":response.source_id,"bootstrap_source_id":bootstrap.source_id})).status("not_found"));
    }
    response.check()?;
    let valid = match kind {
        "domain" => {
            body["objectClassName"] == "domain"
                && body["ldhName"]
                    .as_str()
                    .is_some_and(|s| s.trim_end_matches('.').eq_ignore_ascii_case(&normalized))
        }
        "ip" => {
            body["objectClassName"] == "ip network"
                && body["startAddress"]
                    .as_str()
                    .zip(body["endAddress"].as_str())
                    .is_some_and(|(s, e)| {
                        match (
                            s.parse::<IpAddr>(),
                            e.parse::<IpAddr>(),
                            normalized.parse::<IpAddr>(),
                        ) {
                            (Ok(s), Ok(e), Ok(n)) => {
                                let (v, n) = ip_number(n);
                                let (sv, s) = ip_number(s);
                                let (ev, e) = ip_number(e);
                                v == sv && v == ev && n >= s && n <= e
                            }
                            _ => false,
                        }
                    })
        }
        "asn" => {
            body["objectClassName"] == "autnum"
                && body["startAutnum"]
                    .as_u64()
                    .zip(body["endAutnum"].as_u64())
                    .is_some_and(|(s, e)| normalized.parse::<u64>().is_ok_and(|n| n >= s && n <= e))
        }
        _ => false,
    };
    if !valid {
        return Err(fail(
            "unknown",
            "RDAP_IDENTITY",
            "Authoritative RDAP record did not match requested identity/range",
        ));
    }
    let sources = vec![response.source_id.clone()];
    let mut data = body;
    data["target"] = normalized.clone().into();
    data["kind"] = kind.into();
    data["found"] = true.into();
    data["source_id"] = response.source_id.into();
    data["bootstrap_source_id"] = bootstrap.source_id.into();
    data["authoritative_service"] = base.into();
    data["scope"] =
        "Public registry registration observations; no ownership or physical-location inference"
            .into();
    let names = data["nameservers"].as_array().cloned().unwrap_or_default();
    let mut p = Payload::new(data);
    p.entity(kind, normalized.clone(), &sources);
    if kind == "domain" {
        for name in names.iter().take(ctx.cfg.max_results) {
            if let Some(name) = name["ldhName"].as_str().and_then(|s| domain_name(s).ok()) {
                p.entity("domain", name.clone(), &sources);
                p.edge(
                    "domain",
                    normalized.clone(),
                    "rdap_nameserver",
                    "domain",
                    name,
                    &sources,
                );
            }
        }
    }
    Ok(p)
}

pub(super) fn tls(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let host = match target.parse::<IpAddr>() {
        Ok(ip) => {
            if !public_ip(ip) {
                return Err(fail(
                    "invalid",
                    "PRIVATE_TARGET",
                    "TLS requires public target",
                ));
            }
            ip.to_string()
        }
        Err(_) => domain_name(target)?,
    };
    let port = options["port"].as_u64().unwrap_or(443) as u16;
    if ctx.requests >= ctx.cfg.max_requests {
        return Err(fail("partial", "REQUEST_LIMIT", "Request budget exhausted"));
    }
    ctx.requests += 1;
    let addresses = public_addresses(&host, port)?;
    let timeout = Duration::from_secs(ctx.cfg.timeout_secs.clamp(1, 120));
    let started = Instant::now();
    let mut socket = None;
    for address in addresses {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) {
            socket = Some(stream);
            break;
        }
    }
    let mut socket = socket.ok_or_else(|| {
        fail(
            "error",
            "TLS_CONNECT",
            "Could not connect to public TLS endpoint",
        )
    })?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.clone())?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), name)?;
    while connection.is_handshaking() {
        if started.elapsed() > timeout {
            return Err(fail(
                "error",
                "TLS_TIMEOUT",
                "TLS handshake deadline exceeded",
            ));
        }
        connection.complete_io(&mut socket).map_err(|_| {
            fail(
                "error",
                "TLS_VERIFICATION",
                "TLS handshake or hostname/certificate verification failed",
            )
        })?;
    }
    let certificate = connection
        .peer_certificates()
        .and_then(|p| p.first())
        .ok_or_else(|| anyhow!("TLS endpoint did not present a certificate"))?
        .as_ref()
        .to_vec();
    if certificate.len() > ctx.cfg.max_bytes.saturating_sub(ctx.bytes) {
        return Err(fail(
            "partial",
            "BYTE_LIMIT",
            "TLS certificate exceeds remaining byte budget",
        ));
    }
    ctx.bytes += certificate.len();
    let hash = digest(&certificate);
    let sid = ctx.source(
        "Verified TLS leaf certificate",
        None,
        &certificate,
        "tls_certificate",
        json!({"host":host,"port":port,"hash_scope":"certificate_der","verified":true}),
    );
    let sources = vec![sid.clone()];
    let mut data = json!({"target":host,"port":port,"sha256":hash,"source_id":sid,"certificate_verified":true,"protocol":format!("{:?}",connection.protocol_version()),"cipher_suite":format!("{:?}",connection.negotiated_cipher_suite()),"scope":"Certificate presented by this endpoint at collection time"});
    if let Ok((_, cert)) = x509_parser::parse_x509_certificate(&certificate) {
        data["subject"] = cert.subject().to_string().into();
        data["issuer"] = cert.issuer().to_string().into();
        data["serial_number"] = cert.raw_serial_as_string().into();
        data["not_before"] = cert
            .validity()
            .not_before
            .to_rfc2822()
            .unwrap_or_default()
            .into();
        data["not_after"] = cert
            .validity()
            .not_after
            .to_rfc2822()
            .unwrap_or_default()
            .into();
        if let Ok(Some(sans)) = cert.subject_alternative_name() {
            data["dns_names"] = json!(
                sans.value
                    .general_names
                    .iter()
                    .filter_map(|n| match n {
                        x509_parser::extensions::GeneralName::DNSName(s) => Some(*s),
                        _ => None,
                    })
                    .take(ctx.cfg.max_results)
                    .collect::<Vec<_>>()
            );
        }
    }
    let kind = if host.parse::<IpAddr>().is_ok() {
        "ip"
    } else {
        "domain"
    };
    let mut p = Payload::new(data);
    p.entity(kind, host.clone(), &sources);
    p.entity("certificate_sha256", hash.clone(), &sources);
    p.edge(
        kind,
        host,
        "tls_presented_certificate",
        "certificate_sha256",
        hash,
        &sources,
    );
    Ok(p)
}

pub(super) fn ct(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let name = domain_name(target)?;
    let mut url = Url::parse("https://crt.sh/")?;
    url.query_pairs_mut()
        .append_pair("q", &format!("%.{name}"))
        .append_pair("output", "json");
    let response = ctx.fetch(url.as_str(), false)?;
    response.check()?;
    let raw = response.json()?;
    let rows = raw.as_array().ok_or_else(|| {
        fail(
            "unknown",
            "CT_SCHEMA",
            "Certificate log search did not return an array",
        )
    })?;
    let sources = vec![response.source_id.clone()];
    let mut names = std::collections::BTreeSet::new();
    let mut certificates = vec![];
    for row in rows.iter().take(10000) {
        let Some(values) = row["name_value"].as_str() else {
            continue;
        };
        if row["id"].as_u64().is_none() {
            continue;
        }
        let mut scoped = vec![];
        for value in values.lines() {
            let wildcard = value.starts_with("*.");
            let Ok(candidate) = domain_name(value.trim_start_matches("*.")) else {
                continue;
            };
            if candidate != name && !candidate.ends_with(&format!(".{name}")) {
                continue;
            }
            let candidate = if wildcard {
                format!("*.{candidate}")
            } else {
                candidate
            };
            names.insert(candidate.clone());
            scoped.push(candidate);
        }
        if !scoped.is_empty() && certificates.len() < ctx.cfg.max_results {
            let mut record = row.clone();
            record["dns_names"] = json!(scoped);
            certificates.push(record);
        }
    }
    let observed = names
        .iter()
        .take(ctx.cfg.max_results)
        .map(|s| json!({"name":s,"wildcard":s.starts_with("*."),"source_id":response.source_id}))
        .collect::<Vec<_>>();
    let mut p = Payload::new(
        json!({"target":name,"provider":"crt.sh","names":observed,"certificates":certificates,"names_truncated":names.len()>ctx.cfg.max_results,"source_ids":sources,"scope":"Historical certificate-log observations; not current DNS or ownership proof"}),
    );
    for record in observed {
        let value = record["name"].as_str().unwrap();
        if !value.starts_with("*.") {
            p.entity("domain", value, &sources);
            if value != name {
                p.edge(
                    "domain",
                    name.clone(),
                    "certificate_log_name",
                    "domain",
                    value,
                    &sources,
                );
            }
        }
    }
    Ok(p)
}
pub(super) fn email(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let Some((local, raw_domain)) = target.rsplit_once('@') else {
        return Err(fail("invalid", "INVALID_EMAIL", "Supply local-part@domain"));
    };
    if target.len() > 254
        || local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !regex::Regex::new(r"^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+$")?.is_match(local)
    {
        return Err(fail("invalid", "INVALID_EMAIL", "Unsupported email syntax"));
    }
    let name = domain_name(raw_domain)?;
    let normalized = format!("{local}@{name}");
    let mut p = Payload::new(
        json!({"address":normalized,"domain":name,"syntax_accepted":true,"mailbox_existence":"not_checked","checks":{},"scope":"Syntax and domain DNS only; no mailbox existence, signup, recovery, or SMTP probes"}),
    );
    let mut sources = vec![];
    for (key, target, kind) in [
        ("mx", name.clone(), "MX"),
        ("txt", name.clone(), "TXT"),
        ("dmarc", format!("_dmarc.{name}"), "TXT"),
    ] {
        match dns(ctx, &target, &json!({"types":[kind]})) {
            Ok(check) => {
                for query in check.data["queries"].as_array().into_iter().flatten() {
                    if let Some(s) = query["source_id"].as_str() {
                        sources.push(s.to_owned());
                    }
                }
                p.data["checks"][key] = check.data;
                p.errors.extend(check.errors);
            }
            Err(e) => p.errors.push(error_value(&e)),
        }
    }
    p.entity("email", normalized.clone(), &sources);
    p.entity("domain", name.clone(), &sources);
    p.edge(
        "email",
        normalized,
        "email_domain",
        "domain",
        name,
        &sources,
    );
    if !p.errors.is_empty() {
        p.status = "partial".into();
    }
    Ok(p)
}
pub(super) fn domain(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let name = domain_name(target)?;
    let modules = options["modules"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["dns", "rdap"]);
    let mut p = Payload::new(json!({"domain":name,"modules":{}}));
    for module in modules {
        let result = match module {
            "dns" => dns(ctx, &name, &json!({})),
            "rdap" => rdap(ctx, &name, &json!({})),
            "ct" => ct(ctx, &name),
            "tls" => tls(ctx, &name, &json!({})),
            "http" => web::http(ctx, &format!("https://{name}/")),
            _ => {
                return Err(fail(
                    "invalid",
                    "INVALID_MODULE",
                    "Domain modules are dns,rdap,ct,tls,http",
                ));
            }
        };
        match result {
            Ok(found) => {
                p.data["modules"][module] = json!({"status":found.status,"data":found.data});
                if !matches!(found.status.as_str(), "ok" | "not_found") {
                    p.status = "partial".into();
                }
                p.entities.extend(found.entities);
                p.relationships.extend(found.relationships);
                p.errors.extend(found.errors);
            }
            Err(e) => {
                p.data["modules"][module] =
                    json!({"status":error_status(&e),"error":error_value(&e)});
                p.errors.push(error_value(&e));
                p.status = "partial".into();
            }
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_uses_longest_ip_prefix() {
        let b = json!({"services":[[["1.0.0.0/8"],["https://b.example/"]],[["1.1.0.0/16"],["https://a.example/"]]]});
        assert_eq!(
            bootstrap_service(&b, "1.1.1.1", "ip").unwrap(),
            "https://a.example/"
        );
        assert!(bootstrap_service(&b, "2.2.2.2", "ip").is_err());
    }
    #[test]
    fn cidr_checks_address_families() {
        assert_eq!(
            cidr_contains("2001:4860::/32", "2001:4860:4860::8888".parse().unwrap()),
            Some(32)
        );
        assert_eq!(
            cidr_contains("1.1.1.0/24", "2.2.2.2".parse().unwrap()),
            None
        );
    }
}
