# Ronin 2022 candidate — rejected for this live trial

This was a public-data **feasibility candidate**, not a completed benchmark. No claim that Fieldkit solved the incident or traced the funds.

Checked on 2026-11-13 through actual Fieldkit MCP:

- Blockscout transaction JSON: HTTP 200, retained source SHA-256 `68ac2e406804763e77df3c75fe2def1ff491d7e6457fbb6b835c4d11639227c8`. The outer transaction value is zero. A decoded call argument and token-burning event are not substitutes for retained internal ETH-transfer evidence.
- Blockscout internal-transactions request: timed out on its only attempt; no response bytes were retained. No retry.
- Separate single-seed Etherscan trial: HTTP 403 with `cf-mitigated: challenge`, retained source SHA-256 `951d150fe0cd0e1348479e965a9672d6ffb40428f237f6b04264e5f47d4af760`. No bypass or retry.

The web reader could display internal transfers, but that did **not** establish that Fieldkit's collector could retrieve their original bytes. Its output was not silently substituted into either case.

Both stopped cases passed offline MCP integrity verification and independent SQLite report/source-hash checks. They contain no findings, reviews or accepted summary items. Raw cases remain outside git:

- `/Users/guize/fieldkit-ronin-feasibility-uog9a8lj` — two explicitly supplied endpoints, **not** seed discovery.
- `/Users/guize/fieldkit-ronin-public-1tz4hziq` — one supplied Etherscan seed, blocked at the first step.

`task.json` preserves the attempted Etherscan task, not a runnable successful benchmark. Discovery and agent-performance credit: **none**. Do not automatically resume these ledgers or treat access failures as absence of transfers. The replacement public-source trial is [event-stream 2018](../event-stream-2018/).
