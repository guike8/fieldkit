use super::*;
use regex::Regex;
use std::{
    fs::{self, OpenOptions},
    path::Path,
};

pub(super) fn read_file(ctx: &mut Context<'_>, target: &str) -> Result<(Value, Vec<u8>)> {
    let path = Path::new(target);
    let before = fs::symlink_metadata(path).map_err(|_| {
        fail(
            "not_found",
            "FILE_NOT_FOUND",
            "Local evidence file could not be found",
        )
    })?;
    if !before.file_type().is_file() {
        return Err(fail(
            "invalid",
            "NOT_REGULAR_FILE",
            "Evidence must be a regular file, without a symlink",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100 | 0x4);
        #[cfg(not(target_os = "macos"))]
        options.custom_flags(0x20000 | 0x800);
    }
    let file = options.open(path)?;
    let initial = file.metadata()?;
    if !initial.is_file() || initial.len() > ctx.cfg.max_bytes as u64 {
        return Err(fail(
            "invalid",
            "FILE_LIMIT",
            "Local file is not regular or exceeds the byte limit",
        ));
    }
    let mut body = vec![];
    file.take(ctx.cfg.max_bytes as u64 + 1)
        .read_to_end(&mut body)?;
    let final_meta = fs::metadata(path)?;
    if body.len() > ctx.cfg.max_bytes
        || initial.len() != final_meta.len()
        || initial.modified().ok() != final_meta.modified().ok()
        || body.len() as u64 != initial.len()
    {
        return Err(fail(
            "error",
            "FILE_CHANGED",
            "Evidence changed while being read",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.ino() != initial.ino()
            || before.dev() != initial.dev()
            || initial.ino() != final_meta.ino()
            || initial.dev() != final_meta.dev()
        {
            return Err(fail(
                "error",
                "FILE_CHANGED",
                "Evidence file changed while being read",
            ));
        }
    }
    let (media, method) = media_type(&body);
    let sha = digest(&body);
    let sid = id("src");
    ctx.sources.push(json!({"source_id":sid,"kind":"local_file","name":target,"url":null,"sha256":sha,"bytes":body.len(),"media_type":media,"observed_at":now(),"raw_copied":false,"hash_scope":"local_file_bytes","untrusted_content":true}));
    use sha2::{Digest, Sha512};
    let metadata = json!({"path":fs::canonicalize(path)?.to_string_lossy(),"filename":path.file_name().unwrap_or_default().to_string_lossy(),"size_bytes":body.len(),"sha256":sha,"sha512":format!("{:x}",Sha512::digest(&body)),"media_type":media,"media_type_method":method,"source_id":sid,"raw_copied":false});
    Ok((metadata, body))
}

fn media_type(body: &[u8]) -> (&'static str, &'static str) {
    for (signature, media) in [
        (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"%PDF-", "application/pdf"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1f\x8b", "application/gzip"),
        (b"SQLite format 3\0", "application/vnd.sqlite3"),
        (b"\x7fELF", "application/x-elf"),
        (b"\0asm", "application/wasm"),
    ] {
        if body.starts_with(signature) {
            return (media, "file_signature");
        }
    }
    if std::str::from_utf8(body).is_ok() && !body.contains(&0) {
        ("text/plain", "utf8_heuristic")
    } else {
        ("application/octet-stream", "unrecognized")
    }
}
pub(super) fn file(ctx: &mut Context<'_>, target: &str) -> Result<Payload> {
    let (mut metadata, body) = read_file(ctx, target)?;
    if let Some((width, height)) = image_dimensions(&body) {
        metadata["image"] =
            json!({"width":width,"height":height,"unit":"pixels","method":"header_only"});
    }
    let sources = vec![metadata["source_id"].as_str().unwrap().to_owned()];
    let hash = metadata["sha256"].clone();
    let mut p = Payload::new(metadata);
    p.entity("file_sha256", hash, &sources);
    Ok(p)
}

fn image_dimensions(body: &[u8]) -> Option<(u32, u32)> {
    let (width, height) = if body.len() >= 24
        && body.starts_with(b"\x89PNG\r\n\x1a\n")
        && body[8..16] == *b"\0\0\0\rIHDR"
    {
        (
            u32::from_be_bytes(body[16..20].try_into().ok()?),
            u32::from_be_bytes(body[20..24].try_into().ok()?),
        )
    } else if body.starts_with(b"\xff\xd8\xff") {
        let mut pos = 2;
        let mut dimensions = None;
        while pos + 4 <= body.len() {
            if body[pos] != 0xff {
                break;
            }
            while pos < body.len() && body[pos] == 0xff {
                pos += 1;
            }
            let marker = *body.get(pos)?;
            pos += 1;
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                continue;
            }
            if matches!(marker, 0xd9 | 0xda) || pos + 2 > body.len() {
                break;
            }
            let length = u16::from_be_bytes(body[pos..pos + 2].try_into().ok()?) as usize;
            if length < 2 || pos + length > body.len() {
                break;
            }
            if [
                0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
            ]
            .contains(&marker)
                && length >= 8
            {
                dimensions = Some((
                    u16::from_be_bytes(body[pos + 5..pos + 7].try_into().ok()?) as u32,
                    u16::from_be_bytes(body[pos + 3..pos + 5].try_into().ok()?) as u32,
                ));
                break;
            }
            pos += length;
        }
        dimensions?
    } else {
        return None;
    };
    (width > 0 && height > 0).then_some((width, height))
}

pub(super) fn extract(ctx: &mut Context<'_>, target: &str, options: &Value) -> Result<Payload> {
    let (metadata, body) = read_file(ctx, target)?;
    let content = std::str::from_utf8(&body)
        .map_err(|_| fail("invalid", "NOT_UTF8_TEXT", "Extraction requires UTF-8 text"))?
        .trim_start_matches('\u{feff}');
    if content
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(fail(
            "invalid",
            "NOT_UTF8_TEXT",
            "Extraction rejects binary controls",
        ));
    }
    let selected = options["kinds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["url", "email", "domain", "ipv4", "ipv6"]);
    let patterns = [
        ("url", r#"(?i)https?://[^\s<>"'`]{1,4096}"#),
        (
            "email",
            r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,63}",
        ),
        (
            "domain",
            r"(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,63}",
        ),
        ("ipv4", r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}"),
        ("ipv6", r"[0-9A-Fa-f:.]{2,80}"),
    ];
    let sources = vec![metadata["source_id"].as_str().unwrap().to_owned()];
    let mut indicators = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0;
    for kind in selected {
        let Some((_, pattern)) = patterns.iter().find(|(k, _)| *k == kind) else {
            return Err(fail(
                "invalid",
                "INVALID_KIND",
                "Extraction kinds are url,email,domain,ipv4,ipv6",
            ));
        };
        for m in Regex::new(pattern)?.find_iter(content).take(50000) {
            // Word boundaries are checked explicitly: Rust regex deliberately has no lookaround.
            let prior = content[..m.start()].chars().next_back();
            let after = content[m.end()..].chars().next();
            if kind != "url"
                && (prior
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                    || after
                        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
            {
                continue;
            }
            let observed = m.as_str().trim_end_matches(['.', ',', ';']);
            let normalized = match kind {
                "url" => public_url(observed).ok().map(|u| u.to_string()),
                "domain" => domain_name(observed).ok(),
                "email" => observed
                    .rsplit_once('@')
                    .and_then(|(l, d)| domain_name(d).ok().map(|d| format!("{l}@{d}"))),
                "ipv4" | "ipv6" => observed
                    .parse::<IpAddr>()
                    .ok()
                    .filter(|ip| {
                        if kind == "ipv4" {
                            ip.is_ipv4()
                        } else {
                            ip.is_ipv6()
                        }
                    })
                    .map(|ip| ip.to_string()),
                _ => None,
            };
            let Some(value) = normalized else {
                continue;
            };
            total += 1;
            if !seen.insert((kind.to_owned(), value.clone()))
                || indicators.len() >= ctx.cfg.max_results
            {
                continue;
            }
            indicators.push(json!({"kind":kind,"value":value,"observed":observed,"offset_bytes":m.start(),"line":content[..m.start()].bytes().filter(|b|*b==b'\n').count()+1,"verification":"unverified_lexical_indicator","source_ids":sources}));
        }
    }
    let mut p = Payload::new(
        json!({"file":metadata,"indicators":indicators,"returned":indicators.len(),"matches_seen":total,"truncated":seen.len()>ctx.cfg.max_results,"network_requests":0,"scope":"Observed strings only; no remote verification or attribution"}),
    );
    for indicator in indicators {
        let kind = if indicator["kind"] == "ipv4" || indicator["kind"] == "ipv6" {
            "ip"
        } else {
            indicator["kind"].as_str().unwrap()
        };
        p.entity(kind, indicator["value"].clone(), &sources);
        p.edge(
            "file_sha256",
            metadata["sha256"].clone(),
            "contains_indicator",
            kind,
            indicator["value"].clone(),
            &sources,
        );
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extraction_preserves_offsets_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evidence.txt");
        std::fs::write(
            &path,
            "source https://example.org/about\nemail alice@example.org",
        )
        .unwrap();
        let cfg = Config::default();
        let mut ctx = Context::new(&cfg);
        let p = extract(
            &mut ctx,
            path.to_str().unwrap(),
            &json!({"kinds":["url","email"]}),
        )
        .unwrap();
        assert_eq!(ctx.requests, 0);
        assert_eq!(p.data["indicators"][0]["offset_bytes"], 7);
        assert_eq!(p.data["indicators"][1]["line"], 2);
        assert!(ctx.blobs.is_empty());
    }
}
