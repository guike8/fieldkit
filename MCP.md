# Fieldkit MCP server

Fieldkit exposes its command manifest as individually named MCP tools. Tool results contain the same structured evidence envelope as the CLI: status, data, sources, errors, entities, relationships, and timing.

## Install

Requires Python **3.11+**. From the repository:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -e '.[mcp,telegram]'
```

The `mcp` extra installs the official MCP Python SDK (`>=2.3,<3`); `telegram` installs Beautiful Soup for the adapted Bellingcat Telegram extractor. The core CLI and native public Telegram helpers use the standard library.

The native username backend includes a macOS arm64 executable. Rebuild it on another platform with `make -C native/handlecheck`. An installed wheel also carries its C++ source and Makefile under the environment's `share/fieldkit/native/handlecheck` directory; `fieldkit doctor` reports backend availability.

## Connect a client over stdio

Create the workspace directory before starting the server. Copy [examples/mcp-config.json](examples/mcp-config.json) into your MCP client's configuration and replace its paths:

```json
{
  "mcpServers": {
    "fieldkit": {
      "command": "/absolute/path/to/fieldkit/.venv/bin/fieldkit-mcp",
      "args": [
        "--workspace", "/absolute/path/to/investigations",
        "--case", "/absolute/path/to/investigations/cases/default"
      ]
    }
  }
}
```

Use absolute paths because MCP clients often start servers from another working directory. Keep any optional `BRAVE_SEARCH_API_KEY` in the client's process environment. Telegram's public helpers and the adapted Bellingcat extractor need no Telegram API credentials.

The equivalent manual invocation is:

```sh
fieldkit-mcp --workspace /absolute/path/to/investigations --case /absolute/path/to/investigations/cases/default
```

`python -m fieldkit_core.mcp_server` uses the same arguments. The default transport is `stdio`; stdout is reserved for MCP protocol messages. Diagnostic logs go to stderr. Starting this command in a terminal leaves it waiting for an MCP client.

## Discover and call tools

Use the client's MCP `tools/list` operation. Every command in Fieldkit's manifest becomes `fieldkit_<command>`, with hyphens normalized to underscores. Options are flattened into the tool's arguments:

```json
{"name":"fieldkit_username","arguments":{"target":"octocat","sites":["github"],"include_candidates":false}}
```

```json
{"name":"fieldkit_telegram_messages","arguments":{"target":"telegram","limit":10}}
```

```json
{"name":"fieldkit_bellingcat_telegram","arguments":{"target":"https://t.me/telegram/1","download_media":false,"max_media":3}}
```

`fieldkit_capabilities` exposes the current command/provider manifest. When a fixed `--case` directory is configured, `fieldkit_case_show` and `fieldkit_case_verify` inspect that case. Run `tools/list` to see the exact installed schemas; optional dependencies and provider availability are reported in results.

Results include JSON `structuredContent` and a text representation. Failed or incomplete tool operations remain structured and set MCP `isError`; inspect the envelope's status and error codes. A missing provider result, blocked page, or rate limit does not establish absence of an account.

## Workspace and case behavior

The server's `--workspace` confines local-file and Telegram-export reads. Paths resolving outside it, including symlink escapes, are rejected. The case directory is set by the server operator and stays inside that workspace. Tool arguments cannot choose arbitrary output paths or invoke shell commands.

With `--case`, calls append reports and bounded captured sources to the fixed local case. That local write is reflected in MCP tool annotations. A case hash check verifies stored bytes/references; it does not authenticate a provider's claims. External posts, bios, URLs, and file text remain untrusted evidence, not agent instructions.

The server defaults to two concurrent calls. Network limits are configurable with `--timeout`, `--max-requests`, `--max-results`, and `--max-bytes`; see `fieldkit-mcp --help` for ranges.

## Optional local HTTP transport

```sh
fieldkit-mcp --workspace /absolute/path/to/investigations --transport streamable-http --host 127.0.0.1 --port 8000
```

Connect the client's Streamable HTTP transport to **`http://127.0.0.1:8000/mcp`**. HTTP mode accepts loopback hosts only (`127.0.0.1`, `localhost`, or `::1`); `localhost` is bound as literal loopback. It is intended for a client on the same machine. Remote hosting/authentication is a separate deployment concern; this server does not expose an unauthenticated public listener.

## Bellingcat integration scope

`fieldkit_bellingcat_telegram` calls Fieldkit's adapted copy of Bellingcat Auto Archiver's MIT-licensed `TelegramExtractor`, pinned to upstream commit `0507bf21793c2d3621a31295a7e166c8bfcec3e4`. It extracts public Telegram post metadata and media references. Media body capture is opt-in and bounded.

This integration packages the extractor's algorithm with a bounded Fieldkit transport and small metadata/media adapters. It does not install the full Auto Archiver application. Native `telegram-profile`, `telegram-messages`, `telegram-post`, and `telegram-export` are separate Fieldkit helpers. See [THIRD_PARTY.md](THIRD_PARTY.md) and [the vendoring record](third_party/bellingcat/VENDOR.md) for source and license details.
