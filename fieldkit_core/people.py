"""Public handle checks and explicit, unverified people-discovery leads."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import threading
from urllib.parse import quote, urlsplit, parse_qs

from .common import Failure, result, web_url

NATIVE = {"github": "GitHub", "gitlab": "GitLab", "codeberg": "Codeberg", "devto": "DEV",
          "lichess": "Lichess", "chess": "Chess.com", "huggingface": "Hugging Face", "hackernews": "Hacker News"}
_CHECKOUT_BACKEND = Path(__file__).resolve().parent.parent / "native" / "handlecheck" / "handlecheck"
BACKEND = _CHECKOUT_BACKEND if _CHECKOUT_BACKEND.is_file() else Path(sys.prefix) / "share" / "fieldkit" / "native" / "handlecheck" / "handlecheck"
_NATIVE_LOCK = threading.Lock()
NATIVE_URLS = {"github": "https://github.com/{u}", "gitlab": "https://gitlab.com/{u}",
    "codeberg": "https://codeberg.org/{u}", "devto": "https://dev.to/{u}",
    "lichess": "https://lichess.org/@/{u}", "chess": "https://www.chess.com/member/{u}",
    "huggingface": "https://huggingface.co/{u}", "hackernews": "https://news.ycombinator.com/user?id={u}"}
CANDIDATES = {
    "instagram": "https://www.instagram.com/{u}/", "x": "https://x.com/{u}",
    "tiktok": "https://www.tiktok.com/@{u}",
    "threads": "https://www.threads.net/@{u}", "pinterest": "https://www.pinterest.com/{u}/",
    "facebook": "https://www.facebook.com/{u}", "twitch": "https://www.twitch.tv/{u}",
    "medium": "https://medium.com/@{u}", "linkedin": "https://www.linkedin.com/in/{u}/",
    "telegram": "https://t.me/{u}", "keybase": "https://keybase.io/{u}",
}


def _social():
    from . import social
    return social


def catalog() -> dict:
    native = [{"id": site, "name": name, "engine": "native", "verification": "identity_schema_and_negative_control"}
              for site, name in NATIVE.items()]
    social = _social().social_catalog()
    descriptors = social.get("sites", []) if isinstance(social, dict) else social
    return {"verified_adapters": native + descriptors,
            "candidate_only_platforms": [{"id": site, "url_template": url, "status": "unverified"}
                                         for site, url in CANDIDATES.items() if site not in {entry["id"] for entry in descriptors}],
            "identity_attribution": "Shared handles do not establish a shared owner"}


def candidate_links(handle: str) -> list[dict]:
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", handle):
        return []
    return [{"site": site, "url": pattern.format(u=quote(handle, safe="")), "status": "unverified",
             "reason": "URL constructed from handle; profile existence and ownership have not been checked"}
            for site, pattern in CANDIDATES.items()]


def _native(ctx, handle: str, sites: list[str]) -> tuple[list[dict], list[dict]]:
    ctx.reserve_requests(len(sites) * 2)  # Conservative upper bound; engine uses at most two GETs per site.
    command = [str(BACKEND), handle, "--site", ",".join(sites), "--json", "--timeout", str(ctx.timeout), "--jobs", "4"]
    try:
        # Serializing native invocations also respects Lichess's per-host serial-request policy in batches.
        with _NATIVE_LOCK:
            completed = subprocess.run(command, capture_output=True, timeout=2 * ((len(sites) + 3) // 4) * ctx.timeout + 5,
                                       check=False)
    except (FileNotFoundError, PermissionError, OSError) as exc:
        raise Failure("NATIVE_BACKEND_UNAVAILABLE", "Rebuild the native backend with make for this platform", "unavailable",
                      details={"backend": str(BACKEND), "build_argv": ["make", "-C", str(BACKEND.parent)]}) from exc
    except subprocess.TimeoutExpired as exc:
        raise Failure("NATIVE_TIMEOUT", "Username backend exceeded its command deadline", retryable=True) from exc
    if len(completed.stdout) > 2 * 1024 * 1024:
        raise Failure("NATIVE_OUTPUT_LIMIT", "Username backend returned excessive output")
    if completed.returncode not in {0, 2}:
        raise Failure("NATIVE_ARGUMENTS" if completed.returncode == 1 else "NATIVE_FAILED",
                      "Username backend could not run this lookup", "invalid" if completed.returncode == 1 else "error",
                      details={"exit_code": completed.returncode, "message": completed.stderr.decode("utf-8", "replace")[:500]})
    try:
        payload = json.loads(completed.stdout)
        profiles = payload["results"]
        if not isinstance(profiles, list) or len(profiles) != len(sites):
            raise ValueError("Unexpected results")
        seen = set()
        for profile in profiles:
            if not isinstance(profile, dict) or profile.get("site") not in sites or profile["site"] in seen:
                raise ValueError("Unexpected or duplicated site")
            seen.add(profile["site"])
            if profile.get("username") != handle or profile.get("status") not in {
                    "found", "not_found", "blocked", "rate_limited", "unknown", "error", "invalid"}:
                raise ValueError("Unexpected identity or status")
            if not isinstance(profile.get("reason"), str) or not isinstance(profile.get("evidence"), list):
                raise ValueError("Invalid evidence schema")
            if profile.get("profile_url") != NATIVE_URLS[profile["site"]].format(u=quote(handle, safe="")):
                raise ValueError("Unexpected profile URL")
            if profile["status"] == "found":
                negative = profile.get("negative_control")
                if profile.get("control") != "passed" or not isinstance(negative, dict) or negative.get("status") != "not_found":
                    raise ValueError("Unverified negative control")
                if negative.get("site") != profile["site"] or negative.get("username") == handle or not profile["evidence"]:
                    raise ValueError("Invalid control provenance")
    except (ValueError, KeyError, TypeError, AttributeError) as exc:
        raise Failure("NATIVE_PROTOCOL", "Username backend returned an invalid report") from exc
    source_id = ctx.add_inline_source("handlecheck native JSON report", completed.stdout, kind="tool_report",
                                      metadata={"engine": "handlecheck/0.1.0", "provider_request_urls":
                                                [entry.get("request_url") for entry in profiles],
                                                "hash_scope": "engine JSON report, not upstream HTTP bodies"})
    normalized = []
    for profile in profiles:
        normalized.append({"site": profile["site"], "username": profile["username"],
                           "profile_url": profile["profile_url"], "status": profile["status"],
                           "reason": profile["reason"], "evidence": profile.get("evidence", []),
                           "source_ids": [source_id], "checked_at": profile.get("checked_at"),
                           "control": {"status": profile.get("control"),
                                       "result": profile.get("negative_control")}})
    return normalized, []


def username(ctx, target: str, sites: list[str] | None = None, include_candidates: bool = True) -> dict:
    if not isinstance(target, str) or len(target) > 255 or any(ord(ch) < 33 for ch in target):
        raise Failure("INVALID_HANDLE", "Supply a public handle without whitespace", "invalid")
    federated = target.count("@") >= 1 and "@" in target.lstrip("@")
    handle = target if federated else target.removeprefix("@")
    if not federated and not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", handle):
        raise Failure("INVALID_HANDLE", "Handle must use ASCII letters, digits, dots, hyphens, or underscores", "invalid")
    descriptors = _social().social_catalog()
    social_ids = {entry["id"] for entry in descriptors["sites"]}
    defaults = [site for site in ["reddit", "bluesky", "steam", "youtube"] if site in social_ids]
    selected = sites or (["mastodon"] if federated else list(NATIVE) + defaults)
    if not selected or any(site not in set(NATIVE) | social_ids | set(CANDIDATES) for site in selected):
        raise Failure("UNKNOWN_SITE", "Unknown verified adapter; consult capabilities or use candidate links", "invalid",
                      details={"sites": selected})
    selected = list(dict.fromkeys(selected))
    profiles, errors = [], []
    native_sites = [site for site in selected if site in NATIVE]
    if native_sites:
        try:
            entries, failures = _native(ctx, handle, native_sites)
            profiles.extend(entries)
            errors.extend(failures)
        except Failure as exc:
            errors.append(exc.as_dict())
            profiles.extend({"site": site, "username": handle, "status": exc.status, "reason": exc.message,
                             "source_ids": [], "evidence": []} for site in native_sites)
    selected_social = [site for site in selected if site in social_ids]
    if selected_social:
        try:
            social_result = _social().scan_social(ctx, handle, site_ids=selected_social)
            profiles.extend(social_result["data"]["profiles"])
            errors.extend(social_result.get("errors", []))
        except Failure as exc:
            errors.append(exc.as_dict())
            profiles.extend({"site": site, "username": handle, "status": exc.status, "reason": exc.message,
                             "source_ids": [], "evidence": []} for site in selected_social)
    for site in selected:
        if site in CANDIDATES and site not in social_ids:
            profiles.append({"site": site, "username": handle, "status": "unverified",
                             "profile_url": CANDIDATES[site].format(u=quote(handle, safe="")),
                             "reason": "Candidate URL only; no verified adapter for this platform",
                             "source_ids": [], "evidence": []})
    confirmed = [entry for entry in profiles if entry["status"] == "found"]
    unresolved = [entry for entry in profiles if entry["status"] not in {"found", "not_found", "invalid"}]
    for entry in unresolved:
        failure = entry.get("error")
        errors.append(failure if isinstance(failure, dict) else {
            "code": "PROFILE_" + entry["status"].upper(), "message": entry["reason"],
            "retryable": entry["status"] in {"rate_limited", "error"},
            "details": {"site": entry["site"], "source_ids": entry.get("source_ids", [])}})
    all_invalid = profiles and all(entry["status"] == "invalid" for entry in profiles)
    status = "invalid" if all_invalid else "partial" if unresolved else "ok"
    entities, relationships = [], []
    for entry in confirmed:
        profile = entry.get("profile_url")
        if profile:
            entities.append({"type": "profile_url", "value": profile, "source_ids": entry.get("source_ids", [])})
            scoped = entry["site"] + ":" + entry["username"]
            entities.append({"type": "username", "value": scoped, "source_ids": entry.get("source_ids", [])})
            relationships.append({"source": {"type": "username", "value": scoped},
                                  "relation": "public_profile_at", "target": {"type": "profile_url", "value": profile},
                                  "source_ids": entry.get("source_ids", [])})
            for link in entry.get("public_metadata", {}).get("outbound_links", [])[:ctx.max_results]:
                entities.append({"type": "url", "value": link, "source_ids": entry.get("source_ids", [])})
                relationships.append({"source": {"type": "profile_url", "value": profile}, "relation": "links_to",
                                      "target": {"type": "url", "value": link}, "source_ids": entry.get("source_ids", [])})
    return result({"handle": handle, "profiles": profiles, "confirmed_count": len(confirmed),
                   "unresolved_count": len(unresolved), "checked_sites": selected,
                   "candidate_links": candidate_links(handle) if include_candidates else [],
                   "identity_attribution": "not_established", "ownership_warning":
                   "These records show public handle matches, not that accounts belong to one person"},
                  status=status, errors=errors, entities=entities, relationships=relationships)


def person(ctx, target: str, context: str = "", search: bool = False) -> dict:
    if not isinstance(target, str) or not target.strip() or not isinstance(context, str) or len(target) > 120 or any(ord(ch) < 32 for ch in target) or len(context) > 200 or any(ord(ch) < 32 for ch in context):
        raise Failure("INVALID_NAME", "Supply a name of 1-120 characters and optional context up to 200 characters", "invalid")
    name = '"' + target.replace('"', ' ') + '"'
    base = (name + " " + context).strip()
    queries = [base, base + " (site:linkedin.com/in OR site:github.com)",
               base + " (site:instagram.com OR site:x.com OR site:bsky.app)",
               base + " (site:youtube.com OR site:tiktok.com)"]
    data = {"name": target, "context": context, "queries": queries, "hits": [],
            "identity_attribution": "not_established", "search_requested": search, "search_executed": False,
            "next_steps": ["Use returned queries with a web search tool", "Verify candidate profiles with profile or username",
                           "Keep same-name candidates separate until supported by explicit public links"]}
    errors, entities, relationships = [], [], []
    if search:
        from .web import search as web_search
        for query in queries:
            try:
                found = web_search(ctx, query, limit=min(ctx.max_results, 10))
                data["search_executed"] = True
                data["hits"].append({"query": query, "result": found["data"]})
                errors.extend(found.get("errors", []))
                entities.extend(found.get("entities", []))
                relationships.extend(found.get("relationships", []))
            except Failure as exc:
                errors.append(exc.as_dict())
                if exc.status in {"unavailable", "rate_limited", "blocked"}:
                    break
    return result(data, status="partial" if errors else "ok", errors=errors, entities=entities, relationships=relationships)


def profile(ctx, target: str) -> dict:
    url = web_url(target)
    parts = urlsplit(url)
    segments = [piece for piece in parts.path.split("/") if piece]
    original_host = (parts.hostname or "").lower()
    host = original_host.removeprefix("www.")
    standard_port = parts.port in {None, 443 if parts.scheme == "https" else 80}
    mapping = {"github.com": "github", "gitlab.com": "gitlab", "codeberg.org": "codeberg",
               "dev.to": "devto", "huggingface.co": "huggingface", "reddit.com": "reddit"}
    direct = host in mapping and host != "reddit.com" and len(segments) == 1
    reddit = host == "reddit.com" and len(segments) == 2 and segments[0] in {"user", "u"}
    if standard_port and (direct or reddit):
        handle = segments[1] if reddit else segments[0]
        return username(ctx, handle, sites=[mapping[host]], include_candidates=False)
    if standard_port and host == "bsky.app" and len(segments) == 2 and segments[0] == "profile":
        return username(ctx, segments[1], sites=["bluesky"], include_candidates=False)
    if standard_port and host == "steamcommunity.com" and len(segments) == 2 and segments[0] == "id":
        return username(ctx, segments[1], sites=["steam"], include_candidates=False)
    if standard_port and host == "lichess.org" and len(segments) == 2 and segments[0] == "@":
        return username(ctx, segments[1], sites=["lichess"], include_candidates=False)
    if standard_port and host == "chess.com" and len(segments) == 2 and segments[0] == "member":
        return username(ctx, segments[1], sites=["chess"], include_candidates=False)
    if standard_port and host == "news.ycombinator.com" and parts.path == "/user" and len(parse_qs(parts.query).get("id", [])) == 1:
        return username(ctx, parse_qs(parts.query)["id"][0], sites=["hackernews"], include_candidates=False)
    if standard_port and host == "youtube.com" and len(segments) == 1 and segments[0].startswith("@"):
        if "youtube" in {entry["id"] for entry in _social().social_catalog()["sites"]}:
            return username(ctx, segments[0], sites=["youtube"], include_candidates=False)
    if standard_port and len(segments) == 1 and segments[0].startswith("@") and host not in {"instagram.com", "tiktok.com", "youtube.com", "threads.net"}:
        return username(ctx, segments[0] + "@" + original_host, sites=["mastodon"], include_candidates=False)
    from .web import http
    response = http(ctx, url)
    response["data"]["identity_status"] = "unverified"
    response["data"]["interpretation"] = "Page metadata is evidence about this URL, not a verified account identity"
    return response
