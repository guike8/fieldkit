"""Bounded Telegram preview/export and real adapted-extractor fixtures."""
from __future__ import annotations
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fieldkit_core.common import Failure, Response
from fieldkit_core import telegram

CHANNEL = "testchannel"
BASE = "https://t.me/" + CHANNEL
MEDIA = "https://cdn1.telesco.pe/file/photo.jpg"


def message(identifier=123, channel=CHANNEL, *, text="A public fixture", media=True):
    photo = '<a class="tgme_widget_message_photo_wrap" style="background-image:url(\'' + MEDIA + '\')"></a>' if media else ""
    return ('<div class="tgme_widget_message js-widget_message" data-post="' + channel + '/' + str(identifier) + '">'
            '<div class="tgme_widget_message_text">' + text + '<a href="https://example.org/report">source</a></div>' + photo +
            '<span class="tgme_widget_message_views">1.2K</span><time datetime="2026-11-11T00:00:00+00:00">00:42</time></div>')


PROFILE = ('<html><div class="tgme_page_title">Fixture Channel</div><div class="tgme_page_extra">42 subscribers</div>'
           '<div class="tgme_page_description">An evidence fixture.</div><a class="tgme_page_context_link" href="/s/testchannel">Preview channel</a></html>')
FEED = '<div class="tgme_channel_info_header_username">@testchannel</div><link rel="prev" href="/s/testchannel?before=121">' + message(123) + message(124, text="Another message")


class FakeContext:
    timeout = 2
    max_results = 50
    max_file_bytes = 1024 * 1024

    def __init__(self, mapping=None):
        self.mapping = mapping or {}
        self.calls = []
        self.sources = []

    def fetch(self, url, headers=None, follow_redirects=True):
        self.calls.append((url, headers, follow_redirects))
        value = self.mapping[url]
        if isinstance(value, Failure):
            raise value
        if isinstance(value, Response):
            return value
        return Response(url, 200, {"content-type": "text/html; charset=utf-8"}, value.encode(), "source-page")

    def add_local_source(self, path, digest, size, media_type):
        self.sources.append({"path": path, "sha256": digest, "bytes": size, "media_type": media_type})
        return "source-local"


class TargetTests(unittest.TestCase):
    def test_target_forms_normalize_to_fixed_public_host(self):
        for value in ("@testchannel", "testchannel", BASE, "https://telegram.me/testchannel", "https://t.me/s/testchannel"):
            self.assertEqual(telegram.normalize_target(value), (CHANNEL, None, BASE))
        self.assertEqual(telegram.normalize_target(BASE + "/123?single&embed=1", require_post=True), (CHANNEL, 123, BASE + "/123"))

    def test_private_actions_foreign_hosts_and_injection_rejected(self):
        for value in ("https://t.me/+invite", "https://t.me/joinchat/hash", "https://t.me/c/123/456", "https://evil.example/testchannel",
                      "https://user:secret@t.me/testchannel", "https://t.me:8443/testchannel", "https://t.me/testchannel/0",
                      "https://t.me/testchannel/2147483648", "../testchannel", "test channel", "testchannel\n", "https://t.me/share"):
            with self.subTest(value=value), self.assertRaises(Failure):
                telegram.normalize_target(value)

    def test_exact_post_required(self):
        with self.assertRaises(Failure) as caught:
            telegram.normalize_target(CHANNEL, require_post=True)
        self.assertEqual(caught.exception.code, "TELEGRAM_POST_REQUIRED")


class PublicPreviewTests(unittest.TestCase):
    def test_profile_requires_public_channel_evidence(self):
        ctx = FakeContext({BASE: PROFILE})
        report = telegram.telegram_profile(ctx, "@testchannel")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["profile_status"], "verified_public_channel")
        self.assertEqual(report["data"]["audience_observed"], "42 subscribers")
        self.assertFalse(ctx.calls[0][2])
        self.assertEqual(report["entities"][0]["source_ids"], ["source-page"])

    def test_generic_echo_contact_page_is_not_a_match(self):
        body = '<meta property="al:ios:url" content="tg://resolve?domain=testchannel"><div class="tgme_page_title">Contact @testchannel</div>'
        report = telegram.telegram_profile(FakeContext({BASE: body}), CHANNEL)
        self.assertEqual(report["status"], "unknown")
        self.assertEqual(report["entities"], [])

    def test_foreign_preview_link_cannot_verify_channel(self):
        body = PROFILE.replace('href="/s/testchannel"', 'href="https://evil.example/s/testchannel"')
        self.assertEqual(telegram.telegram_profile(FakeContext({BASE: body}), CHANNEL)["status"], "unknown")

    def test_feed_is_bounded_sorted_and_exposes_pagination(self):
        ctx = FakeContext({"https://t.me/s/testchannel": FEED})
        report = telegram.telegram_messages(ctx, CHANNEL, limit=1)
        self.assertEqual(report["status"], "ok")
        data = report["data"]
        self.assertEqual([item["message_id"] for item in data["messages"]], [124])
        self.assertTrue(data["truncated"])
        self.assertEqual(data["next_before"], "121")
        self.assertFalse(data["history_complete"])
        self.assertEqual(len(ctx.calls), 1)

    def test_feed_filter_only_claims_current_page(self):
        url = "https://t.me/s/testchannel?before=124"
        ctx = FakeContext({url: FEED})
        report = telegram.telegram_messages(ctx, CHANNEL, before=124, query="public")
        self.assertEqual([item["message_id"] for item in report["data"]["messages"]], [123])
        self.assertEqual(report["data"]["query_scope"], "returned_preview_page_only")

    def test_feed_zero_query_matches_is_scoped_negative(self):
        report = telegram.telegram_messages(FakeContext({"https://t.me/s/testchannel": FEED}), CHANNEL, query="absent-string")
        self.assertEqual(report["status"], "not_found")
        self.assertFalse(report["data"]["history_complete"])

    def test_wrong_channel_posts_do_not_verify_feed(self):
        with self.assertRaises(Failure) as caught:
            telegram.telegram_messages(FakeContext({"https://t.me/s/testchannel": message(channel="otherchannel")}), CHANNEL)
        self.assertEqual(caught.exception.status, "unknown")

    def test_post_exact_identity_and_media_references(self):
        ctx = FakeContext({BASE + "/123?embed=1": message()})
        report = telegram.telegram_post(ctx, BASE + "/123")
        self.assertEqual(report["data"]["message"]["message_id"], 123)
        self.assertEqual(report["data"]["message"]["media"][0]["url"], MEDIA)
        self.assertFalse(report["data"]["message"]["media"][0]["downloaded"])
        self.assertTrue(any(edge["relation"] == "links_to" for edge in report["relationships"]))

    def test_wrong_post_identity_is_unresolved(self):
        with self.assertRaises(Failure) as caught:
            telegram.telegram_post(FakeContext({BASE + "/123?embed=1": message(124)}), BASE + "/123")
        self.assertEqual(caught.exception.status, "unknown")

    def test_recognized_missing_post_and_access_denial(self):
        with self.assertRaises(Failure) as caught:
            telegram.telegram_post(FakeContext({BASE + "/123?embed=1": '<div class="tgme_widget_message_error">Post not found</div>'}), BASE + "/123")
        self.assertEqual(caught.exception.status, "not_found")
        denied = Response(BASE, 403, {"content-type": "text/html"}, b"denied", "denied-source")
        with self.assertRaises(Failure) as caught:
            telegram.telegram_profile(FakeContext({BASE: denied}), CHANNEL)
        self.assertEqual(caught.exception.status, "blocked")

    def test_limits_types_and_html_complexity(self):
        for kwargs in ({"limit": True}, {"limit": 0}, {"before": True}, {"before": 0}, {"query": "\n"}):
            with self.subTest(kwargs=kwargs), self.assertRaises(Failure):
                telegram.telegram_messages(FakeContext(), CHANNEL, **kwargs)
        with self.assertRaises(Failure):
            telegram.telegram_profile(FakeContext({BASE: "<div>" * 130}), CHANNEL)


class ExportTests(unittest.TestCase):
    def export(self, payload, **options):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps(payload))
            ctx = FakeContext()
            report = telegram.telegram_export(ctx, str(path), **options)
            self.assertEqual(ctx.calls, [])
            self.assertEqual(len(ctx.sources), 1)
            return report

    def test_rich_text_export_local_source_and_bound(self):
        report = self.export({"id": 42, "name": "Public channel", "type": "public_channel", "messages": [
            {"id": 1, "type": "message", "text": ["Hello ", {"type": "bold", "text": "world"}], "date_unixtime": "0"},
            {"id": 2, "type": "message", "text": "Next"}]}, limit=1)
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["messages"][0]["text"], "Hello world")
        self.assertEqual(report["data"]["messages"][0]["timestamp_observed"], "1970-01-01T00:00:00Z")
        self.assertTrue(report["data"]["truncated"])
        self.assertEqual(report["data"]["network_requests"], 0)

    def test_archive_chats_export_query(self):
        report = self.export({"chats": {"list": [{"id": 1, "messages": [{"id": 3, "type": "message", "text": "Needle"}]}]}}, query="needle")
        self.assertEqual(report["data"]["returned"], 1)
        self.assertEqual(report["data"]["chat_count"], 1)

    def test_malformed_records_are_partial(self):
        report = self.export({"messages": [None, {"id": True, "type": "message"}, {"id": 2, "type": "service"}]})
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["invalid_records"], 2)

    def test_unrelated_json_is_not_accepted(self):
        with self.assertRaises(Failure):
            self.export({"unrelated": []})

    def test_export_metadata_cannot_amplify_nested_or_oversized_fields(self):
        report = self.export({"id": {"nested": "x" * 100000}, "name": {"also_nested": "y" * 100000},
                              "type": "t" * 10000, "messages": [
            {"id": index, "type": "message", "text": "bounded", "date": "d" * 10000,
             "from": "s" * 10000, "reply_to_message_id": 2 ** 80} for index in range(1, 11)]})
        self.assertLess(len(json.dumps(report)), 25000)
        for item in report["data"]["messages"]:
            self.assertIsNone(item["chat_id_observed"])
            self.assertIsNone(item["chat_name_observed"])
            self.assertLessEqual(len(item["chat_type_observed"]), 80)
            self.assertLessEqual(len(item["sender_label_observed"]), 500)
            self.assertLessEqual(len(item["timestamp_observed"]), 128)
            self.assertIsNone(item["reply_to_message_id"])

    def test_export_message_ids_are_positive_bounded_integers(self):
        report = self.export({"id": "channel-id", "type": {"not": "scalar"}, "messages": [
            {"id": identifier, "type": "message", "text": "fixture"} for identifier in [0, -1, 2 ** 80, True, 7]]})
        self.assertEqual(report["data"]["invalid_records"], 4)
        self.assertEqual([item["message_id"] for item in report["data"]["messages"]], [7])
        self.assertEqual(report["data"]["messages"][0]["chat_id_observed"], "channel-id")
        self.assertIsNone(report["data"]["messages"][0]["chat_type_observed"])


class BellingcatTests(unittest.TestCase):
    def test_missing_optional_dependency_is_noninteractive_and_no_network(self):
        ctx = FakeContext()
        with patch.object(telegram.importlib.util, "find_spec", return_value=None):
            with self.assertRaises(Failure) as caught:
                telegram.bellingcat_telegram(ctx, BASE + "/123")
        self.assertEqual(caught.exception.status, "unavailable")
        self.assertEqual(ctx.calls, [])

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_real_adapted_upstream_algorithm_metadata_only(self):
        ctx = FakeContext({BASE + "/123?embed=1": message()})
        report = telegram.bellingcat_telegram(ctx, BASE + "/123")
        extraction = report["data"]["extraction"]
        self.assertTrue(extraction["extractor_invoked"])
        self.assertEqual(extraction["upstream"]["commit"], "0507bf21793c2d3621a31295a7e166c8bfcec3e4")
        self.assertEqual(extraction["media"][0]["url"], MEDIA)
        self.assertFalse(extraction["media"][0]["captured"])
        self.assertEqual(len(ctx.calls), 1)

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_explicit_media_capture_uses_bounded_context(self):
        binary = b"\xff\xd8\xfffixture-image"
        ctx = FakeContext({BASE + "/123?embed=1": message(), MEDIA: Response(MEDIA, 200, {"content-type": "image/jpeg"}, binary, "source-media")})
        report = telegram.bellingcat_telegram(ctx, BASE + "/123", download_media=True)
        capture = report["data"]["extraction"]["media"][0]
        self.assertTrue(capture["captured"])
        self.assertEqual(capture["sha256"], hashlib.sha256(binary).hexdigest())
        self.assertEqual(capture["source_ids"], ["source-media"])
        self.assertEqual(len(ctx.calls), 2)
        self.assertFalse(ctx.calls[1][2])

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_failed_media_capture_is_partial_not_saved(self):
        ctx = FakeContext({BASE + "/123?embed=1": message(), MEDIA: Failure("RESPONSE_TOO_LARGE", "bounded")})
        report = telegram.bellingcat_telegram(ctx, BASE + "/123", download_media=True)
        self.assertEqual(report["status"], "partial")
        self.assertFalse(report["data"]["extraction"]["media"][0]["captured"])
        self.assertFalse(report["data"]["extraction"]["extractor_returned_media"])

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_text_only_post_does_not_claim_media_capture(self):
        ctx = FakeContext({BASE + "/123?embed=1": message(media=False)})
        report = telegram.bellingcat_telegram(ctx, BASE + "/123")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["extraction"]["media"], [])
        self.assertFalse(report["data"]["extraction"]["extractor_returned_media"])

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_foreign_media_host_is_typed_unresolved(self):
        body = message().replace(MEDIA, "https://unrelated.example/file.jpg")
        ctx = FakeContext({BASE + "/123?embed=1": body})
        with self.assertRaises(Failure) as caught:
            telegram.bellingcat_telegram(ctx, BASE + "/123", download_media=True)
        self.assertEqual(caught.exception.code, "UNEXPECTED_MEDIA_HOST")
        self.assertEqual(caught.exception.status, "unknown")
        self.assertEqual(len(ctx.calls), 1)

    @unittest.skipUnless(importlib.util.find_spec("bs4"), "Optional Beautiful Soup not installed")
    def test_changed_upstream_media_markup_is_typed_unresolved(self):
        body = message().replace(' style="background-image:url(\'' + MEDIA + '\')"', "")
        with self.assertRaises(Failure) as caught:
            telegram.bellingcat_telegram(FakeContext({BASE + "/123?embed=1": body}), BASE + "/123")
        self.assertEqual(caught.exception.code, "BELLINGCAT_SCHEMA_CHANGED")
        self.assertEqual(caught.exception.status, "unknown")


if __name__ == "__main__":
    unittest.main()
