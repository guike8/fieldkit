"""Bounded local evidence hashing and lexical indicator extraction.

These commands never resolve extracted indicators or send local contents to a
remote provider. An indicator is an observed string, not an attributed identity.
"""
from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import ipaddress
import mimetypes
import os
from pathlib import Path
import re
import stat
import struct

from .common import Failure, domain_name, result, web_url


_CHUNK = 64 * 1024
_DEFAULT_MAX_FILE = 20 * 1024 * 1024
_MAX_OCCURRENCES = 8


def _limits(ctx) -> tuple[int, int]:
    max_file = int(getattr(ctx, "max_file_bytes", _DEFAULT_MAX_FILE))
    max_results = int(getattr(ctx, "max_results", 50))
    if max_file < 1 or max_results < 1:
        raise Failure("INVALID_LIMIT", "File and result limits must be positive", "invalid")
    return max_file, min(max_results, 10000)


def _media_type(head: bytes) -> tuple[str, str]:
    signatures = (
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"), (b"GIF89a", "image/gif"),
        (b"%PDF-", "application/pdf"),
        (b"PK\x03\x04", "application/zip"), (b"PK\x05\x06", "application/zip"),
        (b"\x1f\x8b", "application/gzip"), (b"BZh", "application/x-bzip2"),
        (b"\xfd7zXZ\x00", "application/x-xz"),
        (b"SQLite format 3\x00", "application/vnd.sqlite3"),
        (b"\x7fELF", "application/x-elf"),
        (b"\x00asm", "application/wasm"),
    )
    for signature, media_type in signatures:
        if head.startswith(signature):
            return media_type, "file_signature"
    if len(head) >= 12 and head[:4] == b"RIFF" and head[8:12] == b"WEBP":
        return "image/webp", "file_signature"
    if b"\x00" not in head:
        try:
            text = head.decode("utf-8-sig")
            if not any(ord(ch) < 32 and ch not in "\t\r\n" for ch in text):
                prefix = text.lstrip()[:200].lower()
                if prefix.startswith("<!doctype html") or prefix.startswith("<html"):
                    return "text/html", "utf8_header_heuristic"
                return "text/plain", "utf8_header_heuristic"
        except UnicodeDecodeError:
            pass
    return "application/octet-stream", "unrecognized"


def _image_dimensions(head: bytes, media_type: str) -> dict | None:
    width = height = 0
    if media_type == "image/png" and len(head) >= 24 and head[8:16] == b"\x00\x00\x00\rIHDR":
        width, height = struct.unpack(">II", head[16:24])
    elif media_type == "image/jpeg":
        position = 2
        sof = {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}
        while position + 4 <= len(head):
            if head[position] != 0xFF:
                break
            while position < len(head) and head[position] == 0xFF:
                position += 1
            if position >= len(head):
                break
            marker = head[position]
            position += 1
            if marker in {0xD8, 0x01} or 0xD0 <= marker <= 0xD7:
                continue
            if marker in {0xD9, 0xDA} or position + 2 > len(head):
                break
            length = int.from_bytes(head[position:position + 2], "big")
            if length < 2 or position + length > len(head):
                break
            if marker in sof and length >= 8:
                height = int.from_bytes(head[position + 3:position + 5], "big")
                width = int.from_bytes(head[position + 5:position + 7], "big")
                break
            position += length
    if width and height:
        return {"width": width, "height": height, "unit": "pixels", "method": "header_only"}
    return None


def _read_file(ctx, target: str, *, keep_body: bool = False) -> tuple[dict, bytes | None]:
    max_file, _ = _limits(ctx)
    try:
        path = Path(os.path.abspath(os.fspath(target)))
        flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
        if path.is_symlink():
            raise Failure("SYMLINK_NOT_ALLOWED", "Supply a regular file directly, not a symlink", "invalid")
        descriptor = os.open(path, flags)
    except Failure:
        raise
    except FileNotFoundError as exc:
        raise Failure("FILE_NOT_FOUND", "Local evidence file does not exist", "not_found") from exc
    except (OSError, ValueError, TypeError) as exc:
        raise Failure("FILE_OPEN_FAILED", "Could not open local evidence file", "invalid",
                      details={"reason": str(exc)}) from exc
    try:
        initial = os.fstat(descriptor)
        if not stat.S_ISREG(initial.st_mode):
            raise Failure("NOT_REGULAR_FILE", "Evidence must be a regular file", "invalid")
        if initial.st_size > max_file:
            raise Failure("FILE_TOO_LARGE", "Local evidence exceeds the configured byte limit", "invalid",
                          details={"limit_bytes": max_file, "size_bytes": initial.st_size})
        sha256 = hashlib.sha256()
        sha512 = hashlib.sha512()
        chunks = [] if keep_body else None
        head = bytearray()
        total = 0
        while True:
            chunk = os.read(descriptor, min(_CHUNK, max_file - total + 1))
            if not chunk:
                break
            total += len(chunk)
            if total > max_file:
                raise Failure("FILE_TOO_LARGE", "Local evidence grew beyond the configured byte limit", "invalid",
                              details={"limit_bytes": max_file})
            sha256.update(chunk)
            sha512.update(chunk)
            if len(head) < _CHUNK:
                head.extend(chunk[:_CHUNK - len(head)])
            if chunks is not None:
                chunks.append(chunk)
        final = os.fstat(descriptor)
        if (initial.st_size, initial.st_mtime_ns) != (final.st_size, final.st_mtime_ns) or total != final.st_size:
            raise Failure("FILE_CHANGED", "Evidence changed while it was being read; retry with a stable copy", "error",
                          retryable=True)
        media_type, method = _media_type(bytes(head))
        digest = sha256.hexdigest()
        source_id = ctx.add_local_source(str(path), digest, total, media_type)
        data = {"path": str(path), "filename": path.name, "extension": path.suffix.lower(),
                "size_bytes": total, "sha256": digest, "sha512": sha512.hexdigest(),
                "media_type": media_type, "media_type_method": method,
                "filename_media_type": mimetypes.guess_type(path.name)[0],
                "modified_at": datetime.fromtimestamp(final.st_mtime, timezone.utc).isoformat().replace("+00:00", "Z"),
                "source_id": source_id}
        image = _image_dimensions(bytes(head), media_type)
        if image:
            data["image"] = image
        return data, b"".join(chunks) if chunks is not None else None
    except OSError as exc:
        raise Failure("FILE_READ_FAILED", "Could not read local evidence", "error",
                      details={"reason": str(exc)}) from exc
    finally:
        os.close(descriptor)


def file_info(ctx, target: str) -> dict:
    """Hash a stable regular file without executing or uploading it."""
    data, _ = _read_file(ctx, target)
    return result(data, entities=[{"type": "file_sha256", "value": data["sha256"],
                                   "source_ids": [data["source_id"]]}])


_DOMAIN_CORE = r"(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.){1,126}(?:[A-Za-z][A-Za-z0-9-]{1,62})"
_PATTERNS = {
    "url": re.compile(r"(?i)\bhttps?://[^\s<>\"'`]{1,4096}(?![^\s<>\"'`])"),
    "email": re.compile(r"(?<![A-Za-z0-9.!#$%&'*+/=?^_`{|}~-])"
                        r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@" + _DOMAIN_CORE +
                        r"(?![A-Za-z0-9_-]|\.[A-Za-z0-9_.-])"),
    "domain": re.compile(r"(?<![A-Za-z0-9_.-])" + _DOMAIN_CORE + r"\.?(?![A-Za-z0-9_.-])"),
    "ipv4": re.compile(r"(?<![A-Za-z0-9_.:])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![A-Za-z0-9_]|\.[A-Za-z0-9_.-])"),
    "ipv6": re.compile(r"(?<![A-Za-z0-9_.:%])[0-9A-Fa-f:.]{2,80}(?![A-Za-z0-9_.:])"),
}


def _normalize_candidate(kind: str, observed: str) -> tuple[str, str] | None:
    try:
        if kind == "url":
            candidate = observed.rstrip(".,;")
            for opener, closer in (("(", ")"), ("[", "]"), ("{", "}")):
                while candidate.endswith(closer) and candidate.count(closer) > candidate.count(opener):
                    candidate = candidate[:-1]
            if len(candidate) > 4096:
                return None
            return web_url(candidate), candidate
        if kind == "domain":
            return domain_name(observed), observed
        if kind == "email":
            local_part, domain = observed.rsplit("@", 1)
            if not 1 <= len(local_part) <= 64 or len(observed) > 254 or local_part.startswith(".") or local_part.endswith(".") or ".." in local_part:
                return None
            return local_part + "@" + domain_name(domain), observed
        if kind == "ipv6" and observed.count(":") < 2:
            return None
        if kind == "ipv6":
            observed = observed.rstrip(".")
        address = ipaddress.ip_address(observed)
        if (kind == "ipv4" and address.version != 4) or (kind == "ipv6" and address.version != 6):
            return None
        return address.compressed, observed
    except (Failure, ValueError, UnicodeError):
        return None


def _kinds(value) -> list[str]:
    if value is None:
        return list(_PATTERNS)
    if isinstance(value, str):
        value = value.split(",")
    aliases = {"urls": "url", "emails": "email", "domains": "domain", "ip": "ip", "ips": "ip"}
    kinds = []
    for entry in value:
        kind = aliases.get(str(entry).strip().lower(), str(entry).strip().lower())
        if kind == "ip":
            kinds.extend(item for item in ("ipv4", "ipv6") if item not in kinds)
        elif kind in _PATTERNS:
            if kind not in kinds:
                kinds.append(kind)
        else:
            raise Failure("INVALID_KIND", "Extraction kinds: url,email,domain,ipv4,ipv6 (or ip)", "invalid")
    if not kinds:
        raise Failure("INVALID_KIND", "Choose at least one extraction kind", "invalid")
    return kinds


def extract(ctx, target: str, kinds=None) -> dict:
    """Extract bounded lexical indicators from UTF-8 text, preserving positions."""
    selected = _kinds(kinds)
    _, maximum = _limits(ctx)
    metadata, body = _read_file(ctx, target, keep_body=True)
    try:
        text = body.decode("utf-8-sig")
        if "\x00" in text or any(ord(ch) < 32 and ch not in "\t\r\n" for ch in text):
            raise ValueError("binary controls")
    except (UnicodeDecodeError, ValueError) as exc:
        raise Failure("NOT_UTF8_TEXT", "Indicator extraction accepts UTF-8 text, HTML, JSON, or CSV; use file for binary metadata", "invalid",
                      details={"source_id": metadata["source_id"]}) from exc
    candidate_limit = min(50000, max(1000, maximum * 32))
    candidates = []
    scan_limited = False
    for kind in selected:
        for match in _PATTERNS[kind].finditer(text):
            normalized = _normalize_candidate(kind, match.group())
            if normalized is None:
                continue
            value, observed = normalized
            candidates.append((match.start(), match.start() + len(observed), kind, value, observed))
            if len(candidates) >= candidate_limit:
                scan_limited = True
                break
        if scan_limited:
            break
    candidates.sort(key=lambda item: (item[0], item[2], item[1]))
    indicators = {}
    line = 1
    line_start = last_position = 0
    for start, end, kind, value, observed in candidates:
        line += text.count("\n", last_position, start)
        last_newline = text.rfind("\n", last_position, start)
        if last_newline >= 0:
            line_start = last_newline + 1
        last_position = start
        key = (kind, value)
        if key not in indicators:
            indicators[key] = {"type": kind, "value": value, "source_id": metadata["source_id"],
                               "occurrence_count": 0, "occurrences": [], "occurrences_truncated": False}
        item = indicators[key]
        item["occurrence_count"] += 1
        if len(item["occurrences"]) < _MAX_OCCURRENCES:
            item["occurrences"].append({"line": line, "column": start - line_start + 1,
                                        "char_offset": start, "end_char_offset": end, "observed": observed})
        else:
            item["occurrences_truncated"] = True
    returned = list(indicators.values())[:maximum]
    source_ids = [metadata["source_id"]]
    entities = [{"type": "file_sha256", "value": metadata["sha256"], "source_ids": source_ids}]
    relationships = []
    for item in returned:
        entities.append({"type": item["type"], "value": item["value"], "source_ids": source_ids})
        relationships.append({"source": {"type": "file_sha256", "value": metadata["sha256"]},
                              "relation": "contains_indicator", "target": {"type": item["type"], "value": item["value"]},
                              "source_ids": source_ids})
    data = {"file": metadata, "kinds": selected, "indicators": returned, "returned": len(returned),
            "unique_indicators_seen": len(indicators), "truncated": scan_limited or len(indicators) > maximum,
            "scan_complete": not scan_limited, "candidate_limit": candidate_limit,
            "occurrence_limit_per_indicator": _MAX_OCCURRENCES,
            "offset_convention": "Zero-based Unicode code-point offsets in UTF-8-sig decoded text; end exclusive; line and column are one-based",
            "interpretation": "Lexical strings observed in a local file; not resolved, verified active, or attributed to a person. Domain/email patterns cover common ASCII and punycode forms."}
    return result(data, entities=entities, relationships=relationships)
