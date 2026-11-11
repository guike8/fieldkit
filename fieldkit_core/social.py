"""Conservative public social-profile lookup with paired negative controls.

Contracts: Reddit /user/{username}/about, Bluesky app.bsky.actor.getProfile,
Mastodon /api/v1/accounts/lookup; Steam's deployed g_rgProfileData JSON was
live-validated on 2026-11-11. No JavaScript is executed or login performed.
"""
from __future__ import annotations

from html.parser import HTMLParser
import json
import re
import secrets
from urllib.parse import parse_qs, quote, urlencode, urljoin, urlsplit

from .common import Failure, domain_name, result, utc_now, web_url
from .web import _Page, _text


_SITES = [
    {"id": "reddit", "name": "Reddit", "example_username": "spez",
     "description": "Public account JSON; access-denied responses remain blocked",
     "profile_template": "https://www.reddit.com/user/{username}/", "input_pattern": "[A-Za-z0-9_-]{3,20}",
     "documentation": "https://www.reddit.com/dev/api/#GET_user_{username}_about"},
    {"id": "bluesky", "name": "Bluesky", "example_username": "bsky.app",
     "description": "Public AppView profile by fully qualified handle; bare handles use .bsky.social",
     "profile_template": "https://bsky.app/profile/{username}", "input_pattern": "handle or handle.domain",
     "documentation": "https://github.com/bluesky-social/bsky/blob/main/lexicons/app/bsky/actor/getProfile.json"},
    {"id": "mastodon", "name": "Mastodon", "example_username": "@Gargron@mastodon.social",
     "description": "Exact home-instance account lookup; requires @user@instance, no instance guessing",
     "profile_template": "https://{instance}/@{username}", "input_pattern": "@user@instance.domain",
     "documentation": "https://docs.joinmastodon.org/methods/accounts/#lookup"},
    {"id": "steam", "name": "Steam", "example_username": "gabelogannewell",
     "description": "Public vanity-profile URL and stable ID in embedded profile JSON, without executing scripts",
     "profile_template": "https://steamcommunity.com/id/{username}/", "input_pattern": "[A-Za-z0-9_-]{2,32}",
     "documentation": "https://steamcommunity.com/dev"},
    {"id": "youtube", "name": "YouTube", "example_username": "Google",
     "description": "Public handle URL plus stable channel ID in embedded channel metadata; no script execution",
     "profile_template": "https://www.youtube.com/@{username}", "input_pattern": "[A-Za-z0-9][A-Za-z0-9_.-]{1,28}[A-Za-z0-9]",
     "documentation": "https://developers.google.com/youtube/v3/docs/channels/list#forHandle"},
]


def social_catalog() -> dict:
    return {"sites": [dict(site) for site in _SITES], "negative_controls": "required_for_found",
            "identity_scope": "A matching handle does not establish shared ownership across sites",
            "visibility_scope": "not_found refers only to the queried public provider or home instance"}


def _normalize(site: str, handle: str) -> tuple[str, str, str]:
    if not isinstance(handle, str) or not handle or len(handle) > 255 or any(ord(c) < 33 or ord(c) > 126 for c in handle):
        raise Failure("INVALID_USERNAME", "Supply an ASCII username or a supported qualified handle", "invalid")
    if site == "mastodon":
        value = handle[1:] if handle.startswith("@") else handle
        if value.count("@") != 1:
            raise Failure("MASTODON_INSTANCE_REQUIRED", "Mastodon requires @user@instance.domain", "invalid")
        username, instance = value.split("@", 1)
        if not re.fullmatch(r"[A-Za-z0-9_]{1,64}", username):
            raise Failure("INVALID_USERNAME", "Mastodon username has an unsupported lookup format", "invalid")
        instance = domain_name(instance)
        return username + "@" + instance, "https://" + instance + "/@" + username, instance
    value = handle[1:] if handle.startswith("@") else handle
    if site == "youtube":
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{1,28}[A-Za-z0-9]", value):
            raise Failure("INVALID_USERNAME", "YouTube lookup supports ASCII handles of 3–30 characters", "invalid")
        return value, "https://www.youtube.com/@" + value, ""
    if site == "bluesky":
        if "." not in value:
            value += ".bsky.social"
        value = domain_name(value)
        if "." not in value or len(value) > 253 or not re.search(r"\.[A-Za-z]", value):
            raise Failure("INVALID_USERNAME", "Bluesky requires a valid fully qualified handle", "invalid")
        return value, "https://bsky.app/profile/" + value, ""
    # Keep patterns explicit here so descriptor prose is never executed as a regex.
    pattern = r"[A-Za-z0-9_-]{3,20}" if site == "reddit" else r"[A-Za-z0-9_-]{2,32}"
    if not re.fullmatch(pattern, value):
        raise Failure("INVALID_USERNAME", "Username has an unsupported lookup format for " + site, "invalid")
    base = "https://www.reddit.com/user/" if site == "reddit" else "https://steamcommunity.com/id/"
    return value, base + value + "/", ""


def _record(site: str, handle: str) -> dict:
    return {"site": site, "username": handle, "profile_url": None, "request_url": None,
            "status": "unknown", "reason": "No conclusive public profile evidence", "evidence": [],
            "source_ids": [], "control": "not_run", "checked_at": utc_now()}


def _set(profile: dict, state: str, reason: str) -> dict:
    profile.update(status=state, reason=reason)
    return profile


def _json(ctx, response):
    try:
        return ctx.json(response)
    except Failure:
        return None


def _transport_state(profile: dict, response) -> dict | None:
    if response.status == 429:
        return _set(profile, "rate_limited", "Provider rate limit prevented verification")
    if response.status in {401, 403}:
        return _set(profile, "blocked", "Provider denied unauthenticated access")
    if response.status >= 500:
        return _set(profile, "error", "Provider returned a server error")
    if 300 <= response.status < 400:
        return _set(profile, "unknown", "Redirect does not establish the requested profile identity")
    return None


def _metadata(profile: dict, display_name=None, bio=None, links=None):
    metadata = {}
    if isinstance(display_name, str) and display_name:
        metadata["display_name"] = _text(display_name, 300)
    if isinstance(bio, str) and bio:
        metadata["bio"] = _text(bio, 2000)
    if links:
        metadata["outbound_links"] = links
    if metadata:
        profile["public_metadata"] = metadata


def _plain_links(text: str, maximum: int) -> list[str]:
    links = []
    for match in re.finditer(r"https?://[^\s<>\"'()]+", text):
        try:
            link = web_url(match.group().rstrip(".,;:!"))
            if link not in links:
                links.append(link)
        except Failure:
            continue
        if len(links) >= maximum:
            break
    return links


class _Text(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.text = ""

    def handle_data(self, data):
        if len(self.text) < 2000:
            self.text += " " + data[:2000 - len(self.text)]


class _Steam(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.scripts = []
        self._script = False
        self._current = ""
        self._error_depth = 0
        self._h3 = False
        self.error_heading = ""

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if tag == "script":
            self._script, self._current = True, ""
        if tag == "div":
            if self._error_depth:
                self._error_depth += 1
            elif "error_ctn" in (values.get("class") or "").split():
                self._error_depth = 1
        if tag == "h3" and self._error_depth:
            self._h3 = True

    def handle_endtag(self, tag):
        if tag == "script":
            if "g_rgProfileData" in self._current:
                self.scripts.append(self._current)
            self._script, self._current = False, ""
        if tag == "div" and self._error_depth:
            self._error_depth -= 1
        if tag == "h3":
            self._h3 = False

    def handle_data(self, data):
        if self._script and len(self._current) < 128000:
            self._current += data[:128000 - len(self._current)]
        if self._h3 and len(self.error_heading) < 200:
            self.error_heading += data[:200 - len(self.error_heading)]


class _YouTube(HTMLParser):
    """Read only data-script text and provider error markers, never execute JS."""
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.scripts = []
        self.title = ""
        self.error_frame = False
        self.branded_icon = False
        self._in_title = False
        self._script = False
        self._current = ""

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if tag == "script":
            self._script, self._current = True, ""
        if tag == "title":
            self._in_title = True
        if tag == "iframe" and isinstance(values.get("src"), str):
            try:
                frame = urlsplit(urljoin("https://www.youtube.com/", values["src"]))
                query = parse_qs(frame.query)
                self.error_frame |= (frame.scheme == "https" and frame.hostname == "www.youtube.com" and
                                     frame.path == "/error" and query.get("src") == ["404"] and query.get("ifr") == ["1"])
            except ValueError:
                pass
        if tag == "link" and "icon" in (values.get("rel") or "").lower().split() and isinstance(values.get("href"), str):
            try:
                icon = urlsplit(values["href"])
                self.branded_icon |= (icon.scheme == "https" and icon.hostname == "www.youtube.com" and
                                      re.fullmatch(r"/img/favicon(?:_[0-9]+)?\.(?:ico|png)", icon.path) is not None)
            except ValueError:
                pass

    def handle_endtag(self, tag):
        if tag == "script":
            if "ytInitialData" in self._current:
                self.scripts.append(self._current)
            self._script, self._current = False, ""
        if tag == "title":
            self._in_title = False

    def handle_data(self, data):
        if self._script and len(self._current) < 4 * 1024 * 1024:
            self._current += data[:4 * 1024 * 1024 - len(self._current)]
        if self._in_title and len(self.title) < 100:
            self.title += data[:100 - len(self.title)]


def _youtube_profile(ctx, profile: dict, response) -> dict:
    media_type = response.headers.get("content-type", "").split(";", 1)[0].strip().lower()
    if media_type not in {"text/html", "application/xhtml+xml"}:
        return _set(profile, "unknown", "YouTube did not return expected public channel HTML")
    page = _YouTube()
    page.feed(response.body.decode("utf-8", errors="replace"))
    page.close()
    parsed = []
    for script in page.scripts:
        for match in re.finditer(r"(?:^|[;\r\n])\s*(?:var\s+)?ytInitialData\s*=\s*", script):
            try:
                payload, _ = json.JSONDecoder().raw_decode(script[match.end():])
                if isinstance(payload, dict):
                    parsed.append(payload)
            except (ValueError, RecursionError):
                pass
    # Live-validated 2026-11-11: a missing handle returns this branded error
    # iframe page. A generic/proxy 404, title alone, or a 200 error never qualifies.
    if response.status == 404 and not parsed and _text(page.title) == "404 Not Found" and page.error_frame and page.branded_icon:
        return _set(profile, "not_found", "YouTube returned its branded 404 page at this public handle lookup URL")
    if response.status != 200 or len(parsed) != 1:
        return _set(profile, "unknown", "YouTube channel data was absent, ambiguous, or returned with an unexpected status")
    metadata = parsed[0].get("metadata")
    metadata = metadata.get("channelMetadataRenderer") if isinstance(metadata, dict) else None
    if not isinstance(metadata, dict):
        return _set(profile, "unknown", "YouTube channel metadata schema was absent or changed")
    stable = metadata.get("externalId")
    if not isinstance(stable, str) or not re.fullmatch(r"UC[A-Za-z0-9_-]{22}", stable):
        return _set(profile, "unknown", "YouTube stable channel ID was missing or invalid")
    owners = metadata.get("ownerUrls", [])
    if not isinstance(owners, list):
        return _set(profile, "unknown", "YouTube owner URL schema changed")
    owners = list(owners)
    if isinstance(metadata.get("vanityChannelUrl"), str):
        owners.append(metadata["vanityChannelUrl"])
    actual = None
    for owner in owners[:100]:
        if not isinstance(owner, str):
            continue
        try:
            parts = urlsplit(web_url(owner))
            if parts.hostname in {"www.youtube.com", "youtube.com", "m.youtube.com"} and parts.path.rstrip("/").lower() == "/@" + profile["username"].lower():
                actual = owner
                break
        except Failure:
            continue
    if actual is None:
        return _set(profile, "unknown", "YouTube metadata did not identify the exact requested handle")
    profile["evidence"] = [{"field": "metadata.channelMetadataRenderer.ownerUrls/vanityChannelUrl",
                            "expected": profile["profile_url"], "actual": actual},
                           {"field": "metadata.channelMetadataRenderer.externalId", "actual": stable}]
    bio = metadata.get("description")
    _metadata(profile, display_name=metadata.get("title"), bio=bio,
              links=_plain_links(bio, ctx.max_results) if isinstance(bio, str) else [])
    return _set(profile, "found", "Public channel metadata matches the exact handle and stable YouTube channel ID")


def _inspect(ctx, site: str, handle: str) -> dict:
    profile = _record(site, handle)
    try:
        username, profile_url, instance = _normalize(site, handle)
        profile.update(username=username, profile_url=profile_url)
        if site == "reddit":
            endpoint = profile_url + "about.json?raw_json=1"
        elif site == "bluesky":
            endpoint = "https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?" + urlencode({"actor": username})
        elif site == "mastodon":
            endpoint = "https://" + instance + "/api/v1/accounts/lookup?" + urlencode({"acct": username.split("@", 1)[0]})
        elif site == "youtube":
            endpoint = profile_url + "/about?hl=en"
        else:
            endpoint = profile_url
        profile["request_url"] = endpoint
        response = ctx.fetch(endpoint, headers={"Accept": "text/html" if site in {"steam", "youtube"} else "application/json",
                                                "Accept-Language": "en-US,en;q=0.9"}, follow_redirects=False)
        profile.update(http_status=response.status, source_ids=[response.source_id])
        state = _transport_state(profile, response)
        if state:
            return state
        if response.url != endpoint:
            return _set(profile, "unknown", "Changed response URL does not establish the requested identity")
        if site == "youtube":
            return _youtube_profile(ctx, profile, response)
        if site == "steam":
            media_type = response.headers.get("content-type", "").split(";", 1)[0].strip().lower()
            if response.status != 200 or media_type not in {"text/html", "application/xhtml+xml"}:
                return _set(profile, "unknown", "Steam did not return the expected public profile HTML")
            page = _Steam()
            page.feed(response.body.decode("utf-8", errors="replace"))
            page.close()
            parsed = []
            for script in page.scripts:
                for match in re.finditer(r"(?:^|[;\r\n])\s*(?:var\s+)?g_rgProfileData\s*=\s*", script):
                    try:
                        payload, _ = json.JSONDecoder().raw_decode(script[match.end():])
                        if isinstance(payload, dict):
                            parsed.append(payload)
                    except (ValueError, RecursionError):
                        pass
            if len(parsed) != 1:
                if not parsed and _text(page.error_heading) == "The specified profile could not be found.":
                    return _set(profile, "not_found", "Steam returned its structured missing-public-profile page")
                return _set(profile, "unknown", "Steam profile JSON was absent, ambiguous, or changed")
            payload = parsed[0]
            actual, stable = payload.get("url"), payload.get("steamid")
            if not isinstance(actual, str) or not isinstance(stable, str) or not re.fullmatch(r"[0-9]{17}", stable):
                return _set(profile, "unknown", "Steam profile URL or stable ID was missing or invalid")
            actual = web_url(actual)
            parts = urlsplit(actual)
            if parts.hostname != "steamcommunity.com" or parts.path.rstrip("/").lower() != "/id/" + username.lower():
                return _set(profile, "unknown", "Steam returned another vanity profile identity")
            if not 76561197960265728 <= int(stable) <= 76561202255233023:
                return _set(profile, "unknown", "Steam stable ID is outside the public individual-account range")
            profile["evidence"] = [{"field": "g_rgProfileData.url", "expected": profile_url, "actual": actual},
                                   {"field": "g_rgProfileData.steamid", "actual": stable}]
            profile["profile_url"] = actual
            _metadata(profile, display_name=payload.get("personaname"))
            return _set(profile, "found", "Public profile JSON matches the vanity URL and stable Steam account ID")

        payload = _json(ctx, response)
        if not isinstance(payload, dict):
            return _set(profile, "unknown", "Provider did not return the expected JSON profile or error schema")
        if site == "reddit":
            if response.status == 404 and payload.get("error") == 404 and payload.get("message") == "Not Found":
                return _set(profile, "not_found", "Reddit returned no visible public profile for this handle")
            body = payload.get("data")
            if response.status != 200 or payload.get("kind") != "t2" or not isinstance(body, dict):
                return _set(profile, "unknown", "Reddit account schema was absent or changed")
            if body.get("is_suspended") is True:
                return _set(profile, "blocked", "Reddit indicates the profile is suspended; public identity could not be verified")
            actual, stable = body.get("name"), body.get("id")
            if not isinstance(actual, str) or actual.lower() != username.lower() or not isinstance(stable, str) or not re.fullmatch(r"[a-z0-9]{1,20}", stable):
                return _set(profile, "unknown", "Reddit profile identity or stable account ID did not match")
            profile["evidence"] = [{"field": "data.name", "expected": username, "actual": actual},
                                   {"field": "data.id", "actual": stable}]
            subreddit = body.get("subreddit")
            if isinstance(subreddit, dict):
                bio = subreddit.get("public_description")
                _metadata(profile, display_name=subreddit.get("title"), bio=bio,
                          links=_plain_links(bio, ctx.max_results) if isinstance(bio, str) else [])
        elif site == "bluesky":
            if response.status in {400, 404} and payload.get("error") in {"InvalidRequest", "NotFound", "ProfileNotFound"} and payload.get("message") == "Profile not found":
                return _set(profile, "not_found", "No profile is visible in this Bluesky public AppView (not proof no AT Protocol account exists)")
            if payload.get("error") in {"AccountTakedown", "AccountDeactivated"}:
                return _set(profile, "blocked", "Bluesky indicates the account is suspended or deactivated")
            actual, stable = payload.get("handle"), payload.get("did")
            if response.status != 200 or not isinstance(actual, str) or actual.lower() != username.lower() or not isinstance(stable, str) or not re.fullmatch(r"did:[a-z0-9]+:[A-Za-z0-9._:%-]{3,512}", stable):
                return _set(profile, "unknown", "Bluesky profile handle or stable DID did not match the expected schema")
            profile["evidence"] = [{"field": "handle", "expected": username, "actual": actual},
                                   {"field": "did", "actual": stable}]
            bio = payload.get("description")
            _metadata(profile, display_name=payload.get("displayName"), bio=bio,
                      links=_plain_links(bio, ctx.max_results) if isinstance(bio, str) else [])
        else:
            if response.status == 404 and payload.get("error") == "Record not found":
                return _set(profile, "not_found", "Home instance returned no visible account record for this address")
            local = username.split("@", 1)[0]
            actual, acct, stable = payload.get("username"), payload.get("acct"), payload.get("id")
            if response.status != 200 or not isinstance(actual, str) or actual.lower() != local.lower() or not isinstance(acct, str) or acct.lower() not in {local.lower(), username.lower()} or not isinstance(stable, str) or not re.fullmatch(r"[0-9]{1,30}", stable) or int(stable) <= 0:
                return _set(profile, "unknown", "Mastodon home-instance account identity or stable ID did not match")
            actual_url = payload.get("url")
            if not isinstance(actual_url, str):
                return _set(profile, "unknown", "Mastodon profile URL was absent")
            actual_url = web_url(actual_url)
            actual_parts = urlsplit(actual_url)
            if actual_parts.hostname != instance or actual_parts.path.rstrip("/").lower() != "/@" + local.lower():
                return _set(profile, "unknown", "Mastodon returned a different profile URL or instance")
            profile["profile_url"] = actual_url
            profile["evidence"] = [{"field": "username", "expected": local, "actual": actual},
                                   {"field": "acct", "expected": username, "actual": acct},
                                   {"field": "id", "actual": stable}]
            note = payload.get("note")
            if isinstance(note, str):
                text = _Text()
                text.feed(note)
                page = _Page(actual_url, ctx.max_results)
                page.feed(note)
                _metadata(profile, display_name=payload.get("display_name"), bio=text.text,
                          links=[link["url"] for link in page.links])
        return _set(profile, "found", "Public profile identity and independent stable ID matched")
    except Failure as failure:
        profile["error"] = failure.as_dict()
        source_id = failure.details.get("source_id")
        if source_id and source_id not in profile["source_ids"]:
            profile["source_ids"].append(source_id)
        return _set(profile, failure.status if failure.status in {"blocked", "rate_limited", "invalid"} else "error", failure.message)


def scan_social(ctx, handle: str, site_ids=None) -> dict:
    """Scan explicit public profile APIs; successful matches need a negative control."""
    if isinstance(site_ids, str):
        site_ids = site_ids.split(",")
    requested = set(site_ids or [site["id"] for site in _SITES])
    known = {site["id"] for site in _SITES}
    if requested - known:
        raise Failure("INVALID_SITE", "Unknown social site ID", "invalid", details={"unknown_sites": sorted(requested - known)})
    profiles, entities, relationships = [], [], []
    for descriptor in _SITES:
        site = descriptor["id"]
        if site not in requested:
            continue
        profile = _inspect(ctx, site, handle)
        if profile["status"] == "found":
            control_handle = "hc" + secrets.token_hex(7)
            if site == "mastodon":
                control_handle += "@" + profile["username"].split("@", 1)[1]
            elif site == "bluesky":
                control_handle += ".bsky.social"
            control = _inspect(ctx, site, control_handle)
            profile["negative_control"] = control
            if control["status"] == "not_found":
                profile["control"] = "passed"
            else:
                profile["control"] = "failed"
                _set(profile, "unknown", "Match withheld: random negative control was " + control["status"] + "; " + control["reason"])
        profiles.append(profile)
        if profile["status"] == "found":
            url = profile["profile_url"]
            scoped_handle = site + ":" + profile["username"]
            entities.extend([{"type": "url", "value": url, "source_ids": profile["source_ids"]},
                             {"type": "username", "value": scoped_handle, "source_ids": profile["source_ids"]}])
            relationships.append({"source": {"type": "url", "value": url}, "relation": "declares_username",
                                  "target": {"type": "username", "value": scoped_handle}, "source_ids": profile["source_ids"]})
            for link in profile.get("public_metadata", {}).get("outbound_links", []):
                entities.append({"type": "url", "value": link, "source_ids": profile["source_ids"]})
                relationships.append({"source": {"type": "url", "value": url}, "relation": "links_to",
                                      "target": {"type": "url", "value": link}, "source_ids": profile["source_ids"]})
    counts = {state: sum(profile["status"] == state for profile in profiles)
              for state in ("found", "not_found", "blocked", "rate_limited", "unknown", "error", "invalid")}
    unresolved = sum(counts[state] for state in ("blocked", "rate_limited", "unknown", "error"))
    status = "invalid" if counts["invalid"] == len(profiles) else "partial" if unresolved else "ok"
    if counts["error"] == len(profiles):
        status = "error"
    return result({"profiles": profiles, "summary": counts,
                   "complete": unresolved == 0 and counts["invalid"] != len(profiles),
                   "scope": "Public profile evidence only; matching handles do not establish a shared owner"},
                  status=status, entities=entities, relationships=relationships)
