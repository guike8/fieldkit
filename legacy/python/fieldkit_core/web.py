"""Bounded web, archive-index, and optional search observations.

Provider contracts:
https://github.com/internetarchive/wayback/tree/master/wayback-cdx-server
https://api-dashboard.search.brave.com/api-reference/web/search/get
All returned text is untrusted source content, never an instruction to execute.
"""
from __future__ import annotations

from datetime import datetime, timezone
from hashlib import sha256
from html.parser import HTMLParser
import os
import re
from urllib.parse import quote, urlencode, urljoin

from .common import Failure, http_failure, result, web_url


_HEADERS = {
    "content-type", "content-length", "content-language", "cache-control", "etag",
    "last-modified", "server", "x-powered-by", "strict-transport-security",
    "content-security-policy", "content-security-policy-report-only", "x-frame-options",
    "x-content-type-options", "referrer-policy", "permissions-policy",
    "cross-origin-opener-policy", "cross-origin-resource-policy", "cross-origin-embedder-policy",
}


def _text(value: str, limit: int = 2000) -> str:
    """Bound external text and remove terminal controls without interpreting it."""
    return re.sub(r"\s+", " ", "".join(ch for ch in value if ord(ch) >= 32 or ch in "\t\r\n"))[:limit].strip()


def _entity(kind: str, value: str, source_id: str) -> dict:
    return {"type": kind, "value": value, "source_ids": [source_id]}


def _relation(source: str, relation: str, target: str, source_id: str) -> dict:
    return {"source": {"type": "url", "value": source}, "relation": relation,
            "target": {"type": "url", "value": target}, "source_ids": [source_id]}


def _resolve_link(base: str, href: str) -> str | None:
    if not href or len(href) > 8192:
        return None
    try:
        return web_url(urljoin(base, href.strip()))
    except (Failure, ValueError):
        return None


class _Page(HTMLParser):
    def __init__(self, url: str, maximum: int):
        super().__init__(convert_charrefs=True)
        self.url = url
        self.base = url
        self.maximum = maximum
        self.title = ""
        self.metadata: list[dict] = []
        self.canonical: str | None = None
        self.links: list[dict] = []
        self.links_seen = 0
        self.links_truncated = False
        self.metadata_truncated = False
        self._in_title = False
        self._anchor: dict | None = None
        self._seen: set[str] = set()
        self._has_base = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]):
        values = {key.lower(): value for key, value in attrs if isinstance(value, str)}
        if tag == "title":
            self._in_title = True
        elif tag == "base" and not self._has_base:
            base = _resolve_link(self.url, values.get("href", ""))
            if base:
                self.base, self._has_base = base, True
        elif tag == "meta":
            name = values.get("name", values.get("property", values.get("http-equiv", "")))
            if name and "content" in values:
                if len(self.metadata) < self.maximum:
                    self.metadata.append({"name": _text(name.lower(), 100), "content": _text(values["content"])})
                else:
                    self.metadata_truncated = True
        elif tag == "link" and "canonical" in values.get("rel", "").lower().split():
            if self.canonical is None:
                self.canonical = _resolve_link(self.base, values.get("href", ""))
        elif tag == "a":
            self._anchor = None
            href = _resolve_link(self.base, values.get("href", ""))
            if not href:
                return
            self.links_seen += 1
            if href in self._seen:
                return
            if len(self.links) >= self.maximum:
                self.links_truncated = True
                return
            self._seen.add(href)
            self._anchor = {"url": href, "text": ""}
            self.links.append(self._anchor)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]):
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_endtag(self, tag: str):
        if tag == "title":
            self._in_title = False
        elif tag == "a":
            self._anchor = None

    def handle_data(self, data: str):
        if self._in_title and len(self.title) < 1000:
            self.title += data[:1000 - len(self.title)]
        if self._anchor is not None and len(self._anchor["text"]) < 300:
            self._anchor["text"] += data[:300 - len(self._anchor["text"])]


def http(ctx, target: str) -> dict:
    """Inspect one public URL. Links and scripts are never followed/executed."""
    url = web_url(target)
    response = ctx.fetch(url, headers={"Accept": "text/html,application/xhtml+xml,application/json;q=0.8,*/*;q=0.5"})
    headers = {key.lower(): value for key, value in response.headers.items()}
    media_type = headers.get("content-type", "").split(";", 1)[0].strip().lower()
    data = {"requested_url": url, "url": response.url, "http_status": response.status,
            "media_type": media_type or None, "body_bytes": len(response.body),
            "body_sha256": sha256(response.body).hexdigest(), "source_ids": [response.source_id],
            "headers": {key: _text(value, 4096) for key, value in headers.items() if key in _HEADERS},
            "title": None, "metadata": [], "canonical_url": None, "links": [],
            "links_seen": 0, "links_truncated": False, "metadata_truncated": False,
            "scripts_executed": False}
    entities = [_entity("url", response.url, response.source_id)]
    relationships = []
    if media_type in {"text/html", "application/xhtml+xml"}:
        encoding = "utf-8"
        match = re.search(r"charset\s*=\s*[\"']?([^\s;\"']+)", headers.get("content-type", ""), re.I)
        if match:
            encoding = match.group(1)
        try:
            content = response.body.decode(encoding, errors="replace")
        except LookupError:
            content = response.body.decode("utf-8", errors="replace")
        page = _Page(response.url, ctx.max_results)
        page.feed(content)
        page.close()
        data.update(title=_text(page.title, 1000) or None, metadata=page.metadata,
                    canonical_url=page.canonical, links_seen=page.links_seen,
                    links_truncated=page.links_truncated, metadata_truncated=page.metadata_truncated)
        for link in page.links:
            link["text"] = _text(link["text"], 300)
            link["source_ids"] = [response.source_id]
            data["links"].append(link)
            entities.append(_entity("url", link["url"], response.source_id))
            relationships.append(_relation(response.url, "links_to", link["url"], response.source_id))
        if page.canonical:
            entities.append(_entity("url", page.canonical, response.source_id))
            relationships.append(_relation(response.url, "declares_canonical", page.canonical, response.source_id))
    if response.status in {404, 410}:
        return result(data, status="not_found", entities=entities, relationships=relationships)
    failure = http_failure(response)
    if failure:
        return result(data, status=failure.status, errors=[failure.as_dict()], entities=entities,
                      relationships=relationships)
    return result(data, entities=entities, relationships=relationships)


def _limit(ctx, value: int | None, maximum: int) -> int:
    selected = min(ctx.max_results, maximum) if value is None else value
    if isinstance(selected, bool) or not isinstance(selected, int) or not 1 <= selected <= min(ctx.max_results, maximum):
        raise Failure("INVALID_LIMIT", f"Limit must be an integer from 1 to {min(ctx.max_results, maximum)}", "invalid")
    return selected


def archive(ctx, target: str, limit: int | None = None, prefix: bool = False) -> dict:
    """Read a bounded capture index; do not fetch or submit archived pages."""
    url = web_url(target)
    selected = _limit(ctx, limit, 1000)
    if not isinstance(prefix, bool):
        raise Failure("INVALID_PREFIX", "prefix must be a boolean", "invalid")
    fields = ["timestamp", "original", "statuscode", "mimetype", "digest"]
    query = urlencode({"url": url, "output": "json", "fl": ",".join(fields),
                       "matchType": "prefix" if prefix else "exact", "limit": selected + 1})
    response = ctx.fetch("https://web.archive.org/cdx/search/cdx?" + query)
    failure = http_failure(response)
    if failure:
        raise failure
    payload = ctx.json(response)
    if not isinstance(payload, list) or (payload and payload[0] != fields):
        raise Failure("ARCHIVE_SCHEMA", "Archive API did not return the expected CDX header", details={"source_id": response.source_id})
    rows = payload[1:] if payload else []
    captures, entities, relationships = [], [_entity("url", url, response.source_id)], []
    invalid = 0
    for row in rows[:selected]:
        try:
            if not isinstance(row, list) or len(row) != len(fields) or not all(isinstance(v, str) for v in row):
                raise ValueError("Malformed CDX row")
            timestamp, original, status_code, mimetype, digest = row
            if not re.fullmatch(r"[0-9]{14}", timestamp):
                raise ValueError("Invalid CDX timestamp")
            captured_at = datetime.strptime(timestamp, "%Y%m%d%H%M%S").replace(tzinfo=timezone.utc).isoformat().replace("+00:00", "Z")
            normalized = web_url(original)
            if status_code != "-" and (not status_code.isdigit() or not 100 <= int(status_code) <= 599):
                raise ValueError("Invalid CDX status")
            replay = "https://web.archive.org/web/" + timestamp + "id_/" + quote(normalized, safe=":/?=&%+-._~")
            captures.append({"timestamp": timestamp, "captured_at": captured_at, "original_url": original,
                             "normalized_url": normalized, "http_status": int(status_code) if status_code != "-" else None,
                             "mimetype": _text(mimetype, 200), "archive_digest": _text(digest, 200),
                             "replay_url": replay, "source_ids": [response.source_id]})
            entities.append(_entity("url", normalized, response.source_id))
            entities.append(_entity("url", replay, response.source_id))
            relationships.append(_relation(normalized, "archived_at", replay, response.source_id))
        except (Failure, ValueError, OverflowError):
            invalid += 1
    data = {"url": url, "match_type": "prefix" if prefix else "exact", "provider": "Internet Archive CDX",
            "matching_semantics": "CDX canonical URL matching can merge scheme and equivalent URL forms",
            "captures": captures, "returned": len(captures), "limit": selected,
            "truncated": len(rows) > selected, "invalid_records": invalid,
            "source_ids": [response.source_id], "replays_fetched": False}
    errors = [Failure("ARCHIVE_RECORDS", f"Skipped {invalid} malformed archive record(s)",
                      details={"source_id": response.source_id}).as_dict()] if invalid else []
    status = "partial" if invalid else ("ok" if captures else "not_found")
    return result(data, status=status, errors=errors, entities=entities, relationships=relationships)


def search(ctx, target: str, limit: int | None = None) -> dict:
    """Search Brave's public index with an explicitly configured API key."""
    if not isinstance(target, str):
        raise Failure("INVALID_QUERY", "Search query must be a string", "invalid")
    query = target.strip()
    if not query or len(query) > 600 or len(query.split()) > 75 or any(ord(c) < 32 or ord(c) == 127 for c in query):
        raise Failure("INVALID_QUERY", "Search query must contain 1–600 characters and at most 75 words, without controls", "invalid")
    selected = _limit(ctx, limit, 20)
    key = os.environ.get("BRAVE_SEARCH_API_KEY", "").strip()
    if not key:
        raise Failure("SEARCH_NOT_CONFIGURED", "Set BRAVE_SEARCH_API_KEY to enable Brave web search", "unavailable",
                      details={"required_environment_variable": "BRAVE_SEARCH_API_KEY"})
    if len(key) > 4096 or any(ord(ch) < 33 or ord(ch) > 126 for ch in key):
        raise Failure("SEARCH_KEY_FORMAT", "Configured search key has an invalid format", "unavailable")
    params = urlencode({"q": query, "count": selected, "result_filter": "web", "spellcheck": "false",
                        "text_decorations": "false"})
    response = ctx.fetch("https://api.search.brave.com/res/v1/web/search?" + params,
                         headers={"Accept": "application/json", "X-Subscription-Token": key},
                         follow_redirects=False)
    failure = http_failure(response)
    if failure:
        raise failure
    payload = ctx.json(response)
    if not isinstance(payload, dict) or payload.get("type") != "search" or not isinstance(payload.get("query"), dict):
        raise Failure("SEARCH_SCHEMA", "Search provider returned an unexpected response schema", details={"source_id": response.source_id})
    query_info = payload["query"]
    if not isinstance(query_info.get("original"), str):
        raise Failure("SEARCH_SCHEMA", "Search response has no original query", details={"source_id": response.source_id})
    web = payload.get("web")
    if web is None:
        rows = []
    elif isinstance(web, dict) and isinstance(web.get("results"), list):
        rows = web["results"]
    else:
        raise Failure("SEARCH_SCHEMA", "Search response has an invalid web results collection", details={"source_id": response.source_id})
    found, entities, invalid = [], [], 0
    for rank, row in enumerate(rows[:selected], 1):
        try:
            if not isinstance(row, dict) or not isinstance(row.get("url"), str) or not isinstance(row.get("title"), str):
                raise ValueError("Invalid search result")
            url = web_url(row["url"])
            description = row.get("description", "")
            if not isinstance(description, str):
                raise ValueError("Invalid search snippet")
            found.append({"rank": rank, "url": url, "title": _text(row["title"], 1000),
                          "snippet": _text(description, 4000), "source_ids": [response.source_id]})
            entities.append(_entity("url", url, response.source_id))
        except (Failure, ValueError):
            invalid += 1
    changed = query_info["original"] != query or bool(query_info.get("altered"))
    errors = []
    if invalid:
        errors.append(Failure("SEARCH_RECORDS", f"Skipped {invalid} malformed search result(s)",
                              details={"source_id": response.source_id}).as_dict())
    if changed:
        errors.append(Failure("SEARCH_QUERY_CHANGED", "Provider reported a different or altered search query",
                              details={"source_id": response.source_id}).as_dict())
    more = query_info.get("more_results_available")
    data = {"provider": "Brave Search", "query": query, "provider_query": _text(query_info["original"], 600),
            "altered_query": _text(query_info["altered"], 600) if isinstance(query_info.get("altered"), str) else None,
            "results": found, "returned": len(found), "limit": selected,
            "more_results_available": more if isinstance(more, bool) else None,
            "truncated": len(rows) > selected or more is True, "invalid_records": invalid,
            "source_ids": [response.source_id]}
    return result(data, status="partial" if errors else ("ok" if found else "not_found"),
                  errors=errors, entities=entities)
