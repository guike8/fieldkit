# Validation — November 11, 2026

## Version 0.4.0: practitioner workbench

**280 Python tests passed with zero failures/skips**, including 23 shared-workbench model tests, 12 Textual interaction tests, the real loopback browser API, and MCP workspace operations. JavaScript syntax and Node frontend workflow checks also passed.

Browser verification used the real local case API: capture source evidence, save a separate assessment, create a research flow, add/run a step, observe its saved evidence, inspect graph provenance, and accept a shared-reference lead with written limitations. Case hash/reference verification passed after those actions. The TUI's real-model tests exercise the same saved flow and evidence model.

The installable app shares a case directory with the TUI and agent tools. MCP exposes 22 base tools, or 37 with a fixed case, including evidence, entities, flows, correlation review, notes, and workspace export. The hosted synthetic preview was successfully deployed; browser verification of that hosted URL stopped at its ChatGPT sign-in gate. Local browser workflows were fully checked.

See [the v0.4 check summary](checks-2026-11-11-v04.json).

## Version 0.3.0: MCP and Telegram

The extended suite passed **233 tests with no failures or skips**, including real official-SDK modern/legacy stdio clients and a loopback Streamable HTTP client. The installable package exposes 22 MCP tools, or 24 when fixed-case tools are configured.

Six end-to-end MCP calls passed: Telegram profile, bounded channel messages, exact post, the actual adapted Bellingcat extractor, a synthetic local Desktop export, and case verification. Bellingcat's pinned parser extracted one media reference from the official Telegram post. Explicit media-body capture was tested with bounded binary fixtures. The development case verified five reports and four captured artifacts.

See [the v0.3 check summary](checks-2026-11-11-v03.json). Optional integration versions were MCP SDK 2.3.0 and Beautiful Soup 4.15.0. These tests require no Telegram login or API credentials.

## Initial CLI validation

The Python suite passed **177 offline tests**, covering provider fixtures, negative controls, transport limits, profile routing, strict JSON jobs, batch behavior, output protection, file extraction, and case integrity.

An end-to-end batch ran 15 public-fixture jobs:

- **Verified with successful negative controls:** GitHub `octocat`, Bluesky `bsky.app`, YouTube `Google`, Steam `gabelogannewell`, and Mastodon `@Gargron@mastodon.social`.
- **Completed:** DNS, authoritative domain RDAP, HTTP metadata, verified TLS, archive index, email-domain configuration, local file metadata, and local indicator extraction.
- **Unresolved as reported:** Reddit denied anonymous access with HTTP 403; crt.sh exceeded the configured 8-second timeout. Neither became a negative or a match.
- **Case verification:** 15 stored reports, 21 artifacts, and 21 source references passed hash/reference verification.

The optional Brave search API was fixture-tested, but not live-tested because no API key was configured.

See [checks-2026-11-11.json](checks-2026-11-11.json) for the per-command summary. Full raw fixture evidence was kept in the development case and is not bundled. The check summary preserves statuses without redistributing unrelated public profile biographies or page bodies.

The inherited native Handlecheck engine also retains its source and regression suite. Its eight adapters passed the earlier live health check in this session; the integrated CLI subsequently exercised its GitHub adapter.

These checks validate bounded fixtures and failure handling. They do not establish a universal accuracy rate, current account ownership, or future provider availability.
