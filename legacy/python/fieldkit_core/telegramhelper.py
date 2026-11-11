"""Adapted Bellingcat Auto Archiver Telegram extractor, with bounded Fieldkit I/O.

Upstream 1.3.1, commit 0507bf21793c2d3621a31295a7e166c8bfcec3e4:
https://github.com/bellingcat/auto-archiver/blob/0507bf21793c2d3621a31295a7e166c8bfcec3e4/src/auto_archiver/modules/telegram_extractor/telegram_extractor.py
Original SHA256: 9399a4d2f3715c50ccebd2c19b138ceeb27c29b48df28d6e20edee57d93ae280.

The parser/download selection algorithm below is adapted from that source.
Imports, HTTP/download I/O, and Metadata/Media adapters are changed. This is not
the complete Auto Archiver application. Default operation extracts references;
only download_media=True captures bounded media bodies into Context evidence.

MIT License
Copyright (c) 2021 Stichting Bellingcat

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""
from __future__ import annotations

import hashlib
import html
import logging
import re
from types import SimpleNamespace
from urllib.parse import urlsplit

from .common import Failure, http_failure, web_url

UPSTREAM = {"project": "Bellingcat Auto Archiver", "version": "1.3.1", "license": "MIT",
            "commit": "0507bf21793c2d3621a31295a7e166c8bfcec3e4",
            "source_url": "https://github.com/bellingcat/auto-archiver/blob/0507bf21793c2d3621a31295a7e166c8bfcec3e4/src/auto_archiver/modules/telegram_extractor/telegram_extractor.py",
            "original_sha256": "9399a4d2f3715c50ccebd2c19b138ceeb27c29b48df28d6e20edee57d93ae280",
            "integration": "adapted_pinned_extractor", "full_application": False}


class Metadata(dict):
    def __init__(self, url=""):
        super().__init__()
        self.url, self.netloc = url, urlsplit(url).netloc
        self.media = []

    def get_url(self):
        return self.url

    def set_content(self, content):
        self["content"] = content

    def set_timestamp(self, timestamp):
        self["timestamp"] = timestamp

    def add_media(self, media):
        self.media.append(media)

    def success(self, extractor):
        self["extractor"] = extractor
        return self


class Media(dict):
    def __init__(self, reference):
        super().__init__(reference=reference)

    def set(self, key, value):
        self[key] = value


class BellingcatTelegramExtractor:
    """Pinned upstream selection algorithm with instance-local dependencies."""
    def __init__(self, soup, requests, download):
        self.soup, self.requests, self.download_from_url = soup, requests, download
        self.logger = logging.getLogger("fieldkit.bellingcat.telegram")

    def download(self, item: Metadata):
        url = item.get_url()
        if "t.me" != item.netloc:
            return False
        headers = {"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/81.0.4044.138 Safari/537.36"}
        if url[-8:] != "?embed=1":
            url += "?embed=1"
        t = self.requests.get(url, headers=headers)
        s = self.soup(t.content, "html.parser")
        output = Metadata()
        output.set_content(html.escape(str(t.content)))
        if timestamp := (s.find_all("time") or [{}])[0].get("datetime"):
            output.set_timestamp(timestamp)
        video = s.find("video")
        if video is None:
            image_tags = s.find_all(class_="tgme_widget_message_photo_wrap")
            image_urls = []
            for image in image_tags:
                image_urls += [value.replace("'", "") for value in re.findall(r"url\((.*?)\)", image["style"])]
            if not image_urls:
                return False
            for image_url in image_urls:
                reference = self.download_from_url(image_url)
                if not reference:
                    continue
                output.add_media(Media(reference))
        else:
            reference = self.download_from_url(video.get("src"))
            if not reference:
                return False
            media = Media(reference)
            try:
                duration = s.find_all("time")[0].contents[0]
                duration = float(duration.split(":")[0]) * 60 + float(duration.split(":")[1]) if ":" in duration else float(duration)
                media.set("duration", duration)
            except (IndexError, TypeError, ValueError):
                pass
            output.add_media(media)
        return output.success("telegram")


def run_extractor(ctx, url, response, *, download_media=False, max_media=3):
    try:
        from bs4 import BeautifulSoup
    except ImportError as exc:
        raise Failure("BELLINGCAT_DEPENDENCY", "Install Fieldkit's telegram extra (beautifulsoup4) for the adapted Bellingcat extractor", "unavailable",
                      details={"dependency": "beautifulsoup4>=4.12,<5", "upstream": UPSTREAM}) from exc
    media, errors = [], []
    state = {"truncated": False}

    class Requests:
        def get(self, requested, headers=None):
            if requested != url + "?embed=1":
                raise Failure("EXTRACTOR_PROTOCOL", "Adapted extractor requested an unexpected page")
            return SimpleNamespace(content=response.body)

    def download(value):
        if len(media) >= max_media:
            state["truncated"] = True
            return None
        if not isinstance(value, str) or not value:
            raise Failure("BELLINGCAT_SCHEMA_CHANGED", "Telegram media URL was absent or changed type", "unknown")
        normalized = web_url(value)
        host = (urlsplit(normalized).hostname or "").lower()
        if not (host == "t.me" or host == "telegram.org" or host.endswith(".telegram.org") or host.endswith(".telesco.pe")):
            raise Failure("UNEXPECTED_MEDIA_HOST", "Extractor returned a URL outside Telegram's public media hosts", "unknown")
        entry = {"url": normalized, "captured": False, "source_ids": [response.source_id], "sha256": None}
        media.append(entry)
        if download_media:
            try:
                fetched = ctx.fetch(normalized, headers={"Accept": "image/*,video/*,application/octet-stream"}, follow_redirects=False)
                failure = http_failure(fetched)
                if failure:
                    raise failure
                kind = fetched.headers.get("content-type", "").split(";", 1)[0].lower()
                if not (kind.startswith("image/") or kind.startswith("video/") or kind == "application/octet-stream"):
                    raise Failure("UNEXPECTED_MEDIA_TYPE", "Media host did not return image/video/binary content", "unknown")
                entry.update(captured=True, sha256=hashlib.sha256(fetched.body).hexdigest(), bytes=len(fetched.body),
                             media_type=kind, source_ids=[fetched.source_id])
            except Failure as failure:
                entry["error"] = failure.as_dict()
                errors.append(failure.as_dict())
                return None
        return "sha256:" + entry["sha256"] if entry["captured"] else "reference:" + normalized

    try:
        output = BellingcatTelegramExtractor(BeautifulSoup, Requests(), download).download(Metadata(url))
    except (KeyError, TypeError, AttributeError, IndexError, RecursionError) as failure:
        raise Failure("BELLINGCAT_SCHEMA_CHANGED", "Telegram markup no longer matches the pinned extractor schema", "unknown",
                      details={"source_id": response.source_id, "upstream_commit": UPSTREAM["commit"]}) from failure
    return {"upstream": dict(UPSTREAM), "extractor_invoked": True, "extractor_returned_metadata": bool(output),
            "extractor_returned_media": bool(output and output.media),
            "timestamp": output.get("timestamp") if output else None,
            "media": media, "returned": len(media), "truncated": state["truncated"],
            "download_media": download_media, "errors": errors,
            "body_persistence": "Captured bodies are retained only when the caller saves Context artifacts to a case",
            "adaptations": ["Context-bound HTTP with request/byte limits", "Metadata-only references by default",
                            "Explicit bounded media-body capture", "No arbitrary filesystem writes or application plugins"]}
