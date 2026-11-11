"""People/handle orchestration tests. Native and social transports are mocked."""
import json
import subprocess
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fieldkit_core.common import Failure, result
from fieldkit_core import people


class FakeContext:
    timeout = 3
    max_results = 10

    def __init__(self):
        self.reserved = 0
        self.sources = []

    def reserve_requests(self, count=1):
        self.reserved += count

    def add_inline_source(self, name, body, kind="tool", metadata=None):
        self.sources.append({"name": name, "body": body, "kind": kind, "metadata": metadata})
        return "native-source"


def entry(site="github", username="alice", status="found", control="passed", negative=True):
    templates = {
        "github": ("https://github.com/{u}", "https://api.github.com/users/{u}"),
        "gitlab": ("https://gitlab.com/{u}", "https://gitlab.com/api/v4/users?username={u}"),
        "codeberg": ("https://codeberg.org/{u}", "https://codeberg.org/api/v1/users/{u}"),
        "devto": ("https://dev.to/{u}", "https://dev.to/api/users/by_username?url={u}"),
        "lichess": ("https://lichess.org/@/{u}", "https://lichess.org/api/user/{u}"),
        "chess": ("https://www.chess.com/member/{u}", "https://api.chess.com/pub/player/{u}"),
        "huggingface": ("https://huggingface.co/{u}", "https://huggingface.co/api/users/{u}/overview"),
        "hackernews": ("https://news.ycombinator.com/user?id={u}", "https://hacker-news.firebaseio.com/v0/user/{u}.json"),
    }
    profile_template, request_template = templates.get(site, templates["github"])
    value = {"site": site, "site_name": people.NATIVE.get(site, site), "username": username,
             "profile_url": profile_template.format(u=username),
             "request_url": request_template.format(u=username),
             "status": status, "reason": "fixture identity and profile schema",
             "evidence": [{"field": "/login", "expected": username, "actual": username}] if status == "found" else [],
             "control": control, "http_status": 404 if status == "not_found" else 200, "elapsed_ms": 1,
             "checked_at": "2026-11-11T08:00:00Z"}
    if negative and status == "found":
        value["negative_control"] = entry(site, "hc00deadbeef00", "not_found", "not_run", negative=False)
    return value


def completed(profiles=None, code=0, raw=None):
    profiles = [entry()] if profiles is None else profiles
    payload = {"tool": "handlecheck", "version": "0.1.0", "mode": "lookup", "username": "alice",
               "complete": code == 0, "results": profiles, "summary": {},
               "started_at": "2026-11-11T08:00:00Z", "completed_at": "2026-11-11T08:00:01Z"}
    return SimpleNamespace(stdout=raw if raw is not None else json.dumps(payload).encode(),
                           stderr=b"fixture stderr", returncode=code)


class FakeSocial:
    def __init__(self):
        self.scan_social = Mock(return_value=result({"profiles": []}))

    def social_catalog(self):
        return {"sites": [{"id": key, "name": key} for key in ("reddit", "bluesky", "steam", "mastodon")]}


class NativeTests(unittest.TestCase):
    def test_native_argv_no_shell_and_source_capture(self):
        ctx = FakeContext()
        with patch.object(people.subprocess, "run", return_value=completed()) as run:
            profiles, errors = people._native(ctx, "alice", ["github"])
        self.assertEqual(profiles[0]["status"], "found")
        self.assertEqual(profiles[0]["source_ids"], ["native-source"])
        self.assertEqual(errors, [])
        self.assertEqual(ctx.reserved, 2)
        argv = run.call_args.args[0]
        self.assertIsInstance(argv, list)
        self.assertEqual(argv[1], "alice")
        self.assertIn("--json", argv)
        self.assertFalse(run.call_args.kwargs.get("shell", False))
        self.assertIn("not upstream HTTP bodies", ctx.sources[0]["metadata"]["hash_scope"])

    def test_missing_permission_and_wrong_architecture_report_unavailable(self):
        for failure in (FileNotFoundError(), PermissionError(), OSError(8, "Exec format error")):
            with self.subTest(failure=failure), patch.object(people.subprocess, "run", side_effect=failure):
                with self.assertRaises(Failure) as caught:
                    people._native(FakeContext(), "alice", ["github"])
                self.assertEqual(caught.exception.status, "unavailable")

    def test_deadline_timeout_is_retryable(self):
        with patch.object(people.subprocess, "run", side_effect=subprocess.TimeoutExpired(["backend"], 5)):
            with self.assertRaises(Failure) as caught:
                people._native(FakeContext(), "alice", ["github"])
            self.assertEqual(caught.exception.code, "NATIVE_TIMEOUT")
            self.assertTrue(caught.exception.retryable)

    def test_excessive_output_rejected(self):
        with patch.object(people.subprocess, "run", return_value=completed(raw=b"x" * (2 * 1024 * 1024 + 1))):
            with self.assertRaises(Failure) as caught:
                people._native(FakeContext(), "alice", ["github"])
            self.assertEqual(caught.exception.code, "NATIVE_OUTPUT_LIMIT")

    def test_invalid_json_and_report_shape_are_protocol_errors(self):
        for stdout in (b"not json", b"[]", b'{"results":{}}', b'{"results":[]}'):
            with self.subTest(stdout=stdout), patch.object(people.subprocess, "run", return_value=completed(raw=stdout)):
                with self.assertRaises(Failure) as caught:
                    people._native(FakeContext(), "alice", ["github"])
                self.assertEqual(caught.exception.code, "NATIVE_PROTOCOL")

    def test_malformed_result_entries_are_protocol_errors(self):
        malformed = ["not-an-object", {}, dict(entry(), status="made_up"),
                     dict(entry(), username="someone-else"), dict(entry(), site="gitlab"),
                     dict(entry(), evidence="not-an-array"), dict(entry(), reason=42)]
        for profile in malformed:
            with self.subTest(profile=profile), patch.object(people.subprocess, "run", return_value=completed([profile])):
                with self.assertRaises(Failure) as caught:
                    people._native(FakeContext(), "alice", ["github"])
                self.assertEqual(caught.exception.code, "NATIVE_PROTOCOL")

    def test_duplicate_site_results_rejected(self):
        with patch.object(people.subprocess, "run", return_value=completed([entry(), entry()])):
            with self.assertRaises(Failure) as caught:
                people._native(FakeContext(), "alice", ["github", "gitlab"])
            self.assertEqual(caught.exception.code, "NATIVE_PROTOCOL")

    def test_found_requires_passed_negative_control(self):
        profiles = [dict(entry(), control="not_run"), dict(entry(), control="failed"), entry(negative=False),
                    dict(entry(), negative_control=entry(status="found", negative=False))]
        for profile in profiles:
            with self.subTest(profile=profile), patch.object(people.subprocess, "run", return_value=completed([profile])):
                with self.assertRaises(Failure) as caught:
                    people._native(FakeContext(), "alice", ["github"])
                self.assertEqual(caught.exception.code, "NATIVE_PROTOCOL")

    def test_backend_crash_is_not_an_invalid_user_argument(self):
        with patch.object(people.subprocess, "run", return_value=completed(code=-11)):
            with self.assertRaises(Failure) as caught:
                people._native(FakeContext(), "alice", ["github"])
            self.assertEqual(caught.exception.status, "error")


class UsernameTests(unittest.TestCase):
    def setUp(self):
        self.social = FakeSocial()
        self.patcher = patch.object(people, "_social", return_value=self.social)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)

    def test_confirmed_profiles_do_not_infer_same_owner(self):
        with patch.object(people.subprocess, "run", return_value=completed()):
            report = people.username(FakeContext(), "@alice", ["github"])
        self.assertEqual(report["data"]["confirmed_count"], 1)
        self.assertEqual(report["data"]["identity_attribution"], "not_established")
        self.assertTrue(all(relation["relation"] == "public_profile_at" for relation in report["relationships"]))
        self.assertTrue(all(candidate["status"] == "unverified" for candidate in report["data"]["candidate_links"]))

    def test_native_failure_preserves_typed_unresolved_records(self):
        with patch.object(people.subprocess, "run", side_effect=FileNotFoundError()):
            report = people.username(FakeContext(), "alice", ["github"])
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["confirmed_count"], 0)
        self.assertEqual(report["data"]["profiles"][0]["status"], "unavailable")
        self.assertEqual(report["relationships"], [])

    def test_native_site_rejects_incompatible_handle_without_claims(self):
        output = completed(code=1)
        output.stderr = b"No selected site accepts this username format; no requests sent"
        with patch.object(people.subprocess, "run", return_value=output):
            report = people.username(FakeContext(), "_", ["github"])
        self.assertEqual(report["status"], "invalid")
        self.assertEqual(report["data"]["confirmed_count"], 0)
        self.assertEqual(report["data"]["profiles"][0]["status"], "invalid")
        self.assertEqual(report["relationships"], [])

    def test_social_group_failure_keeps_native_success(self):
        self.social.scan_social.side_effect = Failure("UPSTREAM", "fixture social outage")
        with patch.object(people.subprocess, "run", return_value=completed()):
            report = people.username(FakeContext(), "alice", ["github", "reddit"])
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["confirmed_count"], 1)
        self.assertEqual({row["site"] for row in report["data"]["profiles"]}, {"github", "reddit"})

    def test_federated_default_only_uses_mastodon(self):
        with patch.object(people.subprocess, "run") as run:
            report = people.username(FakeContext(), "@alice@example.social")
        self.assertEqual(report["data"]["checked_sites"], ["mastodon"])
        self.social.scan_social.assert_called_once_with(unittest.mock.ANY, "@alice@example.social", site_ids=["mastodon"])
        run.assert_not_called()

    def test_candidate_only_selection_stays_unverified_without_network(self):
        with patch.object(people.subprocess, "run") as run:
            report = people.username(FakeContext(), "alice", ["instagram"])
        self.assertEqual(report["data"]["confirmed_count"], 0)
        self.assertTrue(any(row["status"] == "unverified" for row in report["data"]["profiles"]))
        self.assertEqual(report["relationships"], [])
        run.assert_not_called()
        self.social.scan_social.assert_not_called()

    def test_unknown_site_rejects_without_subprocess(self):
        with patch.object(people.subprocess, "run") as run:
            with self.assertRaises(Failure) as caught:
                people.username(FakeContext(), "alice", ["no-such-platform"])
        self.assertEqual(caught.exception.code, "UNKNOWN_SITE")
        run.assert_not_called()

    def test_invalid_handle_no_external_work(self):
        with patch.object(people.subprocess, "run") as run:
            for handle in ("", "../alice", "alice\n", "a b", "x" * 256):
                with self.subTest(handle=handle), self.assertRaises(Failure):
                    people.username(FakeContext(), handle, ["github"])
            run.assert_not_called()

    def test_candidates_never_claim_existence_and_unsafe_handle_has_none(self):
        self.assertTrue(all(row["status"] == "unverified" for row in people.candidate_links("alice")))
        self.assertEqual(people.candidate_links("alice/bob"), [])


class PersonAndProfileTests(unittest.TestCase):
    def test_person_plans_quoted_queries_without_network(self):
        with patch.object(people.subprocess, "run") as run, patch("fieldkit_core.web.search") as search:
            report = people.person(FakeContext(), 'Alice " OR site:evil.example', context="London")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["data"]["identity_attribution"], "not_established")
        self.assertEqual(report["data"]["queries"][0].count('"'), 2)
        self.assertTrue(report["data"]["queries"][0].startswith('"Alice '))
        search.assert_not_called()
        run.assert_not_called()

    def test_blank_names_rejected(self):
        for value in ("", "   ", "\nAlice"):
            with self.subTest(value=value), self.assertRaises(Failure):
                people.person(FakeContext(), value)

    def test_optional_search_absent_key_reports_unavailable_without_secret_access_here(self):
        with patch("fieldkit_core.web.search", side_effect=Failure("API_KEY_REQUIRED", "Search key unavailable", "unavailable")) as search:
            report = people.person(FakeContext(), "Alice Example", search=True)
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["hits"], [])
        self.assertEqual(search.call_count, 1)

    def test_exact_profile_routes_to_corresponding_adapter(self):
        examples = {"https://github.com/alice": ("alice", "github"),
                    "https://reddit.com/user/alice/": ("alice", "reddit"),
                    "https://bsky.app/profile/alice.bsky.social": ("alice.bsky.social", "bluesky"),
                    "https://steamcommunity.com/id/alice": ("alice", "steam"),
                    "https://example.social/@alice": ("@alice@example.social", "mastodon")}
        for url, (handle, site) in examples.items():
            with self.subTest(url=url), patch.object(people, "username", return_value={"fixture": True}) as username:
                people.profile(FakeContext(), url)
                self.assertEqual(username.call_args.args[1], handle)
                self.assertEqual(username.call_args.kwargs["sites"], [site])

    def test_repo_subpaths_and_unusual_ports_are_only_url_evidence(self):
        examples = ("https://github.com/alice/repo", "https://gitlab.com/alice/project",
                    "https://reddit.com/r/alice", "https://github.com:8443/alice",
                    "https://github.com.evil.example/alice")
        for url in examples:
            with self.subTest(url=url), patch.object(people, "username") as username, \
                 patch("fieldkit_core.web.http", return_value=result({"url": url})) as http:
                report = people.profile(FakeContext(), url)
                username.assert_not_called()
                http.assert_called_once()
                self.assertEqual(report["data"]["identity_status"], "unverified")

    def test_federated_www_host_is_not_silently_changed(self):
        with patch.object(people, "username", return_value={"fixture": True}) as username:
            people.profile(FakeContext(), "https://www.social.example/@alice")
        self.assertEqual(username.call_args.args[1], "@alice@www.social.example")


if __name__ == "__main__":
    unittest.main()
