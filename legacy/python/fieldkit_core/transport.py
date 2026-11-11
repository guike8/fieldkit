"""Bounded GET transport with DNS-pinned public connections and source capture."""
from __future__ import annotations

import hashlib
import http.client
import ipaddress
import json
import math
import socket
import ssl
import threading
import time
import uuid
from urllib.parse import quote, urljoin, urlsplit

from .common import Failure, Response, utc_now, web_url


_RESOLVER_SLOTS = threading.BoundedSemaphore(8)


def resolve_public(host: str, port: int, timeout: float = 10) -> list[tuple]:
    # Daemon workers bound OS resolver stalls without making interpreter exit wait
    # for getaddrinfo. A fixed semaphore prevents unbounded resolver threads.
    started = time.monotonic()
    if not _RESOLVER_SLOTS.acquire(timeout=timeout):
        raise Failure("DNS_TIMEOUT", "DNS resolver capacity timed out", retryable=True)
    finished = threading.Event()
    answer = []
    def lookup():
        try:
            answer.append(socket.getaddrinfo(host, port, type=socket.SOCK_STREAM))
        except OSError as exc:
            answer.append(exc)
        finally:
            _RESOLVER_SLOTS.release()
            finished.set()
    threading.Thread(target=lookup, daemon=True).start()
    if not finished.wait(max(0, timeout - (time.monotonic() - started))):
        raise Failure("DNS_TIMEOUT", f"DNS resolution timed out for {host}", retryable=True)
    if not answer or isinstance(answer[0], OSError):
        raise Failure("DNS_ERROR", f"Could not resolve {host}", retryable=True)
    records = answer[0]
    if not records:
        raise Failure("DNS_ERROR", f"No address returned for {host}", retryable=True)
    unique = []
    for family, kind, protocol, _, address in records:
        ip = ipaddress.ip_address(address[0].split("%", 1)[0])
        if not ip.is_global:
            raise Failure("NON_PUBLIC_TARGET", "Resolved address is outside the public Internet", "invalid")
        entry = (family, kind, protocol, address)
        if entry not in unique:
            unique.append(entry)
    return unique


def connect_public(host: str, port: int, timeout: float) -> socket.socket:
    deadline = time.monotonic() + timeout
    records = resolve_public(host, port, timeout)
    last = None
    for family, kind, protocol, address in records:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        connection = socket.socket(family, kind, protocol)
        connection.settimeout(remaining)
        try:
            connection.connect(address)
            return connection
        except OSError as exc:
            connection.close()
            last = exc
    raise Failure("CONNECT_ERROR", f"Could not connect to {host}:{port}", retryable=True) from last


class PinnedConnection(http.client.HTTPConnection):
    def __init__(self, host: str, port: int, timeout: float, secure: bool):
        super().__init__(host, port, timeout=timeout)
        self.secure = secure

    def connect(self):
        deadline = time.monotonic() + self.timeout
        connection = connect_public(self.host, self.port, self.timeout)
        try:
            connection.settimeout(max(0.001, deadline - time.monotonic()))
            self.sock = ssl.create_default_context().wrap_socket(connection, server_hostname=self.host) if self.secure else connection
        except BaseException:
            connection.close()
            raise


class Context:
    def __init__(self, *, timeout: float = 10, max_results: int = 50, max_requests: int = 64,
                 max_bytes: int = 4 * 1024 * 1024, max_file_bytes: int = 20 * 1024 * 1024):
        self.timeout, self.max_results, self.max_requests = timeout, max_results, max_requests
        self.max_bytes, self.max_file_bytes = max_bytes, max_file_bytes
        self.sources: list[dict] = []
        self.artifacts: dict[str, bytes] = {}
        self._lock = threading.RLock()
        self._requests = 0

    def reserve_requests(self, count: int = 1):
        with self._lock:
            if self._requests + count > self.max_requests:
                raise Failure("REQUEST_BUDGET", "Per-command network request budget exhausted", "partial")
            self._requests += count

    def add_inline_source(self, name: str, body: bytes, *, kind: str = "tool", metadata: dict | None = None) -> str:
        digest = hashlib.sha256(body).hexdigest()
        identifier = "src_" + uuid.uuid4().hex
        record = {"source_id": identifier, "kind": kind, "name": name, "observed_at": utc_now(),
                  "sha256": digest, "bytes": len(body), "untrusted_content": True}
        record.update(metadata or {})
        with self._lock:
            # Bounded response bodies are retained for optional local case storage.
            if digest not in self.artifacts and sum(map(len, self.artifacts.values())) + len(body) > 32 * 1024 * 1024:
                raise Failure("EVIDENCE_BUDGET", "Per-command source capture exceeded 32 MiB", "partial")
            self.artifacts[digest] = body
            self.sources.append(record)
        return identifier

    def add_local_source(self, path: str, sha256: str, size: int, media_type: str) -> str:
        identifier = "src_" + uuid.uuid4().hex
        with self._lock:
            self.sources.append({"source_id": identifier, "kind": "local_file", "path": str(path),
                                 "observed_at": utc_now(), "sha256": sha256, "bytes": size,
                                 "media_type": media_type, "raw_copied": False, "untrusted_content": True})
        return identifier

    def _record(self, url: str, status: int, headers: dict, body: bytes, *, error: str | None = None,
                truncated: bool = False) -> str:
        allowed = {"content-type", "content-length", "etag", "last-modified", "cache-control",
                   "date", "location", "retry-after", "content-encoding"}
        metadata = {"url": url, "http_status": status, "headers": {k: v for k, v in headers.items() if k in allowed},
                    "truncated": truncated}
        if error:
            metadata["error"] = error
        return self.add_inline_source(url, body, kind="http", metadata=metadata)

    def fetch(self, url: str, headers: dict | None = None, follow_redirects: bool = True) -> Response:
        current = web_url(url)
        request_headers = {"User-Agent": "Fieldkit/0.2 (+public-source-research)",
                           "Accept": "application/json", "Accept-Encoding": "identity"}
        request_headers.update(headers or {})
        visited = set()
        for hop in range(6):
            if current in visited:
                raise Failure("REDIRECT_LOOP", "Provider returned a redirect loop")
            visited.add(current)
            self.reserve_requests()
            parts = urlsplit(current)
            connection = PinnedConnection(parts.hostname, parts.port or (443 if parts.scheme == "https" else 80),
                                          self.timeout, parts.scheme == "https")
            path = quote(parts.path or "/", safe="/%:@!$&'()*+,;=-._~")
            if parts.query:
                path += "?" + quote(parts.query, safe="/%?:@!$&'()*+,;=-._~")
            try:
                deadline = time.monotonic() + self.timeout
                connection.request("GET", path, headers=request_headers)
                if connection.sock:
                    connection.sock.settimeout(max(0.001, deadline - time.monotonic()))
                response = connection.getresponse()
                response_headers = {k.lower(): v for k, v in response.getheaders()}
                length = response_headers.get("content-length", "")
                if length.isdigit() and int(length) > self.max_bytes:
                    source = self._record(current, response.status, response_headers, b"", truncated=True)
                    raise Failure("RESPONSE_TOO_LARGE", "Response exceeds the configured byte limit",
                                  details={"source_id": source})
                chunks, size = [], 0
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError("Response read timeout")
                    if connection.sock:
                        connection.sock.settimeout(remaining)
                    chunk = response.read1(min(65536, self.max_bytes - size + 1))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    size += len(chunk)
                    if size > self.max_bytes:
                        source = self._record(current, response.status, response_headers,
                                              b"".join(chunks)[:self.max_bytes], truncated=True)
                        raise Failure("RESPONSE_TOO_LARGE", "Response exceeds the configured byte limit",
                                      details={"source_id": source})
                body = b"".join(chunks)
                if length.isdigit() and "chunked" not in response_headers.get("transfer-encoding", "").lower() and len(body) != int(length):
                    source = self._record(current, response.status, response_headers, body, truncated=True)
                    raise Failure("INCOMPLETE_RESPONSE", "Response ended before its declared length", details={"source_id": source})
                source = self._record(current, response.status, response_headers, body)
                if follow_redirects and response.status in {301, 302, 303, 307, 308}:
                    location = response_headers.get("location")
                    if not location:
                        raise Failure("INVALID_REDIRECT", "Redirect has no Location header", details={"source_id": source})
                    if hop == 5:
                        raise Failure("REDIRECT_LIMIT", "Provider exceeded five redirects", details={"source_id": source})
                    destination = web_url(urljoin(current, location))
                    if parts.scheme == "https" and urlsplit(destination).scheme != "https":
                        raise Failure("INSECURE_REDIRECT", "HTTPS redirect downgraded to HTTP", details={"source_id": source})
                    current = destination
                    # Never forward API credentials, cookies, or custom headers on a redirect.
                    request_headers = {"User-Agent": request_headers["User-Agent"],
                                       "Accept": request_headers["Accept"], "Accept-Encoding": "identity"}
                    continue
                return Response(current, response.status, response_headers, body, source)
            except Failure as exc:
                if "source_id" not in exc.details:
                    exc.details["source_id"] = self._record(current, 0, {}, b"", error=exc.code)
                raise
            except ssl.SSLError as exc:
                source = self._record(current, 0, {}, b"", error="TLS_ERROR")
                raise Failure("TLS_ERROR", "TLS certificate or handshake verification failed", details={"source_id": source}) from exc
            except (TimeoutError, socket.timeout) as exc:
                source = self._record(current, 0, {}, b"", error="TIMEOUT")
                raise Failure("TIMEOUT", "Network operation timed out", retryable=True, details={"source_id": source}) from exc
            except (OSError, http.client.HTTPException, ValueError) as exc:
                source = self._record(current, 0, {}, b"", error="NETWORK_ERROR")
                raise Failure("NETWORK_ERROR", "HTTP request failed", retryable=True, details={"source_id": source}) from exc
            finally:
                connection.close()
        raise Failure("REDIRECT_LIMIT", "Redirect limit exceeded")

    def json(self, response: Response):
        media_type = response.headers.get("content-type", "").split(";", 1)[0].strip().lower()
        if media_type not in {"application/json", "application/dns-json", "application/rdap+json"} and not ("/" in media_type and media_type.endswith("+json")):
            raise Failure("UNEXPECTED_CONTENT", "Provider did not return JSON", "unknown",
                          details={"source_id": response.source_id, "content_type": media_type})
        try:
            return strict_json(response.body.decode("utf-8"))
        except (ValueError, UnicodeError, RecursionError) as exc:
            raise Failure("INVALID_JSON", "Provider returned malformed or excessively nested JSON", "unknown",
                          details={"source_id": response.source_id}) from exc

    def tls_info(self, host: str, port: int = 443) -> dict:
        if not isinstance(port, int) or not 1 <= port <= 65535:
            raise Failure("INVALID_PORT", "Port must be 1-65535", "invalid")
        self.reserve_requests()
        connection = None
        try:
            connection = connect_public(host, port, self.timeout)
            with ssl.create_default_context().wrap_socket(connection, server_hostname=host) as secure:
                certificate = secure.getpeercert()
                der = secure.getpeercert(binary_form=True)
                source = self.add_inline_source(f"tls://{host}:{port}", der, kind="tls_certificate",
                                                metadata={"host": host, "port": port, "verified": True})
                return {"host": host, "port": port, "verified": True, "subject": certificate.get("subject"),
                        "issuer": certificate.get("issuer"), "serial_number": certificate.get("serialNumber"),
                        "not_before": certificate.get("notBefore"), "not_after": certificate.get("notAfter"),
                        "subject_alt_names": certificate.get("subjectAltName", [])[:self.max_results],
                        "protocol": secure.version(), "cipher": secure.cipher(),
                        "leaf_sha256": hashlib.sha256(der).hexdigest(), "source_id": source}
        except Failure as exc:
            if "source_id" not in exc.details:
                exc.details["source_id"] = self.add_inline_source(f"tls://{host}:{port}", b"", kind="tls_attempt",
                    metadata={"host": host, "port": port, "error": exc.code})
            raise
        except ssl.SSLError as exc:
            source = self.add_inline_source(f"tls://{host}:{port}", b"", kind="tls_attempt", metadata={"error": "TLS_ERROR"})
            raise Failure("TLS_ERROR", "TLS verification or handshake failed", details={"source_id": source}) from exc
        except OSError as exc:
            source = self.add_inline_source(f"tls://{host}:{port}", b"", kind="tls_attempt", metadata={"error": "NETWORK_ERROR"})
            raise Failure("NETWORK_ERROR", "TLS connection failed", retryable=True, details={"source_id": source}) from exc
        finally:
            if connection is not None:
                connection.close()


def strict_json(text: str):
    depth, quoted, escaped = 0, False, False
    for character in text:
        if quoted:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quoted = False
        elif character == '"':
            quoted = True
        elif character in "[{":
            depth += 1
            if depth > 128:
                raise ValueError("JSON nesting exceeds 128")
        elif character in "]}":
            depth -= 1
    def reject_constant(value):
        raise ValueError("Nonfinite JSON number")
    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError("Duplicate JSON key")
            value[key] = item
        return value
    def finite_float(value):
        number = float(value)
        if not math.isfinite(number):
            raise ValueError("Nonfinite JSON number")
        return number
    return json.loads(text, object_pairs_hook=unique_object, parse_constant=reject_constant, parse_float=finite_float)
