# Third-party code and integrations

## Native Rust dependencies

Cargo.toml declares the runtime dependencies and Cargo.lock pins exact versions. They include Ratatui/Crossterm (terminal UI), rusqlite/SQLite (cases), reqwest/rustls (public HTTP and verified TLS), scraper (HTML parsing), serde (structured records), and tiny_http (local application server). Dependency licenses apply to their corresponding components; Cargo retains their package metadata and source notices.

## Bellingcat Auto Archiver TelegramExtractor

The `bellingcat-telegram` command and MCP tool implement native Rust public-post extraction informed by Fieldkit's earlier adaptation of [Bellingcat Auto Archiver](https://github.com/bellingcat/auto-archiver), version 1.3.1, pinned commit `0507bf21793c2d3621a31295a7e166c8bfcec3e4`.

The [pinned upstream extractor](https://github.com/bellingcat/auto-archiver/blob/0507bf21793c2d3621a31295a7e166c8bfcec3e4/src/auto_archiver/modules/telegram_extractor/telegram_extractor.py) is MIT licensed. Its license and original adaptation record remain under [third_party/bellingcat](third_party/bellingcat/VENDOR.md). The Rust command retains bounded public I/O, post provenance, and optional media collection. It does not install or execute the Auto Archiver framework or a Python extractor. Other Telegram helpers are separate Fieldkit implementations.

## Historical dependencies

The earlier Python/C++ implementations are archived under `legacy/`. Their code retains notices for the MCP Python SDK, Beautiful Soup, JSON for Modern C++, and libcurl. See [legacy/native/handlecheck/THIRD_PARTY.md](legacy/native/handlecheck/THIRD_PARTY.md). They are not current runtime dependencies.

## NVIDIA Auto Ontology

The Rust adapter calls the actual [NVIDIA Auto Ontology](https://github.com/NVIDIA/auto-ontology) gateway. The optional local stack builds upstream commit `fc43a82600e18b19d159c52023f54f5572f537ac` under its Apache-2.0 license. The upstream service runs separately and reads a normalized projection; Fieldkit remains the authoritative evidence ledger. Its models may receive selected projected records and metadata when NVIDIA mode is enabled. No NVIDIA source is vendored into the Rust crate. See [integration setup and provenance](integrations/auto-ontology/README.md).

Dependency notices do not assign a license to Fieldkit's original code.
