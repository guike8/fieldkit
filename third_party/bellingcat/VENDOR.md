# Bellingcat Telegram extractor adaptation

Fieldkit's `fieldkit_core/telegramhelper.py` adapts the public-media selection algorithm from **Bellingcat Auto Archiver**, version **1.3.1**, under the included MIT license.

- Project: <https://github.com/bellingcat/auto-archiver>
- Pinned commit: `0507bf21793c2d3621a31295a7e166c8bfcec3e4`
- Original source: <https://github.com/bellingcat/auto-archiver/blob/0507bf21793c2d3621a31295a7e166c8bfcec3e4/src/auto_archiver/modules/telegram_extractor/telegram_extractor.py>
- Original unmodified source SHA-256: `9399a4d2f3715c50ccebd2c19b138ceeb27c29b48df28d6e20edee57d93ae280`
- Copyright: 2021 Stichting Bellingcat
- License: [MIT](LICENSE)

The adapted class invokes the upstream `download` selection sequence: parse the embed with Beautiful Soup, use its timestamp, prefer the video element, otherwise enumerate Telegram photo background URLs, and construct media/metadata records. Fieldkit changes dependency imports and I/O, uses small `Metadata`/`Media` compatibility classes, and identifies the integration as an adaptation rather than the complete Auto Archiver application.

All HTTP passes through Fieldkit's bounded public-network `Context`. The page is fetched once, validated against its requested public channel/message identity, then passed into the extractor through an instance-local request adapter. The default `download_from_url` adapter records media references only and marks `captured: false`. The explicit `download_media` option captures bounded media bodies from Telegram-owned media hosts into Context evidence, subject to its request, byte, and artifact limits. It does not write arbitrary filenames or invoke Auto Archiver storage, browser, login, plugin, or cloud modules. Use a case to persist captured bodies.

The optional dependency is `beautifulsoup4>=4.12,<5`. Fieldkit's independent public-preview and local-export commands use the Python standard library and do not claim to be Bellingcat tools.
