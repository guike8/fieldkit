"""Machine-readable command contracts; also used to validate stdin jobs."""
from __future__ import annotations
import math
from .common import Failure


def string(description="", **extra):
    return {"type": "string", "minLength": 1, "description": description, **extra}


def array(items, **extra):
    return {"type": "array", "items": items, "minItems": 1, "maxItems": 50, **extra}


COMMANDS = {
    "username": {"description": "Verify public handles and return separate unverified social candidate links",
                 "options": {"sites": array(string()), "include_candidates": {"type": "boolean", "default": True}}},
    "person": {"description": "Create name/context search queries; optionally run public web search without merging identities",
               "options": {"context": {"type": "string", "maxLength": 200, "default": ""},
                           "search": {"type": "boolean", "default": False}}},
    "profile": {"description": "Verify supported public profile URLs; inspect other pages as unverified URL evidence", "options": {}},
    "search": {"description": "Public web search via optional BRAVE_SEARCH_API_KEY", "options": {
        "limit": {"type": "integer", "minimum": 1, "maximum": 20}}},
    "dns": {"description": "Public DNS-over-HTTPS records including reverse PTR", "options": {
        "types": array({"type": "string", "enum": ["A", "AAAA", "MX", "NS", "TXT", "CAA", "CNAME", "SOA", "PTR", "SRV"]})}},
    "rdap": {"description": "Authoritative registration data using IANA bootstrap", "options": {
        "kind": {"type": "string", "enum": ["auto", "domain", "ip", "asn"], "default": "auto"}}},
    "tls": {"description": "Verified public TLS leaf certificate and connection metadata", "options": {
        "port": {"type": "integer", "minimum": 1, "maximum": 65535, "default": 443}}},
    "ct": {"description": "Historical names observed in public certificate logs, not proof of current DNS", "options": {}},
    "http": {"description": "Bounded public page metadata, outbound links, headers, and source hashes", "options": {}},
    "archive": {"description": "Bounded Internet Archive CDX index observations; no replay crawling", "options": {
        "limit": {"type": "integer", "minimum": 1, "maximum": 500}, "prefix": {"type": "boolean", "default": False}}},
    "email": {"description": "Email syntax and domain MX/SPF/DMARC; does not probe mailbox existence", "options": {}},
    "file": {"description": "Local regular-file hashes, size, format signatures and image dimensions", "local_target": True, "options": {}},
    "extract": {"description": "Extract unverified indicators from a local UTF-8 evidence file with offsets", "local_target": True, "options": {
        "kinds": array({"type": "string", "enum": ["url", "email", "domain", "ipv4", "ipv6"]})}},
    "domain": {"description": "Compose supporting domain modules with individually reported failures", "options": {
        "modules": array({"type": "string", "enum": ["dns", "rdap", "ct", "tls", "http"]}, default=["dns", "rdap"]) }},
    "doctor": {"description": "Check runtime, native backend, and optional key availability without network requests",
               "target_required": False, "options": {}},
    "integrations": {"description": "Describe installed optional integrations, upstream provenance, and supported operations",
                     "target_required": False, "options": {}},
    "telegram-profile": {"description": "Public Telegram profile/channel metadata from an exact handle or t.me URL", "options": {}},
    "telegram-messages": {"description": "One bounded public Telegram channel preview page with message provenance",
                          "options": {"limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20},
                                      "before": {"type": "integer", "minimum": 1, "maximum": 2147483647},
                                      "query": string(maxLength=200)}},
    "telegram-post": {"description": "Inspect a single public Telegram post URL without logging in", "options": {}},
    "telegram-export": {"description": "Analyze a local Telegram Desktop JSON export; records never leave the local process",
                        "local_target": True, "options": {
                            "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50},
                            "query": string(maxLength=200)}},
    "bellingcat-telegram": {"description": "Bellingcat Auto Archiver Telegram extractor, pinned MIT adaptation with bounded public I/O",
                           "integration": "bellingcat_auto_archiver", "options": {
                               "download_media": {"type": "boolean", "default": False},
                               "max_media": {"type": "integer", "minimum": 1, "maximum": 10, "default": 3}}},
}

STATUSES = ["ok", "partial", "not_found", "blocked", "rate_limited", "unknown", "unavailable", "invalid", "error"]
OUTPUT_SCHEMA = {"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object",
    "required": ["schema_version", "run_id", "command", "target", "status", "data", "sources", "errors", "timing"],
    "properties": {"schema_version": {"const": "1.0"}, "run_id": string(), "command": string(),
                   "target": {"type": ["string", "null"]}, "status": {"enum": STATUSES},
                   "data": {}, "sources": {"type": "array"}, "errors": {"type": "array"},
                   "entities": {"type": "array"}, "relationships": {"type": "array"}, "timing": {"type": "object"}}}


def command_schema(command: str) -> dict:
    descriptor = COMMANDS[command]
    required = ["command"] + (["target"] if descriptor.get("target_required", True) else [])
    return {"type": "object", "additionalProperties": False, "required": required,
            "properties": {"command": {"const": command}, "target": string(maxLength=8192),
                           "options": {"type": "object", "additionalProperties": False,
                                       "properties": descriptor["options"]}}}


def validate(value, schema: dict, path="job"):
    expected = schema.get("type")
    types = {"object": dict, "array": list, "string": str, "boolean": bool, "integer": int}
    if expected in types:
        if not isinstance(value, types[expected]) or (expected == "integer" and isinstance(value, bool)):
            raise Failure("INVALID_JOB", f"{path} must be {expected}", "invalid")
    if "const" in schema and value != schema["const"]:
        raise Failure("INVALID_JOB", f"{path} has an invalid constant", "invalid")
    if "enum" in schema and value not in schema["enum"]:
        raise Failure("INVALID_JOB", f"{path} must be one of {schema['enum']}", "invalid")
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False and set(value) - set(properties):
            raise Failure("INVALID_JOB", f"{path} has unsupported fields: {sorted(set(value) - set(properties))}", "invalid")
        for field in schema.get("required", []):
            if field not in value:
                raise Failure("INVALID_JOB", f"{path}.{field} is required", "invalid")
        for field, item in value.items():
            if field in properties:
                validate(item, properties[field], path + "." + field)
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0) or len(value) > schema.get("maxItems", 10000):
            raise Failure("INVALID_JOB", f"{path} has an invalid array length", "invalid")
        for index, item in enumerate(value):
            validate(item, schema.get("items", {}), f"{path}[{index}]")
    if isinstance(value, str) and (len(value) < schema.get("minLength", 0) or len(value) > schema.get("maxLength", 1000000)):
        raise Failure("INVALID_JOB", f"{path} has an invalid string length", "invalid")
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if (isinstance(value, float) and not math.isfinite(value)) or value < schema.get("minimum", -math.inf) or value > schema.get("maximum", math.inf):
            raise Failure("INVALID_JOB", f"{path} is outside its numeric range", "invalid")


def validate_job(job) -> dict:
    if not isinstance(job, dict) or not isinstance(job.get("command"), str) or job.get("command") not in COMMANDS:
        raise Failure("INVALID_JOB", "Expected a job with a supported command; consult capabilities", "invalid")
    validate(job, command_schema(job["command"]))
    return job
