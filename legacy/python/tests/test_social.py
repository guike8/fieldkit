from __future__ import annotations

import json
import unittest
from unittest.mock import patch

from fieldkit_core.common import Failure, Response
from fieldkit_core.social import scan_social, social_catalog


class Context:
    max_results = 5

    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def fetch(self, url, headers=None, follow_redirects=True):
        self.calls.append((url, headers, follow_redirects))
        payload, code, content_type = self.responses.pop(0)
        if isinstance(payload, Failure):
            raise payload
        body = payload.encode() if isinstance(payload, str) else json.dumps(payload).encode()
        return Response(url, code, {"content-type": content_type}, body, "src-" + str(len(self.calls)))

    def json(self, response):
        if "json" not in response.headers.get("content-type", ""):
            raise Failure("JSON_TYPE", "Expected JSON")
        try:
            return json.loads(response.body)
        except ValueError as exc:
            raise Failure("JSON_INVALID", "Invalid JSON") from exc


def js(payload, status=200):
    return payload, status, "application/json"


def html(payload, status=200):
    return payload, status, "text/html; charset=utf-8"


def reddit(name="alice", **extra):
    return {"kind": "t2", "data": {"name": name, "id": "ab123", **extra}}


def bluesky(handle="alice.bsky.social", **extra):
    return {"handle": handle, "did": "did:plc:ewvi7nxzyoun6zhxrhs64oiz", **extra}


def mastodon(**extra):
    return {"username": "alice", "acct": "alice", "id": "12345",
            "url": "https://social.example/@alice", **extra}


def steam(name="alice", stable="76561197960287930"):
    payload = json.dumps({"url": "https://steamcommunity.com/id/" + name + "/", "steamid": stable,
                          "personaname": "Display name"})
    return '<html><script type="text/javascript">\ng_rgProfileData = ' + payload + ';\n</script></html>'


REDDIT_MISSING = js({"error": 404, "message": "Not Found"}, 404)
BSKY_MISSING = js({"error": "InvalidRequest", "message": "Profile not found"}, 400)
MASTO_MISSING = js({"error": "Record not found"}, 404)
STEAM_MISSING = html('<div class="error_ctn"><div><h3>The specified profile could not be found.</h3></div></div>')
YOUTUBE_MISSING = html('<title>404 Not Found</title><link rel="shortcut icon" href="https://www.youtube.com/img/favicon.ico">'
                       '<iframe src="/error?src=404&amp;ifr=1&amp;error="></iframe>', 404)


def youtube(handle="alice", stable="UCK8sQmJBp8GCxrOtXWBpyEA"):
    payload = {"metadata": {"channelMetadataRenderer": {"externalId": stable,
                "ownerUrls": ["http://www.youtube.com/@" + handle],
                "vanityChannelUrl": "http://www.youtube.com/@" + handle,
                "title": "Public channel", "description": "Channel bio https://example.com"}}}
    return '<html><script>var ytInitialData = ' + json.dumps(payload) + ';</script></html>'


class SocialTests(unittest.TestCase):
    def profile(self, ctx, handle="alice", site="reddit"):
        return scan_social(ctx, handle, [site])["data"]["profiles"][0]

    def test_catalog_scope(self):
        catalog = social_catalog()
        self.assertEqual({s["id"] for s in catalog["sites"]}, {"reddit", "bluesky", "mastodon", "steam", "youtube"})
        self.assertEqual(catalog["negative_controls"], "required_for_found")

    def test_reddit_positive_has_control_and_explicit_graph_facts(self):
        ctx = Context([js(reddit("Alice", subreddit={"title": "Public display", "public_description": "Site https://example.com"})), REDDIT_MISSING])
        result = scan_social(ctx, "alice", ["reddit"])
        profile = result["data"]["profiles"][0]
        self.assertEqual(profile["status"], "found")
        self.assertEqual(profile["control"], "passed")
        self.assertEqual(profile["negative_control"]["status"], "not_found")
        self.assertEqual(profile["public_metadata"]["outbound_links"], ["https://example.com/"])
        self.assertTrue(result["data"]["complete"])
        self.assertEqual(len(ctx.calls), 2)
        self.assertFalse(ctx.calls[0][2])
        self.assertTrue(all(r["relation"] != "same_person" for r in result["relationships"]))
        self.assertTrue(all(e["source_ids"] for e in result["entities"]))

    def test_wrong_identity_and_missing_stable_id_stay_unknown(self):
        for payload in (reddit("bob"), {"kind": "t2", "data": {"name": "alice"}},
                        {"kind": "t3", "data": {"name": "alice", "id": "ab123"}}):
            ctx = Context([js(payload)])
            self.assertEqual(self.profile(ctx)["status"], "unknown")
            self.assertEqual(len(ctx.calls), 1)

    def test_suspension_and_http_failures_are_not_absence(self):
        self.assertEqual(self.profile(Context([js(reddit(is_suspended=True))]))["status"], "blocked")
        for code, state in ((401, "blocked"), (403, "blocked"), (429, "rate_limited"), (502, "error"), (302, "unknown")):
            with self.subTest(code=code):
                self.assertEqual(self.profile(Context([js(reddit(), code)]))["status"], state)

    def test_html_and_unrecognized_negative_are_unknown(self):
        self.assertEqual(self.profile(Context([html("<h1>Not found</h1>", 404)]))["status"], "unknown")
        self.assertEqual(self.profile(Context([js({"error": 404, "message": "maintenance"}, 404)]))["status"], "unknown")
        self.assertEqual(self.profile(Context([js("not json")]))["status"], "unknown")

    def test_negative_control_cannot_verify_blocked_or_false_positive(self):
        for second in (js({}, 403), js(reddit("hc1234567890abcd"))):
            ctx = Context([js(reddit()), second])
            with patch("fieldkit_core.social.secrets.token_hex", return_value="1234567890abcd"):
                output = scan_social(ctx, "alice", ["reddit"])
            profile = output["data"]["profiles"][0]
            self.assertEqual(profile["status"], "unknown")
            self.assertEqual(profile["control"], "failed")
            self.assertEqual(output["status"], "partial")
            self.assertFalse(output["entities"])
            self.assertFalse(output["relationships"])

    def test_bluesky_bare_handle_has_explicit_namespace(self):
        ctx = Context([js(bluesky(description="Public https://example.org/info")), BSKY_MISSING])
        profile = self.profile(ctx, "alice", "bluesky")
        self.assertEqual(profile["status"], "found")
        self.assertEqual(profile["username"], "alice.bsky.social")
        self.assertIn("actor=alice.bsky.social", ctx.calls[0][0])
        self.assertIn("public AppView", profile["negative_control"]["reason"])

    def test_bluesky_custom_handle_and_did_schema(self):
        ctx = Context([js(bluesky("alice.example")), BSKY_MISSING])
        self.assertEqual(self.profile(ctx, "alice.example", "bluesky")["status"], "found")
        self.assertEqual(self.profile(Context([js(bluesky("bob.bsky.social"))]), "alice", "bluesky")["status"], "unknown")
        self.assertEqual(self.profile(Context([js(bluesky(did="not-a-did"))]), "alice", "bluesky")["status"], "unknown")
        self.assertEqual(self.profile(Context([js({"error": "AccountTakedown"}, 400)]), "alice", "bluesky")["status"], "blocked")

    def test_mastodon_qualified_identity_and_outbound_links(self):
        ctx = Context([js(mastodon(note='<p>Public bio <a href="https://example.org">site</a></p>',
                                   display_name="Public Alice")), MASTO_MISSING])
        profile = self.profile(ctx, "@alice@social.example", "mastodon")
        self.assertEqual(profile["status"], "found")
        self.assertEqual(profile["username"], "alice@social.example")
        self.assertEqual(profile["public_metadata"]["bio"], "Public bio site")
        self.assertEqual(profile["public_metadata"]["outbound_links"], ["https://example.org/"])
        self.assertIn("acct=alice", ctx.calls[0][0])
        self.assertIn("social.example/api/v1/accounts/lookup?acct=hc", ctx.calls[1][0])

    def test_mastodon_no_guessing_and_remote_identity_rejected(self):
        ctx = Context([])
        self.assertEqual(self.profile(ctx, "alice", "mastodon")["status"], "invalid")
        self.assertFalse(ctx.calls)
        for payload in (mastodon(acct="alice@other.example"), mastodon(url="https://other.example/@alice"),
                        mastodon(id="0")):
            self.assertEqual(self.profile(Context([js(payload)]), "alice@social.example", "mastodon")["status"], "unknown")

    def test_steam_structured_identity_and_control(self):
        ctx = Context([html(steam("ALICE")), STEAM_MISSING])
        profile = self.profile(ctx, "alice", "steam")
        self.assertEqual(profile["status"], "found")
        self.assertEqual(profile["control"], "passed")
        self.assertEqual(profile["evidence"][1]["actual"], "76561197960287930")

    def test_steam_wrong_identity_bad_id_and_generic_errors(self):
        for content in (steam("bob"), steam(stable="123"), steam(stable="11111111111111111"),
                        '<h3>The specified profile could not be found.</h3>',
                        '<div class="error_ctn"><h3>Maintenance</h3></div>',
                        '<script>g_rgProfileData = function(){alert(1)};</script>'):
            self.assertEqual(self.profile(Context([html(content)]), "alice", "steam")["status"], "unknown")

    def test_steam_known_missing_profile_is_not_found_without_controls(self):
        ctx = Context([STEAM_MISSING])
        profile = self.profile(ctx, "alice", "steam")
        self.assertEqual(profile["status"], "not_found")
        self.assertEqual(profile["control"], "not_run")
        self.assertEqual(len(ctx.calls), 1)

    def test_youtube_exact_handle_channel_id_and_branded_negative_control(self):
        ctx = Context([html(youtube("ALICE")), YOUTUBE_MISSING])
        profile = self.profile(ctx, "@alice", "youtube")
        self.assertEqual(profile["status"], "found")
        self.assertEqual(profile["control"], "passed")
        self.assertEqual(profile["evidence"][1]["actual"], "UCK8sQmJBp8GCxrOtXWBpyEA")
        self.assertEqual(profile["public_metadata"]["bio"], "Channel bio https://example.com")
        self.assertTrue(ctx.calls[0][0].endswith("/@alice/about?hl=en"))
        self.assertFalse(ctx.calls[0][2])

    def test_youtube_generic_or_unbranded_404_never_proves_absence(self):
        samples = [html("<title>404 Not Found</title>", 404),
                   html('<title>404 Not Found</title><link rel="icon" href="https://other.example/img/favicon.ico">'
                        '<iframe src="/error?src=404&amp;ifr=1"></iframe>', 404),
                   html('<title>404 Not Found</title><link rel="icon" href="https://www.youtube.com/img/favicon.ico">'
                        '<iframe src="https://other.example/error?src=404&amp;ifr=1"></iframe>', 404),
                   (YOUTUBE_MISSING[0], 200, YOUTUBE_MISSING[2])]
        for sample in samples:
            self.assertEqual(self.profile(Context([sample]), "alice", "youtube")["status"], "unknown")

    def test_youtube_wrong_handle_bad_id_and_script_function_are_unknown(self):
        for sample in (youtube("bob"), youtube(stable="not-a-channel-id"),
                       '<script>var ytInitialData = function(){ throw "not JSON"; };</script>',
                       youtube() + youtube()):
            self.assertEqual(self.profile(Context([html(sample)]), "alice", "youtube")["status"], "unknown")

    def test_youtube_positive_control_unresolved_withholds_match(self):
        ctx = Context([html(youtube()), html("<title>temporary challenge</title>")])
        output = scan_social(ctx, "alice", ["youtube"])
        profile = output["data"]["profiles"][0]
        self.assertEqual(profile["status"], "unknown")
        self.assertEqual(profile["control"], "failed")
        self.assertFalse(output["relationships"])

    def test_invalid_and_unknown_sites_no_requests(self):
        ctx = Context([])
        with self.assertRaises(Failure):
            scan_social(ctx, "alice", ["instagram"])
        self.assertFalse(ctx.calls)
        for site in ("reddit", "bluesky", "mastodon", "steam", "youtube"):
            self.assertEqual(self.profile(Context([]), "../evil", site)["status"], "invalid")

    def test_network_failures_are_explicit(self):
        failure = Failure("NETWORK", "TLS validation failed", details={"source_id": "src-failed"})
        ctx = Context([(failure, 0, "")])
        output = scan_social(ctx, "alice", ["reddit"])
        profile = output["data"]["profiles"][0]
        self.assertEqual(output["status"], "error")
        self.assertEqual(profile["status"], "error")
        self.assertEqual(profile["source_ids"], ["src-failed"])


if __name__ == "__main__":
    unittest.main()
