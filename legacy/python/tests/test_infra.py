import json
import unittest
from urllib.parse import parse_qs, urlsplit

from fieldkit_core import infra
from fieldkit_core.common import Failure, Response


class FakeContext:
    max_results = 10
    timeout = 10

    def __init__(self, handler):
        self.handler = handler
        self.calls = []
        self.sources = []

    def fetch(self, url, headers=None, follow_redirects=True):
        self.calls.append(url)
        status, body = self.handler(url)
        response = Response(url, status, {"content-type": "application/json"}, json.dumps(body).encode(),
                            "source-" + str(len(self.calls)))
        self.sources.append({"id": response.source_id})
        return response

    def json(self, response):
        return json.loads(response.body)

    def tls_info(self, host, port=443):
        self.calls.append((host, port))
        return {"verified": True, "certificate_sha256": "a" * 64, "source_id": "tls-source"}


def dns_body(url, *, answers=None, status=0):
    query = parse_qs(urlsplit(url).query)
    record_type = query["type"][0]
    return {"Status": status, "Question": [{"name": query["name"][0] + ".", "type": infra.RR_TYPES[record_type]}],
            "Answer": answers or [], "AD": True}


class DNSTests(unittest.TestCase):
    def test_addresses_have_provenance_and_graph(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, answers=[
            {"name": "example.com.", "type": 1, "TTL": 300, "data": "8.8.8.8"}])))
        report = infra.dns(ctx, "EXAMPLE.COM", ["A"])
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["records"][0]["value"], "8.8.8.8")
        self.assertEqual(report["relationships"][0]["relation"], "resolves_to")
        self.assertEqual(report["relationships"][0]["source_ids"], ["source-1"])
        self.assertIn("edns_client_subnet=0.0.0.0%2F0", ctx.calls[0])

    def test_default_types_are_bounded_and_distinct(self):
        ctx = FakeContext(lambda url: (200, dns_body(url)))
        report = infra.dns(ctx, "example.com")
        self.assertEqual(len(ctx.calls), 6)
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["records"], [])

    def test_nxdomain_is_not_found(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, status=3)))
        self.assertEqual(infra.dns(ctx, "missing.example", ["A"])["status"], "not_found")

    def test_servfail_is_error_not_missing(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, status=2)))
        report = infra.dns(ctx, "example.com", ["A"])
        self.assertEqual(report["status"], "error")
        self.assertTrue(report["errors"][0]["retryable"])

    def test_wrong_question_is_unknown(self):
        ctx = FakeContext(lambda url: (200, {"Status": 0, "Question": [{"name": "other.com.", "type": 1}]}))
        self.assertEqual(infra.dns(ctx, "example.com", ["A"])["status"], "unknown")

    def test_rate_limit_remains_unresolved(self):
        ctx = FakeContext(lambda url: (429, {"error": "rate limit"}))
        self.assertEqual(infra.dns(ctx, "example.com", ["A"])["status"], "rate_limited")

    def test_output_cap_is_reported(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, answers=[
            {"name": "example.com.", "type": 1, "TTL": 300, "data": f"8.8.8.{i}"} for i in range(1, 20)])))
        ctx.max_results = 3
        report = infra.dns(ctx, "example.com", ["A"])
        self.assertEqual(len(report["data"]["records"]), 3)
        self.assertEqual(report["data"]["records_total_in_responses"], 19)
        self.assertTrue(report["data"]["records_truncated"])

    def test_invalid_answer_does_not_become_a_record(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, answers=[{"name": "example.com", "type": 1,
                                                                "TTL": True, "data": "8.8.8.8"}])))
        report = infra.dns(ctx, "example.com", ["A"])
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["records"], [])

    def test_ptr_public_ip_uses_reverse_name(self):
        ctx = FakeContext(lambda url: (200, dns_body(url)))
        report = infra.dns(ctx, "8.8.8.8", ["PTR"])
        self.assertEqual(report["data"]["query_name"], "8.8.8.8.in-addr.arpa")
        with self.assertRaises(Failure):
            infra.dns(ctx, "127.0.0.1", ["PTR"])

    def test_underscore_names_and_invalid_types(self):
        ctx = FakeContext(lambda url: (200, dns_body(url)))
        self.assertEqual(infra.dns(ctx, "_dmarc.example.com", ["TXT"])["status"], "ok")
        with self.assertRaises(Failure):
            infra.dns(ctx, "example.com", ["ANY"])


class RDAPTests(unittest.TestCase):
    def test_domain_bootstrap_and_identity(self):
        def handler(url):
            if "data.iana.org" in url:
                return 200, {"services": [[["com"], ["http://insecure.invalid/", "https://registry.example/rdap/"]]]}
            return 200, {"objectClassName": "domain", "ldhName": "EXAMPLE.COM", "handle": "123",
                         "nameservers": [{"ldhName": "NS1.EXAMPLE.COM"}],
                         "events": [{"eventAction": "registration", "eventDate": "1995-08-14T04:00:00Z"}]}
        ctx = FakeContext(handler)
        report = infra.rdap(ctx, "Example.COM")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(ctx.calls[-1], "https://registry.example/rdap/domain/example.com")
        self.assertEqual(report["data"]["nameservers"], ["ns1.example.com"])
        self.assertEqual(report["relationships"][0]["relation"], "rdap_nameserver")

    def test_ip_bootstrap_longest_prefix_and_range_match(self):
        bootstrap = {"services": [[["8.0.0.0/8"], ["https://coarse.example/"]],
                                   [["8.8.8.0/24"], ["https://specific.example/"]]]}
        def handler(url):
            if "data.iana.org" in url:
                return 200, bootstrap
            return 200, {"objectClassName": "ip network", "startAddress": "8.8.8.0", "endAddress": "8.8.8.255"}
        ctx = FakeContext(handler)
        self.assertEqual(infra.rdap(ctx, "8.8.8.8")["status"], "ok")
        self.assertEqual(ctx.calls[-1], "https://specific.example/ip/8.8.8.8")

    def test_ipv6_uses_ipv6_bootstrap(self):
        ctx = FakeContext(lambda url: (200, {"services": [[["2001:4860::/32"], ["https://registry.example/"]]]})
                          if "iana" in url else (200, {"objectClassName": "ip network", "startAddress": "2001:4860::",
                                                     "endAddress": "2001:4860:ffff:ffff:ffff:ffff:ffff:ffff"}))
        self.assertEqual(infra.rdap(ctx, "2001:4860:4860::8888")["status"], "ok")
        self.assertIn("ipv6.json", ctx.calls[0])

    def test_asn_range_and_route(self):
        ctx = FakeContext(lambda url: (200, {"services": [[["10000-20000"], ["https://registry.example/"]]]})
                          if "iana" in url else (200, {"objectClassName": "autnum", "startAutnum": 13335, "endAutnum": 13335}))
        self.assertEqual(infra.rdap(ctx, "AS13335")["status"], "ok")
        self.assertTrue(ctx.calls[-1].endswith("/autnum/13335"))

    def test_wrong_object_is_unknown(self):
        for wrong in ({"objectClassName": "domain", "ldhName": "other.com"},
                      {"objectClassName": "domain", "ldhName": 42}, {"ldhName": "example.com"}):
            ctx = FakeContext(lambda url, wrong=wrong: (200, {"services": [[["com"], ["https://registry.example/"]]]})
                              if "iana" in url else (200, wrong))
            with self.assertRaises(Failure) as caught:
                infra.rdap(ctx, "example.com")
            self.assertEqual(caught.exception.status, "unknown")

    def test_structured_404_only(self):
        for body, expected in (({"errorCode": 404}, "not_found"), ({"message": "Not Found"}, "unknown")):
            ctx = FakeContext(lambda url, body=body: (200, {"services": [[["com"], ["https://registry.example/"]]]})
                              if "iana" in url else (404, body))
            if expected == "not_found":
                self.assertEqual(infra.rdap(ctx, "example.com")["status"], expected)
            else:
                with self.assertRaises(Failure) as caught:
                    infra.rdap(ctx, "example.com")
                self.assertEqual(caught.exception.status, expected)

    def test_missing_bootstrap_and_http_only_are_unavailable(self):
        for bootstrap in ({"services": []}, {"services": [[["com"], ["http://registry.example/"]]]}):
            ctx = FakeContext(lambda url, bootstrap=bootstrap: (200, bootstrap))
            with self.assertRaises(Failure) as caught:
                infra.rdap(ctx, "example.com")
            self.assertEqual(caught.exception.status, "unavailable")
            self.assertEqual(len(ctx.calls), 1)

    def test_no_local_address_query(self):
        ctx = FakeContext(lambda url: self.fail("No request expected"))
        with self.assertRaises(Failure):
            infra.rdap(ctx, "127.0.0.1")
        with self.assertRaises(Failure):
            infra.rdap(ctx, "AS4294967296")

    def test_public_contacts_bounded_and_malformed_card_tolerated(self):
        body = {"objectClassName": "domain", "ldhName": "example.com", "entities": [
            {"handle": "registrar", "roles": ["registrar"], "vcardArray": ["vcard", [
                ["fn", {}, "text", "Example Registrar"], [["bad"], {}, "text", "ignored"]]]}]}
        ctx = FakeContext(lambda url: (200, {"services": [[["com"], ["https://registry.example/"]]]})
                          if "iana" in url else (200, body))
        report = infra.rdap(ctx, "example.com")
        self.assertEqual(report["data"]["entities"][0]["public_contact"][0]["value"], "Example Registrar")


class CTAndTLSTests(unittest.TestCase):
    def test_ct_names_scoped_deduplicated_and_historical(self):
        rows = [{"id": 42, "name_value": "example.com\n*.example.com\nwww.example.com\nevil-example.com\nexample.com.evil.com",
                 "not_after": "2000-01-01T00:00:00"},
                {"id": 43, "name_value": "www.example.com"}]
        ctx = FakeContext(lambda url: (200, rows))
        report = infra.ct(ctx, "example.com")
        names = [name["name"] for name in report["data"]["names"]]
        self.assertEqual(names, ["*.example.com", "example.com", "www.example.com"])
        self.assertEqual(report["data"]["names"][-1]["certificate_ids"], [42, 43])
        self.assertIn("Historical", report["data"]["scope"])
        self.assertEqual(parse_qs(urlsplit(ctx.calls[0]).query)["q"], ["%.example.com"])

    def test_ct_empty_is_not_claim_of_no_subdomains(self):
        ctx = FakeContext(lambda url: (200, []))
        report = infra.ct(ctx, "example.com")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["names"], [])

    def test_ct_malformed_is_unknown(self):
        ctx = FakeContext(lambda url: (200, [{"message": "server failure"}]))
        with self.assertRaises(Failure) as caught:
            infra.ct(ctx, "example.com")
        self.assertEqual(caught.exception.status, "unknown")

    def test_ct_caps_names(self):
        ctx = FakeContext(lambda url: (200, [{"id": 42, "name_value": "\n".join(f"n{i}.example.com" for i in range(20))}]))
        ctx.max_results = 2
        report = infra.ct(ctx, "example.com")
        self.assertEqual(len(report["data"]["names"]), 2)
        self.assertTrue(report["data"]["names_truncated"])

    def test_tls_validated_by_transport(self):
        ctx = FakeContext(lambda url: self.fail("TLS must use tls_info"))
        report = infra.tls(ctx, "example.com", 443)
        self.assertEqual(report["data"]["target"], "example.com")
        self.assertEqual(report["relationships"][0]["relation"], "tls_presented_certificate")
        self.assertEqual(ctx.calls, [("example.com", 443)])
        for port in (0, 65536, True, "443"):
            with self.assertRaises(Failure):
                infra.tls(ctx, "example.com", port)


class EmailTests(unittest.TestCase):
    def test_email_reports_dns_only(self):
        def handler(url):
            query = parse_qs(urlsplit(url).query)
            name, kind = query["name"][0], query["type"][0]
            values = []
            if kind == "MX":
                values = [{"name": name, "type": 15, "TTL": 300, "data": "10 mail.example.com."}]
            elif name.startswith("_dmarc."):
                values = [{"name": name, "type": 16, "TTL": 300, "data": '"v=DMARC1; p=reject"'}]
            else:
                values = [{"name": name, "type": 16, "TTL": 300, "data": '"v=spf1 " "-all"'}]
            return 200, dns_body(url, answers=values)
        ctx = FakeContext(handler)
        report = infra.email(ctx, "Some.One@EXAMPLE.COM")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["address"], "Some.One@example.com")
        self.assertEqual(report["data"]["mailbox_existence"], "not_checked")
        self.assertEqual(report["data"]["spf_records"][0]["text"], "v=spf1 -all")
        self.assertEqual(len(ctx.calls), 3)
        self.assertFalse(report["data"]["null_mx"])

    def test_null_mx_is_reported(self):
        ctx = FakeContext(lambda url: (200, dns_body(url, answers=[
            {"name": "example.com", "type": 15, "TTL": 300, "data": "0 ."}]
            if parse_qs(urlsplit(url).query)["type"] == ["MX"] else [])))
        self.assertTrue(infra.email(ctx, "hello@example.com")["data"]["null_mx"])

    def test_syntax_rejections_send_nothing(self):
        ctx = FakeContext(lambda url: self.fail("Invalid email must not make requests"))
        for address in ("a@@example.com", "a..b@example.com", ".a@example.com", "a.@example.com", "a@", '"a b"@example.com'):
            with self.subTest(address=address), self.assertRaises(Failure):
                infra.email(ctx, address)


if __name__ == "__main__":
    unittest.main()
