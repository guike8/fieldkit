# Third-party code and integrations

## Bellingcat Auto Archiver TelegramExtractor

- Project: [Bellingcat Auto Archiver](https://github.com/bellingcat/auto-archiver)
- Upstream version: 1.3.1
- Pinned commit: `0507bf21793c2d3621a31295a7e166c8bfcec3e4`
- Source: [telegram_extractor.py at the pinned commit](https://github.com/bellingcat/auto-archiver/blob/0507bf21793c2d3621a31295a7e166c8bfcec3e4/src/auto_archiver/modules/telegram_extractor/telegram_extractor.py)
- License: MIT; retained in [third_party/bellingcat/LICENSE](third_party/bellingcat/LICENSE)
- Adaptation record: [third_party/bellingcat/VENDOR.md](third_party/bellingcat/VENDOR.md)

Fieldkit adapts the public-post extractor to its bounded, evidence-recording HTTP transport and local metadata/media result types. The `bellingcat-telegram` CLI command and `fieldkit_bellingcat_telegram` MCP tool call this adapted extractor. The optional `telegram` extra supplies Beautiful Soup. The full Auto Archiver framework is not installed by this extra.

Fieldkit's other public Telegram helpers and local JSON export analyzer are separate implementations. They should not be attributed to Bellingcat's extractor.

## MCP Python SDK

The optional `mcp` extra uses the [official MCP Python SDK](https://github.com/modelcontextprotocol/python-sdk), tested against 2.3.0, with dependency range `>=2.3,<3`. The SDK is installed as a dependency rather than vendored. Its package carries its own license notices.

## Beautiful Soup

The optional `telegram` extra installs [Beautiful Soup](https://www.crummy.com/software/BeautifulSoup/) (`beautifulsoup4>=4.12,<5`) for the adapted Bellingcat extractor. It is installed as a dependency rather than vendored and carries its own license notices.

## Native Handlecheck dependencies

The native backend vendors JSON for Modern C++ v3.12.0 and dynamically links the system libcurl. See [native/handlecheck/THIRD_PARTY.md](native/handlecheck/THIRD_PARTY.md) for upstream links, the JSON header's SHA-256, and its MIT license.

Dependency licenses describe the corresponding third-party components. They do not assign a license to Fieldkit's original code.
