from __future__ import annotations

import json
import os
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from fieldkit_core.common import Failure, Response
from fieldkit_core.web import archive, http, search


class Context:
    def __init__(self, payload=None, *, body=None, status=200, headers=None, url="https://example.com/", maximum=5):
        self.response = Response(url, status, headers or {"content-type": "application/json"},
                                 body if body is not None else json.dumps(payload).encode(), "src-test")
        self.max_results = maximum
        self.calls = []

    def fetch(self, url, headers=None, follow_redirects=True):
        self.calls.append((url, headers, follow_redirects))
        return self.response

    def json(self, response):
        return json.loads(response.body)


class HttpTests(unittest.TestCase):
    def page(self, content, maximum=5, status=200, headers=None):
        return Context(body=content.encode(), maximum=maximum, status=status,
                       headers=headers or {"content-type": "text/html; charset=utf-8"})

    def test_extracts_metadata_links_and_provenance_without_secondary_requests(self):
        ctx = self.page('<title> A &amp; B </title><meta name="description" content="Hi">'
                        '<link rel="canonical" href="/canonical"><a href="/one"> First <b>link</b></a>'
                        '<a href="javascript:alert(1)">bad</a><a href="mailto:a@example.com">mail</a>'
                        '<script>fetch("https://evil.test/")</script>')
        output = http(ctx, "https://example.com")
        self.assertEqual(output["status"], "ok")
        data = output["data"]
        self.assertEqual(data["title"], "A & B")
        self.assertEqual(data["metadata"], [{"name": "description", "content": "Hi"}])
        self.assertEqual(data["canonical_url"], "https://example.com/canonical")
        self.assertEqual(data["links"][0]["text"], "First link")
        self.assertEqual(len(data["links"]), 1)
        self.assertEqual(len(ctx.calls), 1)
        self.assertFalse(data["scripts_executed"])
        self.assertEqual(output["relationships"][0]["source"]["type"], "url")
        self.assertEqual(output["relationships"][0]["source_ids"], ["src-test"])

    def test_link_cap_dedup_and_metadata_cap(self):
        output = http(self.page('<meta name="a" content="a"><meta name="b" content="b">'
                                '<a href="/a">a</a><a href="/a">again</a><a href="/b">b</a>', maximum=1),
                      "https://example.com")
        self.assertEqual(len(output["data"]["links"]), 1)
        self.assertEqual(output["data"]["links_seen"], 3)
        self.assertTrue(output["data"]["links_truncated"])
        self.assertTrue(output["data"]["metadata_truncated"])

    def test_html_base_and_controls(self):
        output = http(self.page('<base href="https://cdn.example.com/dir/"><title>\x1b[31mHello</title>'
                                '<a href="one">one</a>'), "https://example.com")
        self.assertEqual(output["data"]["links"][0]["url"], "https://cdn.example.com/dir/one")
        self.assertNotIn("\x1b", output["data"]["title"])

    def test_json_not_interpreted_as_html_and_sensitive_headers_omitted(self):
        ctx = Context({"title": "<title>bogus</title>"}, headers={"content-type": "application/json",
                      "set-cookie": "secret-session", "server": "test"})
        data = http(ctx, "https://example.com")["data"]
        self.assertIsNone(data["title"])
        self.assertNotIn("set-cookie", data["headers"])
        self.assertEqual(len(data["body_sha256"]), 64)

    def test_http_states(self):
        for code, expected in ((404, "not_found"), (410, "not_found"), (403, "blocked"),
                               (429, "rate_limited"), (502, "error")):
            with self.subTest(code=code):
                self.assertEqual(http(self.page("<title>error</title>", status=code),
                                      "https://example.com")["status"], expected)

    def test_invalid_url_never_fetches(self):
        ctx = Context()
        with self.assertRaises(Failure):
            http(ctx, "file:///etc/passwd")
        self.assertFalse(ctx.calls)


class ArchiveTests(unittest.TestCase):
    fields = ["timestamp", "original", "statuscode", "mimetype", "digest"]
    row = ["20240102030405", "https://example.com/", "200", "text/html", "HASH"]

    def test_capture_and_bounded_exact_query(self):
        ctx = Context([self.fields, self.row])
        output = archive(ctx, "https://example.com/", limit=2)
        query = parse_qs(urlsplit(ctx.calls[0][0]).query)
        self.assertEqual(query["limit"], ["3"])
        self.assertEqual(query["matchType"], ["exact"])
        self.assertEqual(output["status"], "ok")
        self.assertEqual(output["data"]["captures"][0]["captured_at"], "2024-01-02T03:04:05Z")
        self.assertFalse(output["data"]["replays_fetched"])
        self.assertEqual(len(ctx.calls), 1)

    def test_prefix_and_truncation(self):
        ctx = Context([self.fields, self.row, self.row])
        output = archive(ctx, "https://example.com/", limit=1, prefix=True)
        self.assertIn("matchType=prefix", ctx.calls[0][0])
        self.assertTrue(output["data"]["truncated"])
        self.assertEqual(len(output["data"]["captures"]), 1)

    def test_empty_is_limited_index_negative(self):
        for payload in ([], [self.fields]):
            output = archive(Context(payload), "https://example.com/")
            self.assertEqual(output["status"], "not_found")
            self.assertEqual(output["data"]["match_type"], "exact")

    def test_bad_schema_is_error(self):
        for payload in ({"error": "oops"}, [["original"]], ["text"]):
            with self.subTest(payload=payload), self.assertRaises(Failure):
                archive(Context(payload), "https://example.com/")

    def test_invalid_records_not_false_negatives(self):
        for bad in (["20240230030405", *self.row[1:]], [self.row[0], "javascript:bad", *self.row[2:]],
                    [self.row[0], self.row[1], "no", *self.row[3:]], ["short"]):
            output = archive(Context([self.fields, bad]), "https://example.com/")
            self.assertEqual(output["status"], "partial")
            self.assertEqual(output["data"]["invalid_records"], 1)

    def test_invalid_limit_and_rate_limit(self):
        for value in (0, -1, True, 1.5, 10):
            ctx = Context([])
            with self.assertRaises(Failure):
                archive(ctx, "https://example.com/", limit=value)
            self.assertFalse(ctx.calls)
        with self.assertRaises(Failure) as error:
            archive(Context([], status=429), "https://example.com/")
        self.assertEqual(error.exception.status, "rate_limited")


class SearchTests(unittest.TestCase):
    def payload(self, rows=None, **query):
        return {"type": "search", "query": {"original": "test query", **query},
                "web": {"results": [{"url": "https://example.com/", "title": "Title", "description": "Snippet"}]
                         if rows is None else rows}}

    def test_missing_key_is_unavailable_no_network(self):
        ctx = Context()
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(Failure) as error:
            search(ctx, "test query")
        self.assertEqual(error.exception.status, "unavailable")
        self.assertFalse(ctx.calls)

    @patch.dict(os.environ, {"BRAVE_SEARCH_API_KEY": "secret-test-token"})
    def test_key_header_only_no_redirect_and_results(self):
        ctx = Context(self.payload(more_results_available=True))
        output = search(ctx, "test query", limit=2)
        url, headers, redirect = ctx.calls[0]
        self.assertFalse(redirect)
        self.assertEqual(headers["X-Subscription-Token"], "secret-test-token")
        self.assertNotIn("secret-test-token", url)
        self.assertNotIn("secret-test-token", json.dumps(output))
        self.assertEqual(parse_qs(urlsplit(url).query)["spellcheck"], ["false"])
        self.assertEqual(output["status"], "ok")
        self.assertTrue(output["data"]["truncated"])

    @patch.dict(os.environ, {"BRAVE_SEARCH_API_KEY": "test-key"})
    def test_negative_and_partial_results(self):
        self.assertEqual(search(Context(self.payload(rows=[])), "test query")["status"], "not_found")
        self.assertEqual(search(Context(self.payload(rows=[{"url": "javascript:bad", "title": "No"}])),
                                "test query")["status"], "partial")
        self.assertEqual(search(Context(self.payload(altered="different")), "test query")["status"], "partial")

    @patch.dict(os.environ, {"BRAVE_SEARCH_API_KEY": "test-key"})
    def test_schema_and_auth_failures(self):
        with self.assertRaises(Failure):
            search(Context({"error": "oops"}), "test query")
        with self.assertRaises(Failure) as error:
            search(Context(self.payload(), status=401), "test query")
        self.assertEqual(error.exception.status, "blocked")

    def test_invalid_query_and_limit_never_fetch(self):
        for query in ("", "x" * 601, "word " * 76, "test\nquery"):
            ctx = Context()
            with self.assertRaises(Failure):
                search(ctx, query)
            self.assertFalse(ctx.calls)
        ctx = Context()
        with self.assertRaises(Failure):
            search(ctx, "test query", limit=21)
        self.assertFalse(ctx.calls)


if __name__ == "__main__":
    unittest.main()
