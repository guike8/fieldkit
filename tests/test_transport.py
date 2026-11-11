"""Transport invariants, using mocks only: no live DNS or sockets."""
import hashlib
import json
import socket
import ssl
import unittest
from unittest.mock import Mock, patch

from fieldkit_core.common import Failure, Response, domain_name, web_url
from fieldkit_core import transport


class FakeResponse:
    def __init__(self, status=200, body=b'{}', headers=None):
        self.status, self.body, self.offset = status, body, 0
        self.headers = headers or {"Content-Type": "application/json"}

    def getheaders(self):
        return list(self.headers.items())

    def read1(self, count):
        value = self.body[self.offset:self.offset + count]
        self.offset += len(value)
        return value


class FakeConnection:
    def __init__(self, response):
        self.response = response
        self.sock = Mock()
        self.requests = []
        self.closed = False

    def request(self, method, path, headers):
        self.requests.append((method, path, dict(headers)))

    def getresponse(self):
        return self.response

    def close(self):
        self.closed = True


class URLAndDomainTests(unittest.TestCase):
    def test_domain_idna_normalization(self):
        self.assertEqual(domain_name("BÜCHER.example."), "xn--bcher-kva.example")
        for invalid in ("", ".example", "a..example", "-a.example", "a-.example", "a/b.example",
                        "\ud800.example", "127.0.0.1", "a" * 64 + ".example"):
            with self.subTest(value=repr(invalid)), self.assertRaises(Failure):
                domain_name(invalid)

    def test_url_credentials_controls_and_schemes_rejected(self):
        invalid = ("https://user:password@example.com/", "https://user@example.com/", "file:///etc/passwd",
                   "javascript:alert(1)", "https://example.com/a\nb", "https://example.com/a b",
                   "https://example.com/\\evil", "https://example.com:65536/", "https://example.com/\x00")
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(Failure):
                web_url(value)

    def test_port_zero_not_silently_retargeted(self):
        with self.assertRaises(Failure):
            web_url("https://example.com:0/")

    def test_url_normalization_retains_valid_port_and_drops_fragment(self):
        self.assertEqual(web_url("https://BÜCHER.example:8443/path?q=a%20b#fragment"),
                         "https://xn--bcher-kva.example:8443/path?q=a%20b")
        self.assertEqual(web_url("https://[2001:4860:4860::8888]/"), "https://[2001:4860:4860::8888]/")


class PublicConnectionTests(unittest.TestCase):
    @staticmethod
    def record(address, family=socket.AF_INET):
        sockaddr = (address, 443) if family == socket.AF_INET else (address, 443, 0, 0)
        return family, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", sockaddr

    def test_private_reserved_and_mapped_addresses_rejected(self):
        for address in ("127.0.0.1", "10.0.0.1", "192.168.1.1", "169.254.169.254", "0.0.0.0",
                        "100.64.0.1", "192.0.2.1", "::1", "fc00::1", "fe80::1", "::ffff:127.0.0.1"):
            family = socket.AF_INET6 if ":" in address else socket.AF_INET
            with self.subTest(address=address), patch.object(transport.socket, "getaddrinfo", return_value=[self.record(address, family)]):
                with self.assertRaises(Failure) as caught:
                    transport.resolve_public("untrusted.example", 443)
                self.assertEqual(caught.exception.code, "NON_PUBLIC_TARGET")

    def test_mixed_public_private_dns_answer_blocks_all(self):
        records = [self.record("8.8.8.8"), self.record("127.0.0.1")]
        with patch.object(transport.socket, "getaddrinfo", return_value=records), \
             patch.object(transport.socket, "socket") as socket_factory:
            with self.assertRaises(Failure):
                transport.connect_public("untrusted.example", 443, 1)
            socket_factory.assert_not_called()

    def test_public_records_deduplicated(self):
        record = self.record("8.8.8.8")
        with patch.object(transport.socket, "getaddrinfo", return_value=[record, record]):
            self.assertEqual(len(transport.resolve_public("example.com", 443)), 1)

    def test_socket_connects_pinned_address_without_second_dns_lookup(self):
        connected = Mock()
        with patch.object(transport.socket, "getaddrinfo", return_value=[self.record("8.8.8.8")]) as resolver, \
             patch.object(transport.socket, "socket", return_value=connected):
            self.assertIs(transport.connect_public("example.com", 443, 2), connected)
            connected.connect.assert_called_once_with(("8.8.8.8", 443))
            resolver.assert_called_once()

    def test_failed_address_falls_back_only_to_prevalidated_addresses(self):
        first, second = Mock(), Mock()
        first.connect.side_effect = OSError("unreachable")
        with patch.object(transport.socket, "getaddrinfo", return_value=[self.record("8.8.8.8"), self.record("1.1.1.1")]), \
             patch.object(transport.socket, "socket", side_effect=[first, second]):
            self.assertIs(transport.connect_public("example.com", 443, 2), second)
            first.close.assert_called_once()
            second.connect.assert_called_once_with(("1.1.1.1", 443))

    def test_tls_verifies_original_hostname_on_pinned_socket(self):
        raw, secure, context = Mock(), Mock(), Mock()
        context.wrap_socket.return_value = secure
        with patch.object(transport, "connect_public", return_value=raw) as connect, \
             patch.object(transport.ssl, "create_default_context", return_value=context):
            connection = transport.PinnedConnection("example.com", 443, 2, True)
            connection.connect()
            connect.assert_called_once_with("example.com", 443, 2)
            context.wrap_socket.assert_called_once_with(raw, server_hostname="example.com")
            self.assertIs(connection.sock, secure)

    def test_tls_failure_closes_raw_socket(self):
        raw, context = Mock(), Mock()
        context.wrap_socket.side_effect = ssl.SSLError("bad cert")
        with patch.object(transport, "connect_public", return_value=raw), \
             patch.object(transport.ssl, "create_default_context", return_value=context):
            with self.assertRaises(ssl.SSLError):
                transport.PinnedConnection("example.com", 443, 2, True).connect()
            raw.close.assert_called_once()


class FetchTests(unittest.TestCase):
    def fetch_with(self, responses, *, context=None, url="https://example.com/", headers=None, follow=True):
        context = context or transport.Context()
        connections = [FakeConnection(response) for response in responses]
        with patch.object(transport, "PinnedConnection", side_effect=connections) as constructor:
            result = context.fetch(url, headers=headers, follow_redirects=follow)
        return result, context, connections, constructor

    def test_capture_hash_and_exclude_cookie_headers(self):
        response, ctx, connections, _ = self.fetch_with([FakeResponse(body=b'{"ok":true}', headers={
            "Content-Type": "application/json", "Set-Cookie": "session=secret", "X-Secret": "hidden"})])
        self.assertEqual(response.body, b'{"ok":true}')
        self.assertEqual(ctx.sources[0]["sha256"], hashlib.sha256(response.body).hexdigest())
        self.assertNotIn("set-cookie", ctx.sources[0]["headers"])
        self.assertNotIn("x-secret", ctx.sources[0]["headers"])
        self.assertTrue(connections[0].closed)

    def test_redirect_strips_credentials_and_rechecks_destination(self):
        responses = [FakeResponse(302, headers={"Location": "https://other.example/new"}), FakeResponse()]
        _, ctx, connections, constructor = self.fetch_with(responses, headers={
            "Authorization": "Bearer secret", "Cookie": "session=secret", "X-Subscription-Token": "api-key"})
        self.assertEqual(constructor.call_args_list[1].args[0], "other.example")
        self.assertIn("Authorization", connections[0].requests[0][2])
        self.assertNotIn("Authorization", connections[1].requests[0][2])
        self.assertNotIn("Cookie", connections[1].requests[0][2])
        self.assertNotIn("X-Subscription-Token", connections[1].requests[0][2])
        self.assertEqual(len(ctx.sources), 2)

    def test_private_redirect_cannot_connect(self):
        first = FakeConnection(FakeResponse(302, headers={"Location": "https://127.0.0.1/metadata"}))
        original = transport.PinnedConnection
        def constructor(host, port, timeout, secure):
            return first if host == "example.com" else original(host, port, timeout, secure)
        with patch.object(transport, "PinnedConnection", side_effect=constructor), \
             patch.object(transport.socket, "getaddrinfo", return_value=[PublicConnectionTests.record("127.0.0.1")]), \
             patch.object(transport.socket, "socket") as sockets:
            with self.assertRaises(Failure) as caught:
                transport.Context().fetch("https://example.com/")
            self.assertEqual(caught.exception.code, "NON_PUBLIC_TARGET")
            sockets.assert_not_called()

    def test_https_downgrade_rejected(self):
        with self.assertRaises(Failure) as caught:
            self.fetch_with([FakeResponse(302, headers={"Location": "http://example.com/"})])
        self.assertEqual(caught.exception.code, "INSECURE_REDIRECT")

    def test_redirect_credentials_rejected(self):
        with self.assertRaises(Failure):
            self.fetch_with([FakeResponse(302, headers={"Location": "https://user:secret@other.example/"})])

    def test_redirect_loop_and_limit(self):
        with self.assertRaises(Failure) as caught:
            self.fetch_with([FakeResponse(302, headers={"Location": "/"})])
        self.assertEqual(caught.exception.code, "REDIRECT_LOOP")
        responses = [FakeResponse(302, headers={"Location": f"/{i}"}) for i in range(6)]
        with self.assertRaises(Failure) as caught:
            self.fetch_with(responses)
        self.assertEqual(caught.exception.code, "REDIRECT_LIMIT")

    def test_no_follow_returns_redirect_as_observation(self):
        response, _, connections, _ = self.fetch_with([FakeResponse(302, headers={"Location": "/next"})], follow=False)
        self.assertEqual(response.status, 302)
        self.assertEqual(len(connections), 1)

    def test_request_budget_includes_redirects(self):
        ctx = transport.Context(max_requests=1)
        with self.assertRaises(Failure) as caught:
            self.fetch_with([FakeResponse(302, headers={"Location": "/next"})], context=ctx)
        self.assertEqual(caught.exception.code, "REQUEST_BUDGET")
        self.assertEqual(len(ctx.sources), 1)

    def test_advertised_size_cap(self):
        ctx = transport.Context(max_bytes=5)
        with self.assertRaises(Failure) as caught:
            self.fetch_with([FakeResponse(headers={"Content-Length": "100"})], context=ctx)
        self.assertEqual(caught.exception.code, "RESPONSE_TOO_LARGE")
        self.assertTrue(ctx.sources[0]["truncated"])

    def test_streaming_size_cap(self):
        ctx = transport.Context(max_bytes=5)
        with self.assertRaises(Failure) as caught:
            self.fetch_with([FakeResponse(body=b"123456789")], context=ctx)
        self.assertEqual(caught.exception.code, "RESPONSE_TOO_LARGE")
        self.assertEqual(ctx.sources[0]["bytes"], 5)
        self.assertTrue(ctx.sources[0]["truncated"])

    def test_incomplete_declared_length_is_not_complete_evidence(self):
        ctx = transport.Context()
        with self.assertRaises(Failure):
            self.fetch_with([FakeResponse(body=b"{}", headers={"Content-Type": "application/json", "Content-Length": "100"})], context=ctx)
        self.assertTrue(ctx.sources[0]["truncated"])

    def test_path_unicode_encoded_without_reinterpreting_query(self):
        _, _, connections, _ = self.fetch_with([FakeResponse()], url="https://example.com/ü?q=a%20b&x=1")
        self.assertEqual(connections[0].requests[0][1], "/%C3%BC?q=a%20b&x=1")


class JSONAndEvidenceTests(unittest.TestCase):
    def response(self, body, media="application/json"):
        return Response("https://example.com/", 200, {"content-type": media}, body, "source-test")

    def test_json_media_types(self):
        ctx = transport.Context()
        for media in ("application/json", "application/rdap+json", "application/dns-json", "application/vnd.github+json; charset=utf-8"):
            self.assertEqual(ctx.json(self.response(b'{"ok":true}', media)), {"ok": True})
        for media in ("text/html", "", "text/plain", "garbage+json"):
            with self.subTest(media=media), self.assertRaises(Failure):
                ctx.json(self.response(b"{}", media))

    def test_malformed_nonfinite_duplicate_and_deep_json_rejected(self):
        ctx = transport.Context()
        for body in (b"{", b"\xff", b'{"value":NaN}', b'{"value":Infinity}', b'{"value":1e999}', b'{"id":"wrong","id":"right"}', b"[" * 2000 + b"]" * 2000):
            with self.subTest(body=body[:60]), self.assertRaises(Failure):
                ctx.json(self.response(body))

    def test_inline_evidence_deduplicates_bytes_but_preserves_observations(self):
        ctx = transport.Context()
        first = ctx.add_inline_source("first", b"same")
        second = ctx.add_inline_source("second", b"same")
        self.assertNotEqual(first, second)
        self.assertEqual(len(ctx.sources), 2)
        self.assertEqual(len(ctx.artifacts), 1)
        self.assertTrue(all(item["untrusted_content"] for item in ctx.sources))

    def test_budget_atomic_under_threads(self):
        import concurrent.futures
        ctx = transport.Context(max_requests=3)
        def reserve(_):
            try:
                ctx.reserve_requests()
                return True
            except Failure:
                return False
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            self.assertEqual(sum(pool.map(reserve, range(20))), 3)


if __name__ == "__main__":
    unittest.main()
