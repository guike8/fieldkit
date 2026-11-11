"""Shared result contract. HTTP implementation lives in transport.py."""
from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any
import ipaddress
import re
from urllib.parse import urlsplit, urlunsplit


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


class Failure(Exception):
    def __init__(self, code: str, message: str, status: str = "error", *, retryable: bool = False,
                 details: dict | None = None):
        super().__init__(message)
        self.code, self.message, self.status = code, message, status
        self.retryable, self.details = retryable, details or {}

    def as_dict(self) -> dict:
        return {"code": self.code, "message": self.message, "retryable": self.retryable,
                "details": self.details}


@dataclass
class Response:
    url: str
    status: int
    headers: dict[str, str]
    body: bytes
    source_id: str


def domain_name(value: str) -> str:
    value = value.strip().rstrip(".")
    if not value or len(value) > 253 or any(ch in value for ch in "/\\@:#? \t\r\n"):
        raise Failure("INVALID_DOMAIN", "Supply a DNS domain name without a URL or port", "invalid")
    try:
        normalized = value.encode("idna").decode("ascii").lower()
    except UnicodeError as exc:
        raise Failure("INVALID_DOMAIN", "Domain cannot be normalized with IDNA", "invalid") from exc
    try:
        ipaddress.ip_address(normalized)
    except ValueError:
        pass
    else:
        raise Failure("INVALID_DOMAIN", "Expected a domain name, received an IP address", "invalid")
    if len(normalized) > 253 or any(not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
                                    for label in normalized.split(".")):
        raise Failure("INVALID_DOMAIN", "Domain has an invalid DNS label", "invalid")
    return normalized


def public_ip(value: str) -> str:
    try:
        address = ipaddress.ip_address(value)
    except ValueError as exc:
        raise Failure("INVALID_IP", "Expected an IPv4 or IPv6 address", "invalid") from exc
    if not address.is_global:
        raise Failure("NON_PUBLIC_TARGET", "This command accepts public IP addresses", "invalid")
    return address.compressed


def web_url(value: str) -> str:
    try:
        parts = urlsplit(value)
        port = parts.port
        if port is not None and not 1 <= port <= 65535:
            raise ValueError("URL port must be 1-65535")
        if parts.scheme not in {"http", "https"} or not parts.hostname:
            raise ValueError("Expected an absolute http:// or https:// URL")
        if parts.username is not None or parts.password is not None:
            raise ValueError("Credentials in URLs are not supported")
        if any(ord(ch) < 33 or ord(ch) == 127 for ch in value) or "\\" in value:
            raise ValueError("URL contains whitespace, controls, or a backslash")
        try:
            host = ipaddress.ip_address(parts.hostname).compressed
            if ":" in host:
                host = "[" + host + "]"
        except ValueError:
            host = domain_name(parts.hostname)
        netloc = host + (":" + str(port) if port else "")
        return urlunsplit((parts.scheme.lower(), netloc, parts.path or "/", parts.query, ""))
    except (ValueError, UnicodeError) as exc:
        raise Failure("INVALID_URL", str(exc), "invalid") from exc


def http_failure(response: Response) -> Failure | None:
    if 200 <= response.status < 300:
        return None
    if response.status == 429:
        return Failure("RATE_LIMITED", "Provider rate limit reached", "rate_limited", retryable=True,
                       details={"source_id": response.source_id, "retry_after": response.headers.get("retry-after")})
    if response.status in {401, 403}:
        return Failure("ACCESS_DENIED", "Provider denied access", "blocked", details={"source_id": response.source_id})
    return Failure("HTTP_STATUS", f"Provider returned HTTP {response.status}", "error",
                   retryable=response.status >= 500,
                   details={"source_id": response.source_id, "http_status": response.status})


def result(data: Any, *, status: str = "ok", errors: list[dict] | None = None,
           entities: list[dict] | None = None, relationships: list[dict] | None = None) -> dict:
    return {"status": status, "data": data, "errors": errors or [], "entities": entities or [],
            "relationships": relationships or []}
