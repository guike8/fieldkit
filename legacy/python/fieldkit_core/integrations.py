"""Discover optional adapters without importing dependencies or reading secrets."""
from importlib import metadata
import re
from .common import result


def package_version(name):
    try:
        return metadata.version(name)
    except metadata.PackageNotFoundError:
        return None


def supported_version(value, minimum, exclusive_major):
    if not isinstance(value, str):
        return False
    match = re.fullmatch(r"([0-9]+)\.([0-9]+)(?:\.([0-9]+))?(?:\.post[0-9]+)?", value)
    if not match:
        return False
    parts = tuple(int(part or 0) for part in match.groups())
    return minimum <= parts and parts[0] < exclusive_major


def catalog():
    sdk = package_version("mcp")
    soup = package_version("beautifulsoup4")
    return {"providers": [
        {"id": "mcp", "name": "Official MCP Python SDK", "available": supported_version(sdk, (2, 3, 0), 3),
         "installed_version": sdk, "supported_version_range": ">=2.3,<3", "install_extra": "mcp",
         "source_url": "https://github.com/modelcontextprotocol/python-sdk", "transports": ["stdio", "streamable-http (loopback)"]},
        {"id": "bellingcat_auto_archiver", "name": "Bellingcat Auto Archiver Telegram extractor",
         "available": supported_version(soup, (4, 12, 0), 5), "implementation": "adapted pinned upstream extractor",
         "upstream_version": "1.3.1", "upstream_commit": "0507bf21793c2d3621a31295a7e166c8bfcec3e4",
         "source_url": "https://github.com/bellingcat/auto-archiver/tree/0507bf21793c2d3621a31295a7e166c8bfcec3e4",
         "license": "MIT", "install_extra": "telegram", "parser_dependency": {"name": "beautifulsoup4", "version": soup},
         "commands": ["bellingcat-telegram"], "requires_telegram_login": False},
        {"id": "telegram_public", "name": "Fieldkit public Telegram and local export tools", "available": True,
         "implementation": "native Fieldkit tools", "requires_telegram_login": False,
         "commands": ["telegram-profile", "telegram-messages", "telegram-post", "telegram-export"]},
    ], "extension_contract": "Declare a bounded handler and strict schema in manifest.COMMANDS; MCP discovers it automatically",
        "scope": "Availability describes local dependencies; provider access can still fail or be blocked"}


def integrations(ctx):
    return result(catalog())
