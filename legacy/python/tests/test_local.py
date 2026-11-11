from __future__ import annotations

import hashlib
import os
from pathlib import Path
import struct
import tempfile
import unittest

from fieldkit_core.common import Failure
from fieldkit_core.local import extract, file_info


class Context:
    max_results = 50
    max_file_bytes = 20 * 1024 * 1024

    def __init__(self):
        self.sources = []

    def add_local_source(self, path, sha256, size, media_type):
        source_id = f"local-{len(self.sources)}"
        self.sources.append({"source_id": source_id, "path": path, "sha256": sha256,
                             "size_bytes": size, "media_type": media_type})
        return source_id


class LocalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary.name)
        self.ctx = Context()

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, content, name="evidence.txt"):
        path = self.path / name
        path.write_bytes(content.encode() if isinstance(content, str) else content)
        return path

    def failure(self, function, target, code, **options):
        with self.assertRaises(Failure) as caught:
            function(self.ctx, str(target), **options)
        self.assertEqual(caught.exception.code, code)
        return caught.exception

    def test_hashes_and_local_provenance(self):
        payload = b"public evidence\n"
        path = self.write(payload)
        response = file_info(self.ctx, str(path))
        data = response["data"]
        self.assertEqual(response["status"], "ok")
        self.assertEqual(data["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(data["sha512"], hashlib.sha512(payload).hexdigest())
        self.assertEqual(data["size_bytes"], len(payload))
        self.assertEqual(data["path"], str(path))
        self.assertEqual(data["source_id"], self.ctx.sources[0]["source_id"])
        self.assertEqual(response["entities"][0]["type"], "file_sha256")

    def test_empty_file(self):
        response = file_info(self.ctx, str(self.write(b"")))
        self.assertEqual(response["data"]["sha256"], hashlib.sha256(b"").hexdigest())
        self.assertEqual(response["data"]["size_bytes"], 0)
        self.assertEqual(extract(self.ctx, str(self.path / "evidence.txt"))["data"]["indicators"], [])

    def test_signature_distinct_from_extension(self):
        path = self.write(b"%PDF-1.7\n", "fake.jpg")
        data = file_info(self.ctx, str(path))["data"]
        self.assertEqual(data["media_type"], "application/pdf")
        self.assertEqual(data["filename_media_type"], "image/jpeg")
        self.assertEqual(data["media_type_method"], "file_signature")

    def test_png_dimensions_only_from_correct_ihdr(self):
        payload = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\rIHDR" + struct.pack(">II", 640, 480)
        self.assertEqual(file_info(self.ctx, str(self.write(payload, "image.png")))["data"]["image"]["width"], 640)
        malformed = payload[:12] + b"FAKE" + payload[16:]
        self.assertNotIn("image", file_info(self.ctx, str(self.write(malformed, "broken.png")))["data"])

    def test_jpeg_dimensions_and_short_header(self):
        payload = bytes.fromhex("ffd8 ffc0 000b 08 01e0 0280 01 01 11 00 ffd9")
        data = file_info(self.ctx, str(self.write(payload, "image.jpeg")))["data"]
        self.assertEqual((data["image"]["width"], data["image"]["height"]), (640, 480))
        self.assertNotIn("image", file_info(self.ctx, str(self.write(b"\xff\xd8\xff\xc0", "short.jpg")))["data"])

    def test_symlinks_directories_and_fifo_rejected(self):
        original = self.write("example.org")
        link = self.path / "link"
        link.symlink_to(original)
        self.failure(file_info, link, "SYMLINK_NOT_ALLOWED")
        self.failure(file_info, self.path, "NOT_REGULAR_FILE")
        if hasattr(os, "mkfifo"):
            fifo = self.path / "pipe"
            os.mkfifo(fifo)
            self.failure(file_info, fifo, "NOT_REGULAR_FILE")

    def test_missing_file(self):
        failure = self.failure(file_info, self.path / "missing", "FILE_NOT_FOUND")
        self.assertEqual(failure.status, "not_found")

    def test_file_size_cap(self):
        self.ctx.max_file_bytes = 3
        self.failure(file_info, self.write("1234"), "FILE_TOO_LARGE")
        self.assertEqual(self.ctx.sources, [])

    def test_utf8_only_and_binary_controls(self):
        self.failure(extract, self.write(b"\xff\xfeExample.org"), "NOT_UTF8_TEXT")
        self.failure(extract, self.write(b"example.org\x00secret"), "NOT_UTF8_TEXT")
        self.failure(extract, self.write(b"example.org\x01"), "NOT_UTF8_TEXT")

    def test_extract_all_common_kinds(self):
        path = self.write("https://EXAMPLE.org/a\nAlice@Example.org 8.8.8.8 2001:4860:4860::8888")
        response = extract(self.ctx, str(path))
        values = {(entry["type"], entry["value"]) for entry in response["data"]["indicators"]}
        self.assertIn(("url", "https://example.org/a"), values)
        self.assertIn(("domain", "example.org"), values)
        self.assertIn(("email", "Alice@example.org"), values)
        self.assertIn(("ipv4", "8.8.8.8"), values)
        self.assertIn(("ipv6", "2001:4860:4860::8888"), values)
        self.assertTrue(all(item["relation"] == "contains_indicator" for item in response["relationships"]))
        self.assertIn("not resolved", response["data"]["interpretation"])

    def test_unicode_offsets_and_lines(self):
        text = "😀 evidence\n✓ https://example.org/path\n"
        values = extract(self.ctx, str(self.write(text)), kinds="url")["data"]["indicators"]
        occurrence = values[0]["occurrences"][0]
        self.assertEqual(occurrence["line"], 2)
        self.assertEqual(occurrence["column"], 3)
        self.assertEqual(occurrence["char_offset"], text.index("https://"))
        self.assertEqual(text[occurrence["char_offset"]:occurrence["end_char_offset"]], "https://example.org/path")

    def test_html_and_json_without_execution(self):
        text = '<script>throw "not executed";</script><a href="https://example.org">mail@example.org</a>'
        response = extract(self.ctx, str(self.write(text, "data.html")), kinds=["url", "email"])
        self.assertEqual(response["data"]["returned"], 2)
        self.assertEqual(response["data"]["file"]["source_id"], self.ctx.sources[0]["source_id"])

    def test_punctuation_and_markdown(self):
        text = "[link](https://example.org/a(b)). Email Alice@example.org. IP 8.8.8.8. IPv6 2001:db8::1."
        values = {(i["type"], i["value"]) for i in extract(self.ctx, str(self.write(text)))["data"]["indicators"]}
        self.assertIn(("url", "https://example.org/a(b)"), values)
        self.assertIn(("email", "Alice@example.org"), values)
        self.assertIn(("ipv4", "8.8.8.8"), values)
        self.assertIn(("ipv6", "2001:db8::1"), values)

    def test_invalid_ip_and_domain_boundaries(self):
        text = "999.1.1.1 01.2.3.4 8.8.8.8.example.org _bad.example.org -bad.example.org bad_.example.org 2001:GGGG::1"
        indicators = extract(self.ctx, str(self.write(text)))["data"]["indicators"]
        self.assertFalse(any(item["type"] in {"ipv4", "ipv6"} for item in indicators))
        self.assertFalse(any(item["value"] in {"bad.example.org", "example.org"} for item in indicators))

    def test_email_local_part_preserved_and_invalid_dots_rejected(self):
        text = "First.Last@EXAMPLE.ORG .bad@example.org bad..name@example.org bad.@example.org"
        indicators = extract(self.ctx, str(self.write(text)), kinds="emails")["data"]["indicators"]
        self.assertEqual([item["value"] for item in indicators], ["First.Last@example.org"])

    def test_repeated_indicators_bounded_occurrences(self):
        values = extract(self.ctx, str(self.write("example.org\n" * 20)), kinds="domains")["data"]["indicators"]
        self.assertEqual(len(values), 1)
        self.assertEqual(values[0]["occurrence_count"], 20)
        self.assertEqual(len(values[0]["occurrences"]), 8)
        self.assertTrue(values[0]["occurrences_truncated"])

    def test_result_limit_and_scan_complete(self):
        self.ctx.max_results = 1
        data = extract(self.ctx, str(self.write("one.example.org two.example.org")), kinds="domain")["data"]
        self.assertEqual(data["returned"], 1)
        self.assertEqual(data["unique_indicators_seen"], 2)
        self.assertTrue(data["truncated"])
        self.assertTrue(data["scan_complete"])

    def test_kind_validation_before_file_read(self):
        self.failure(extract, self.path / "missing", "INVALID_KIND", kinds="password")
        self.assertEqual(self.ctx.sources, [])

    def test_private_ip_is_observed_without_network(self):
        data = extract(self.ctx, str(self.write("10.0.0.1 ::1")), kinds="ip")["data"]
        self.assertEqual({item["value"] for item in data["indicators"]}, {"10.0.0.1", "::1"})

    def test_unreasonably_long_url_not_returned_as_valid_prefix(self):
        data = extract(self.ctx, str(self.write("https://example.org/" + "a" * 5000)), kinds="url")["data"]
        self.assertEqual(data["indicators"], [])


if __name__ == "__main__":
    unittest.main()
