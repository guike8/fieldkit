# Handlecheck

A C++17 CLI for checking public usernames with explicit profile evidence, useful failure states, and JSON reports.

This first version covers **8 sites**: GitHub, GitLab, Codeberg, DEV, Lichess, Chess.com, Hugging Face, and Hacker News. It prioritizes conservative detection. It does not yet cover the full Sherlock site catalog or major social networks such as Instagram, TikTok, and X. Comparative accuracy against Sherlock has not been benchmarked.

## Run

The included `handlecheck` executable was built on this Mac. From this folder:

```sh
./handlecheck YOUR_USERNAME
./handlecheck YOUR_USERNAME --site github,gitlab,devto
./handlecheck YOUR_USERNAME --json
./handlecheck YOUR_USERNAME --output report.json
./handlecheck --doctor
./handlecheck --list-sites
```

Replace `YOUR_USERNAME` with the exact handle. A single leading `@` is accepted. One username is checked per invocation. No API keys or Python runtime are required to run the compiled command.

`--timeout 10` sets the timeout for each request, in seconds (range 1–60). `--jobs 4` controls concurrency across sites (range 1–8). Requests to each site are sequential. Rate-limited requests are reported without automatic retries; wait for the provider's cooldown before rerunning. Lichess asks callers to wait a full minute after a 429.

## Why the results are more conservative

- A `found` result requires JSON from the expected endpoint, an identity matching the requested username, and the adapter's ID/schema checks. HTTP 200 alone proves nothing.
- Every apparent match is checked against a random likely nonexistent username on the same site. If that control is positive, blocked, or otherwise unresolved, the original match becomes `unknown`.
- Redirects, HTML challenges, malformed responses, missing fields, and unexpected schemas never silently become matches.
- Server failures, DNS/TLS errors, authentication blocks, and rate limits have separate states.
- `--doctor` tests a known public account and a random negative control. Health results describe that run; upstream sites and profiles can change.

Negative controls are a safeguard, not a statistical accuracy score. A control handle could theoretically exist. In that case the result is withheld rather than asserted.

## Interpret the output

| Status | Meaning |
| --- | --- |
| `found` | Matching public profile evidence, with a successful negative control. |
| `not_found` | No visible profile record under the adapter's documented API semantics. |
| `invalid` | This site's lookup format does not accept the supplied handle; no request sent. |
| `blocked` | Authentication, access denial, or an HTML challenge prevented verification. |
| `rate_limited` | The provider's quota prevented verification. |
| `error` | Network, TLS, timeout, response size, or upstream server failure. |
| `unknown` | The response, schema, redirect, or negative control did not establish a conclusion. |

`not_found` does not mean a handle is available for registration. Hacker News excludes accounts without public activity from its API. Hugging Face's adapter covers user accounts, not organization namespaces. GitHub can return users, organizations, or bots. Matching handles across services do not establish that the accounts have the same owner.

Exit codes: **0** for a completed lookup or healthy doctor run, **1** for invalid arguments/output errors (including no compatible selected sites), **2** when any checked site remains unresolved. A failed scan never becomes a successful “zero accounts” result.

## JSON reports

`--json` writes machine-readable output to stdout; progress goes to stderr. `--output FILE` additionally creates a JSON report with owner-only permissions. It refuses to overwrite an existing file or symlink. Reports include timestamps, statuses, request/profile URLs, matching identity evidence, and any negative-control result. Raw response bodies and unrelated profile metadata are not saved.

```sh
./handlecheck YOUR_USERNAME --json > report.json
```

Shell redirection follows your shell's normal overwrite rules. Use `--output report.json` for the command's exclusive file-creation protection.

## Build and test

Requires a C++17 compiler, `make`, and libcurl headers/libraries. JSON parsing is vendored, so the build does not download dependencies. The executable links dynamically to libcurl. Rebuild for another platform or architecture:

```sh
make
make test
```

Tests require Python 3 and use a fake HTTP transport; they do not contact live sites. To rebuild the included executable from scratch, use `make clean` followed by `make`.

`include/handlecheck.hpp` defines the adapter/transport interface. `src/catalog.cpp` defines site endpoints and schema rules. `src/classifier.cpp` interprets responses. `src/http.cpp` handles bounded HTTPS requests without following redirects. `src/main.cpp` implements the CLI, concurrency, controls, and exports.

Add adapters with a known-positive fixture, a documented or live-validated negative response, and regression tests. Verify the adapter with `--doctor --site ID` before treating it as supported.

## Sources

The catalog and `--list-sites` include provider documentation:

- [GitHub users API](https://docs.github.com/en/rest/users/users#get-a-user)
- [GitLab users API](https://docs.gitlab.com/api/users/#list-all-users)
- [Codeberg deployed API](https://codeberg.org/api/swagger)
- [DEV / Forem API](https://developers.forem.com/api/v0)
- [Lichess API](https://lichess.org/api) and [API request guidance](https://lichess.org/page/api-tips)
- [Chess.com public API](https://support.chess.com/en/articles/9650547-what-is-the-pubapi-and-how-do-i-use-it)
- [Hugging Face user overview](https://huggingface.co/docs/huggingface_hub/en/package_reference/hf_api#huggingface_hub.HfApi.get_user_overview)
- [Hacker News users API](https://github.com/HackerNews/API#users)

See [THIRD_PARTY.md](THIRD_PARTY.md) for dependency provenance and licenses.
