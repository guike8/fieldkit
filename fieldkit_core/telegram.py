"""Public Telegram previews, local exports, and an attributed Bellingcat adapter.

No login, bot token, phone/contact lookup, join, or message-send operations.
Public previews expose a bounded window, not a complete channel history.
"""
from __future__ import annotations

from datetime import datetime, timezone
from html.parser import HTMLParser
import importlib.util
import re
from urllib.parse import parse_qs, quote, urlencode, urljoin, urlsplit

from .common import Failure, http_failure, result, web_url
from .local import _read_file
from .transport import strict_json

_HANDLE = re.compile(r"[A-Za-z][A-Za-z0-9_]{3,31}")
_VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"}
_TEXT_LIMIT = 10000


def normalize_target(target, *, require_post=False):
    if not isinstance(target, str) or not target or len(target) > 2048 or any(ord(c) < 33 for c in target):
        raise Failure("INVALID_TELEGRAM_TARGET", "Supply a public Telegram username or t.me message URL", "invalid")
    post = None
    if "://" in target:
        parsed = urlsplit(web_url(target))
        if parsed.scheme != "https" or parsed.hostname not in {"t.me", "telegram.me", "www.t.me", "www.telegram.me"} or parsed.port not in {None, 443}:
            raise Failure("INVALID_TELEGRAM_TARGET", "Only public HTTPS t.me or telegram.me links are supported", "invalid")
        segments = [value for value in parsed.path.split("/") if value]
        if segments and segments[0] == "s":
            segments = segments[1:]
        if not segments or len(segments) > 2 or (len(segments) == 2 and not segments[1].isdigit()):
            raise Failure("INVALID_TELEGRAM_TARGET", "Expected a public channel/profile or numeric message link", "invalid")
        handle = segments[0]
        if len(segments) == 2:
            post = int(segments[1])
    else:
        handle = target.removeprefix("@")
    if not _HANDLE.fullmatch(handle) or handle.lower() in {"joinchat", "share", "proxy", "socks", "login", "confirmphone", "addstickers", "addemoji", "iv"}:
        raise Failure("INVALID_TELEGRAM_TARGET", "Unsupported or non-public Telegram username/link", "invalid")
    if post is not None and not 1 <= post <= 2147483647:
        raise Failure("INVALID_TELEGRAM_POST", "Message ID must be a positive 32-bit integer", "invalid")
    if require_post and post is None:
        raise Failure("TELEGRAM_POST_REQUIRED", "Supply an exact public message URL such as https://t.me/telegram/1", "invalid")
    return handle, post, "https://t.me/" + handle + ("/" + str(post) if post else "")


def _limit(ctx, limit, maximum=500):
    if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= maximum:
        raise Failure("INVALID_LIMIT", f"limit must be an integer from 1 to {maximum}", "invalid")
    return min(limit, ctx.max_results)


def _query(query):
    if query is not None and (not isinstance(query, str) or not query.strip() or len(query) > 200 or any(ord(c) < 32 for c in query)):
        raise Failure("INVALID_QUERY", "query must contain 1-200 characters without controls", "invalid")
    return query.casefold() if query is not None else None


class Node:
    def __init__(self, tag="", attrs=None):
        self.tag, self.attrs, self.children = tag, attrs or {}, []

    def has(self, name):
        return name in self.attrs.get("class", "").split()

    def walk(self):
        pending = [self]
        while pending:
            current = pending.pop()
            yield current
            pending.extend(reversed([child for child in current.children if isinstance(child, Node)]))

    def find(self, name):
        return next((node for node in self.walk() if node.has(name)), None)

    def text(self, maximum=_TEXT_LIMIT):
        pending, pieces, size = list(reversed(self.children)), [], 0
        while pending and size < maximum:
            child = pending.pop()
            if isinstance(child, str):
                pieces.append(child[:maximum-size])
                size += len(child)
            elif child.tag not in {"script", "style"}:
                pending.extend(reversed(child.children))
        return re.sub(r"\s+", " ", "".join(pieces)).strip()


class Page(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.root = Node()
        self.stack, self.count = [self.root], 0

    def handle_starttag(self, tag, attrs):
        self.count += 1
        if self.count > 50000 or len(self.stack) > 128:
            raise Failure("TELEGRAM_HTML_LIMIT", "Telegram preview exceeds HTML parsing limits", "unknown")
        node = Node(tag, {key: value for key, value in attrs if isinstance(value, str)})
        self.stack[-1].children.append(node)
        if tag not in _VOID:
            self.stack.append(node)
        if tag == "br":
            self.stack[-1].children.append("\n")

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if tag not in _VOID:
            self.handle_endtag(tag)

    def handle_endtag(self, tag):
        for index in range(len(self.stack)-1, 0, -1):
            if self.stack[index].tag == tag:
                del self.stack[index:]
                break

    def handle_data(self, value):
        self.stack[-1].children.append(value)


def _html(ctx, url):
    response = ctx.fetch(url, headers={"Accept": "text/html"}, follow_redirects=False)
    failure = http_failure(response)
    if failure:
        raise failure
    if response.url != url:
        raise Failure("TELEGRAM_REDIRECT", "Changed URL cannot establish this Telegram identity", "unknown", details={"source_id": response.source_id})
    media_type = response.headers.get("content-type", "").split(";", 1)[0].lower().strip()
    if media_type not in {"text/html", "application/xhtml+xml"}:
        raise Failure("TELEGRAM_CONTENT", "Telegram did not return an HTML preview", "unknown", details={"source_id": response.source_id})
    parser = Page()
    parser.feed(response.body.decode("utf-8", "replace"))
    return response, parser.root


def _url(value, base):
    try:
        return web_url(urljoin(base, value))
    except Failure:
        return None


def _links(node, base, maximum):
    output = []
    for item in node.walk():
        if item.tag == "a" and item.attrs.get("href"):
            value = _url(item.attrs["href"], base)
            if value and value not in output:
                output.append(value)
            if len(output) >= maximum:
                break
    return output


def _channel_evidence(page, handle):
    # Telegram's generic contact landing page echoes arbitrary handles. Require
    # an actual public-channel preview link/username rather than that echo.
    channel_name = page.find("tgme_channel_info_header_username")
    if channel_name and channel_name.text().lstrip("@").casefold() == handle.casefold():
        return {"field": "public_channel_username", "expected": handle, "actual": channel_name.text()}
    for node in page.walk():
        if node.has("tgme_page_context_link"):
            link = _url(node.attrs.get("href", ""), "https://t.me/")
            if link and urlsplit(link).hostname == "t.me" and urlsplit(link).scheme == "https" and urlsplit(link).path.rstrip("/").casefold() == "/s/" + handle.casefold():
                return {"field": "public_preview_link", "expected": "https://t.me/s/" + handle, "actual": link}
    return None


def _message(node, handle, source_id, maximum):
    identifier = node.attrs.get("data-post", "")
    match = re.fullmatch(r"([A-Za-z][A-Za-z0-9_]{3,31})/([1-9][0-9]{0,9})", identifier)
    if not match or match[1].casefold() != handle.casefold():
        return None
    post_id = int(match[2])
    if post_id > 2147483647:
        return None
    canonical = "https://t.me/" + handle + "/" + str(post_id)
    text = node.find("tgme_widget_message_text")
    views = node.find("tgme_widget_message_views")
    timestamp = next((item.attrs.get("datetime") for item in node.walk() if item.tag == "time" and item.attrs.get("datetime")), None)
    media = []
    for item in node.walk():
        value, kind = None, None
        if item.tag in {"video", "audio"} and item.attrs.get("src"):
            value, kind = _url(item.attrs["src"], canonical), item.tag
        elif item.has("tgme_widget_message_photo_wrap"):
            match_url = re.search(r"url\(['\"]?([^)'\"]+)", item.attrs.get("style", ""))
            if match_url:
                value, kind = _url(match_url[1], canonical), "image"
        if value and len(media) < maximum and not any(record["url"] == value for record in media):
            media.append({"kind": kind, "url": value, "downloaded": False})
    return {"message_id": post_id, "url": canonical, "channel": handle, "text": text.text() if text else "",
            "text_truncated": bool(text and len(text.text(_TEXT_LIMIT+1)) > _TEXT_LIMIT),
            "timestamp_observed": timestamp, "views_observed": views.text(100) if views else None,
            "outbound_links": _links(text or node, canonical, maximum), "media": media,
            "source_ids": [source_id]}


def _graph(messages, source_id):
    entities, relationships = [], []
    for message in messages:
        url = message["url"]
        entities.append({"type": "telegram_post", "value": url, "source_ids": [source_id]})
        for link in message["outbound_links"]:
            entities.append({"type": "url", "value": link, "source_ids": [source_id]})
            relationships.append({"source": {"type": "telegram_post", "value": url}, "relation": "links_to",
                                  "target": {"type": "url", "value": link}, "source_ids": [source_id]})
    return entities, relationships


def telegram_profile(ctx, target):
    handle, _, _ = normalize_target(target)
    canonical = "https://t.me/" + handle
    response, page = _html(ctx, canonical)
    evidence = _channel_evidence(page, handle)
    title = page.find("tgme_page_title") or page.find("tgme_channel_info_header_title")
    description = page.find("tgme_page_description") or page.find("tgme_channel_info_description")
    extra = page.find("tgme_page_extra")
    data = {"username": handle, "url": canonical, "title": title.text(500) if title else None,
            "description": description.text(2000) if description else None,
            "audience_observed": extra.text(300) if extra else None, "source_ids": [response.source_id],
            "evidence": [evidence] if evidence else [], "visibility": "public_web_preview",
            "identity_attribution": "not_established", "profile_status": "verified_public_channel" if evidence else "unverified",
            "scope": "A channel preview does not establish an individual's identity or account ownership"}
    if not evidence:
        return result(data, status="unknown", errors=[Failure("TELEGRAM_PROFILE_UNVERIFIED", "No matching public-channel preview evidence; generic contact pages do not establish account existence", "unknown").as_dict()])
    return result(data, entities=[{"type": "telegram_channel", "value": canonical, "source_ids": [response.source_id]}])


def telegram_messages(ctx, target, limit=20, before=None, query=None):
    handle, post, _ = normalize_target(target)
    selected = _limit(ctx, limit)
    needle = _query(query)
    if before is not None and (isinstance(before, bool) or not isinstance(before, int) or not 1 <= before <= 2147483647):
        raise Failure("INVALID_BEFORE", "before must be a positive 32-bit message ID", "invalid")
    before = before if before is not None else post
    url = "https://t.me/s/" + handle + ("?" + urlencode({"before": before}) if before is not None else "")
    response, page = _html(ctx, url)
    found = []
    for node in page.walk():
        if "data-post" in node.attrs:
            message = _message(node, handle, response.source_id, ctx.max_results)
            if message and (before is None or message["message_id"] < before) and (needle is None or needle in message["text"].casefold()):
                found.append(message)
    found = sorted({message["message_id"]: message for message in found}.values(), key=lambda message: message["message_id"], reverse=True)
    evidence = _channel_evidence(page, handle)
    if not found and not evidence:
        raise Failure("TELEGRAM_PREVIEW_UNVERIFIED", "Public channel preview was unavailable or did not match the requested handle", "unknown", details={"source_id": response.source_id})
    messages = found[:selected]
    older = next((_url(node.attrs["href"], url) for node in page.walk() if node.tag == "link" and node.attrs.get("rel") == "prev" and node.attrs.get("href")), None)
    if older:
        parsed = urlsplit(older)
        if parsed.hostname != "t.me" or parsed.path.casefold() != "/s/" + handle.casefold():
            older = None
    data = {"username": handle, "url": url, "messages": messages, "returned": len(messages), "matching_in_page": len(found),
            "limit": selected, "before": before, "query": query, "query_scope": "returned_preview_page_only",
            "truncated": len(found) > selected, "next_before": parse_qs(urlsplit(older).query).get("before", [None])[0] if older else None,
            "history_complete": False, "scope": "One public preview page; no account, group join, or full-history collection",
            "source_ids": [response.source_id]}
    entities, relationships = _graph(messages, response.source_id)
    return result(data, status="ok" if messages else "not_found", entities=entities, relationships=relationships)


def _post(ctx, target):
    handle, identifier, canonical = normalize_target(target, require_post=True)
    response, page = _html(ctx, canonical + "?embed=1")
    message = next((_message(node, handle, response.source_id, ctx.max_results) for node in page.walk()
                    if node.attrs.get("data-post", "").casefold() == (handle + "/" + str(identifier)).casefold()), None)
    if not message:
        error = page.find("tgme_widget_message_error")
        if error and error.text().casefold() in {"post not found", "message not found"}:
            raise Failure("TELEGRAM_POST_NOT_FOUND", "Public embed returned its missing-post message", "not_found", details={"source_id": response.source_id})
        raise Failure("TELEGRAM_POST_UNVERIFIED", "Embed did not contain the requested channel/message identity", "unknown", details={"source_id": response.source_id})
    return canonical, message, response


def telegram_post(ctx, target):
    canonical, message, response = _post(ctx, target)
    entities, relationships = _graph([message], response.source_id)
    return result({"url": canonical, "message": message, "visibility": "public_web_embed", "source_ids": [response.source_id]},
                  entities=entities, relationships=relationships)


def _export_text(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "".join(item if isinstance(item, str) else item.get("text", "") if isinstance(item, dict) and isinstance(item.get("text"), str) else "" for item in value)
    return ""


def _observed_text(value, maximum):
    return value[:maximum] if isinstance(value, str) else None


def _positive_export_id(value):
    return isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= 9223372036854775807


def _observed_chat_id(value):
    if isinstance(value, int) and not isinstance(value, bool) and -9223372036854775808 <= value <= 9223372036854775807:
        return value
    if isinstance(value, str) and len(value) <= 128:
        return value
    return None


def telegram_export(ctx, target, limit=50, query=None):
    selected, needle = _limit(ctx, limit), _query(query)
    metadata, body = _read_file(ctx, target, keep_body=True)
    try:
        payload = strict_json(body.decode("utf-8-sig"))
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise Failure("INVALID_TELEGRAM_EXPORT", "Expected a Telegram Desktop JSON export", "invalid") from exc
    chats = [payload] if isinstance(payload, dict) and isinstance(payload.get("messages"), list) else payload.get("chats", {}).get("list") if isinstance(payload, dict) and isinstance(payload.get("chats"), dict) else None
    if not isinstance(chats, list) or len(chats) > 1000 or not all(isinstance(chat, dict) and isinstance(chat.get("messages"), list) for chat in chats):
        raise Failure("INVALID_TELEGRAM_EXPORT", "Expected messages[] or chats.list[].messages[]", "invalid")
    messages, total, matched, invalid = [], 0, 0, 0
    for chat_index, chat in enumerate(chats):
        for item in chat["messages"]:
            total += 1
            if not isinstance(item, dict) or not _positive_export_id(item.get("id")):
                invalid += 1
                continue
            if item.get("type") != "message":
                continue
            text = _export_text(item.get("text"))
            if needle is not None and needle not in text.casefold():
                continue
            matched += 1
            if len(messages) >= selected:
                continue
            timestamp = _observed_text(item.get("date"), 128)
            unix = item.get("date_unixtime")
            if isinstance(unix, str) and re.fullmatch(r"[0-9]{1,12}", unix):
                try:
                    timestamp = datetime.fromtimestamp(int(unix), timezone.utc).isoformat().replace("+00:00", "Z")
                except (ValueError, OSError, OverflowError):
                    pass
            messages.append({"message_id": item["id"], "chat_index": chat_index, "chat_id_observed": _observed_chat_id(chat.get("id")),
                "chat_name_observed": _observed_text(chat.get("name"), 500), "chat_type_observed": _observed_text(chat.get("type"), 80),
                "text": text[:_TEXT_LIMIT], "text_truncated": len(text) > _TEXT_LIMIT, "timestamp_observed": timestamp,
                "sender_label_observed": _observed_text(item.get("from"), 500),
                "reply_to_message_id": item.get("reply_to_message_id") if _positive_export_id(item.get("reply_to_message_id")) else None,
                "media_present": any(key in item for key in ["photo", "file", "media_type"]),
                "source_ids": [metadata["source_id"]]})
    data = {"file": metadata, "messages": messages, "returned": len(messages), "messages_scanned": total,
            "matching_messages": matched, "chat_count": len(chats), "limit": selected, "query": query,
            "truncated": matched > selected, "invalid_records": invalid, "network_requests": 0,
            "scope": "Local export observations only; no remote verification, uploads, or identity inference"}
    errors = [Failure("TELEGRAM_EXPORT_RECORDS", f"Skipped {invalid} malformed record(s)", "partial").as_dict()] if invalid else []
    return result(data, status="partial" if invalid else "ok", errors=errors,
                  entities=[{"type": "file_sha256", "value": metadata["sha256"], "source_ids": [metadata["source_id"]]}])


def bellingcat_telegram(ctx, target, download_media=False, max_media=3):
    if not isinstance(download_media, bool) or isinstance(max_media, bool) or not isinstance(max_media, int) or not 1 <= max_media <= 10:
        raise Failure("INVALID_MEDIA_OPTIONS", "download_media must be boolean and max_media must be 1-10", "invalid")
    normalize_target(target, require_post=True)
    from .telegramhelper import UPSTREAM, run_extractor
    if importlib.util.find_spec("bs4") is None:
        raise Failure("BELLINGCAT_DEPENDENCY", "Install Fieldkit's telegram extra (beautifulsoup4) to invoke the adapted Bellingcat extractor", "unavailable",
                      details={"dependency": "beautifulsoup4>=4.12,<5", "upstream": UPSTREAM})
    canonical, message, response = _post(ctx, target)
    extraction = run_extractor(ctx, canonical, response, download_media=download_media, max_media=min(max_media, ctx.max_results))
    entities, relationships = _graph([message], response.source_id)
    data = {"url": canonical, "message": message, "extraction": extraction,
            "source_ids": [response.source_id], "operation": "capture_media" if download_media else "extract_media_references"}
    return result(data, status="partial" if extraction["errors"] else "ok", errors=extraction["errors"],
                  entities=entities, relationships=relationships)
