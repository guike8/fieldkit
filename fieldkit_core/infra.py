"""Public DNS, authoritative RDAP, TLS, certificate-log and mail-domain lookups.

Provider contracts: developers.google.com/speed/public-dns/docs/doh/json;
data.iana.org/rdap/; RFCs 9082/9083; crt.sh's public JSON search.
All I/O goes through the bounded, evidence-recording Context transport.
"""
from __future__ import annotations

import ipaddress
import re
import shlex
from urllib.parse import quote, urlencode, urlsplit

from .common import Failure, domain_name, http_failure, public_ip, result

RR_TYPES = {"A": 1, "NS": 2, "CNAME": 5, "SOA": 6, "PTR": 12, "MX": 15,
            "TXT": 16, "AAAA": 28, "SRV": 33, "CAA": 257}
RR_NAMES = {value: key for key, value in RR_TYPES.items()}
DEFAULT_TYPES = ("A", "AAAA", "MX", "NS", "TXT", "CAA")


def _integer(value, minimum=0):
    return isinstance(value, int) and not isinstance(value, bool) and value >= minimum


def _entity(kind, value, source_ids):
    return {"type": kind, "value": str(value), "source_ids": list(source_ids)}


def _edge(source_type, source_value, relation, target_type, target_value, source_ids):
    return {"source": {"type": source_type, "value": str(source_value)}, "relation": relation,
            "target": {"type": target_type, "value": str(target_value)}, "source_ids": list(source_ids)}


def _schema(message, source_id=None):
    return Failure("UNEXPECTED_SCHEMA", message, "unknown",
                   details={"source_id": source_id} if source_id else {})


def _json_response(ctx, response):
    failure = http_failure(response)
    if failure:
        raise failure
    return ctx.json(response)


def _dns_name(value):
    # Underscored service labels are legitimate DNS names (DMARC/SRV etc.).
    if "_" not in value:
        return domain_name(value)
    if len(value) > 253 or any(ch in value for ch in "/\\@:#? \t\r\n"):
        raise Failure("INVALID_DOMAIN", "Invalid DNS query name", "invalid")
    labels = value.rstrip(".").split(".")
    normalized = []
    for label in labels:
        if label.startswith("_") and re.fullmatch(r"_[A-Za-z0-9-]{1,62}", label):
            normalized.append(label.lower())
        else:
            normalized.append(domain_name(label))
    return ".".join(normalized)


def _combined_status(items):
    statuses = [item["status"] for item in items]
    if not statuses:
        return "error"
    if all(status == "not_found" for status in statuses):
        return "not_found"
    if all(status in {"ok", "not_found"} for status in statuses):
        return "ok"
    if any(status in {"ok", "not_found", "partial"} for status in statuses):
        return "partial"
    if len(set(statuses)) == 1:
        return statuses[0]
    return "error"


def dns(ctx, target: str, types=None):
    """Resolve requested record types; NXDOMAIN is distinct from NODATA/SERVFAIL."""
    if isinstance(types, str):
        types = types.split(",")
    types = list(dict.fromkeys(str(value).upper().strip() for value in (types or DEFAULT_TYPES)))
    if not types or any(value not in RR_TYPES for value in types):
        raise Failure("INVALID_DNS_TYPE", "Supported record types: " + ", ".join(RR_TYPES), "invalid")
    if "PTR" in types:
        try:
            address = ipaddress.ip_address(target)
        except ValueError:
            name = _dns_name(target)
        else:
            public_ip(target)
            name = address.reverse_pointer
    else:
        name = _dns_name(target)
    queries, records, errors, entities, edges = [], [], [], [], []
    total_valid = 0
    for query_type in types:
        source_id = None
        try:
            url = "https://dns.google/resolve?" + urlencode({"name": name, "type": query_type,
                  "do": "true", "cd": "false", "edns_client_subnet": "0.0.0.0/0"})
            response = ctx.fetch(url, headers={"Accept": "application/dns-json, application/json"})
            source_id = response.source_id
            body = _json_response(ctx, response)
            if not isinstance(body, dict) or not _integer(body.get("Status")):
                raise _schema("DNS response lacks a numeric Status", source_id)
            questions = body.get("Question")
            if not isinstance(questions, list) or not any(
                isinstance(q, dict) and isinstance(q.get("name"), str) and
                q["name"].rstrip(".").lower() == name.lower() and q.get("type") == RR_TYPES[query_type]
                for q in questions):
                raise _schema("DNS response question does not match the requested name and type", source_id)
            rcode = body["Status"]
            if rcode not in {0, 3}:
                raise Failure("DNS_RCODE", f"Resolver returned DNS status {rcode}", "error", retryable=rcode == 2,
                              details={"dns_status": rcode, "source_id": source_id, "query_type": query_type})
            answers = body.get("Answer", [])
            if not isinstance(answers, list):
                raise _schema("DNS Answer field is not an array", source_id)
            invalid_answers = 0
            query_records = 0
            for answer in answers:
                if not isinstance(answer, dict) or not isinstance(answer.get("name"), str) or \
                   not _integer(answer.get("type"), 1) or not isinstance(answer.get("data"), str) or \
                   not _integer(answer.get("TTL")):
                    invalid_answers += 1
                    continue
                total_valid += 1
                query_records += 1
                if len(records) >= ctx.max_results:
                    continue
                record = {"name": answer["name"].rstrip(".").lower(),
                          "type": RR_NAMES.get(answer["type"], str(answer["type"])),
                          "type_code": answer["type"], "ttl": answer["TTL"],
                          "value": answer["data"], "query_type": query_type, "source_id": source_id}
                records.append(record)
                if answer["type"] in {1, 28}:
                    try:
                        ip = str(ipaddress.ip_address(answer["data"]))
                    except ValueError:
                        continue
                    entities.extend([_entity("domain", record["name"], [source_id]), _entity("ip", ip, [source_id])])
                    edges.append(_edge("domain", record["name"], "resolves_to", "ip", ip, [source_id]))
                elif answer["type"] in {2, 5, 12, 15}:
                    destination = answer["data"].split()[-1] if answer["data"].split() else ""
                    try:
                        destination = domain_name(destination)
                    except Failure:
                        continue
                    relation = {2: "nameserver", 5: "cname_target", 12: "ptr_target", 15: "mail_exchanger"}[answer["type"]]
                    entities.append(_entity("domain", destination, [source_id]))
                    edges.append(_edge("domain", record["name"], relation, "domain", destination, [source_id]))
            query_status = "not_found" if rcode == 3 else "ok"
            if invalid_answers:
                query_status = "partial"
                errors.append(_schema("Some DNS answer records had an unexpected schema", source_id).as_dict())
            queries.append({"type": query_type, "status": query_status, "rcode": rcode,
                            "answer_count": query_records, "dnssec_authenticated": body.get("AD") is True,
                            "truncated_by_resolver": body.get("TC") is True, "source_id": source_id})
        except Failure as exc:
            details = dict(exc.details, query_type=query_type)
            errors.append(dict(exc.as_dict(), details=details))
            queries.append({"type": query_type, "status": exc.status, "source_id": source_id})
    return result({"target": target, "query_name": name, "resolver": "Google Public DNS",
                   "queries": queries, "records": records, "records_total_in_responses": total_valid,
                   "records_truncated": total_valid > len(records),
                   "scope": "Resolver observations at retrieval time; NODATA does not imply domain absence"},
                  status=_combined_status(queries), errors=errors, entities=entities, relationships=edges)


def _bootstrap_service(body, target, kind):
    if not isinstance(body, dict) or not isinstance(body.get("services"), list):
        raise _schema("IANA RDAP bootstrap response lacks services")
    matches = []
    for service in body["services"]:
        if not isinstance(service, list) or len(service) != 2 or \
           not isinstance(service[0], list) or not isinstance(service[1], list):
            continue
        for key in service[0]:
            if not isinstance(key, str):
                continue
            try:
                if kind == "domain":
                    matched = target == key.lower() or target.endswith("." + key.lower())
                    specificity = len(key)
                elif kind == "ip":
                    network = ipaddress.ip_network(key)
                    matched = ipaddress.ip_address(target) in network
                    specificity = network.prefixlen
                else:
                    first, _, last = key.partition("-")
                    low, high = int(first), int(last or first)
                    matched = low <= int(target) <= high
                    specificity = -(high - low)
            except (ValueError, TypeError):
                continue
            if matched:
                for endpoint in service[1]:
                    if isinstance(endpoint, str) and endpoint.startswith("https://") and urlsplit(endpoint).hostname:
                        matches.append((specificity, endpoint))
    if not matches:
        raise Failure("RDAP_SERVICE_UNAVAILABLE", "IANA bootstrap has no matching HTTPS RDAP service", "unavailable")
    return max(matches, key=lambda entry: entry[0])[1]


def _rdap_target(target, kind):
    if kind not in {"auto", "domain", "ip", "asn"}:
        raise Failure("INVALID_RDAP_KIND", "RDAP kind must be auto, domain, ip, or asn", "invalid")
    if kind == "auto":
        try:
            ipaddress.ip_address(target)
        except ValueError:
            kind = "asn" if re.fullmatch(r"(?:AS)?[0-9]+", target, re.I) else "domain"
        else:
            kind = "ip"
    if kind == "ip":
        return public_ip(target), kind
    if kind == "domain":
        return domain_name(target), kind
    value = target[2:] if target.upper().startswith("AS") else target
    if not re.fullmatch(r"[0-9]{1,10}", value) or not 1 <= int(value) <= 4294967295:
        raise Failure("INVALID_ASN", "Expected AS1-AS4294967295 or its numeric value", "invalid")
    return str(int(value)), kind


def _rdap_matches(body, target, kind):
    if not isinstance(body, dict):
        return False
    expected_class = {"domain": "domain", "ip": "ip network", "asn": "autnum"}[kind]
    if body.get("objectClassName") != expected_class:
        return False
    try:
        if kind == "domain":
            return isinstance(body.get("ldhName"), str) and domain_name(body["ldhName"]) == target
        if kind == "ip":
            address = ipaddress.ip_address(target)
            start, end = ipaddress.ip_address(body["startAddress"]), ipaddress.ip_address(body["endAddress"])
            return address.version == start.version == end.version and start <= address <= end
        return _integer(body.get("startAutnum")) and _integer(body.get("endAutnum")) and \
               body["startAutnum"] <= int(target) <= body["endAutnum"]
    except (KeyError, ValueError, TypeError, Failure):
        return False


def rdap(ctx, target: str, kind="auto"):
    """Bootstrap against IANA and request the exact authoritative RDAP object."""
    normalized, kind = _rdap_target(target, kind)
    bootstrap_file = "dns" if kind == "domain" else "asn" if kind == "asn" else \
        "ipv4" if ipaddress.ip_address(normalized).version == 4 else "ipv6"
    bootstrap_response = ctx.fetch(f"https://data.iana.org/rdap/{bootstrap_file}.json")
    bootstrap = _json_response(ctx, bootstrap_response)
    base = _bootstrap_service(bootstrap, normalized, kind)
    path_kind = "autnum" if kind == "asn" else kind
    endpoint = base.rstrip("/") + "/" + path_kind + "/" + quote(normalized, safe=":")
    response = ctx.fetch(endpoint, headers={"Accept": "application/rdap+json, application/json"})
    # Only an RDAP error object proves a missing registration record; generic
    # HTTP/edge 404s do not establish registration absence.
    if response.status == 404:
        try:
            missing = ctx.json(response)
        except Failure:
            raise _schema("RDAP 404 did not contain a structured RDAP error", response.source_id)
        if isinstance(missing, dict) and missing.get("errorCode") == 404:
            return result({"target": normalized, "kind": kind, "found": False,
                           "bootstrap_source_id": bootstrap_response.source_id, "source_id": response.source_id,
                           "scope": "No registration object returned by the authoritative RDAP service"}, status="not_found")
        raise _schema("RDAP 404 lacked the expected errorCode", response.source_id)
    body = _json_response(ctx, response)
    if not _rdap_matches(body, normalized, kind):
        raise _schema("RDAP identity/range does not match the requested object", response.source_id)
    data = {"target": normalized, "kind": kind, "found": True, "source_id": response.source_id,
            "bootstrap_source_id": bootstrap_response.source_id, "authoritative_service": base,
            "scope": "Registry registration data; registry country is not physical geolocation or ownership proof"}
    for field in ("objectClassName", "handle", "ldhName", "unicodeName", "name", "type", "country",
                  "startAddress", "endAddress", "ipVersion", "startAutnum", "endAutnum", "parentHandle"):
        if isinstance(body.get(field), (str, int)) and not isinstance(body[field], bool):
            data[field] = body[field]
    cap = ctx.max_results
    for field in ("status", "port43"):
        if isinstance(body.get(field), list):
            data[field] = [item for item in body[field] if isinstance(item, str)][:cap]
        elif isinstance(body.get(field), str):
            data[field] = body[field]
    events = body.get("events", [])
    data["events"] = [{"action": event["eventAction"], "date": event["eventDate"]} for event in events
                      if isinstance(event, dict) and isinstance(event.get("eventAction"), str) and
                      isinstance(event.get("eventDate"), str)][:cap] if isinstance(events, list) else []
    nameservers = body.get("nameservers", [])
    data["nameservers"] = [entry["ldhName"].lower().rstrip(".") for entry in nameservers
                           if isinstance(entry, dict) and isinstance(entry.get("ldhName"), str)][:cap] \
        if isinstance(nameservers, list) else []
    raw_entities = body.get("entities", [])
    data["entities"] = []
    if isinstance(raw_entities, list):
        for entry in raw_entities[:cap]:
            if not isinstance(entry, dict):
                continue
            entity = {"handle": entry.get("handle"),
                      "roles": [role for role in entry.get("roles", []) if isinstance(role, str)][:cap]
                      if isinstance(entry.get("roles"), list) else []}
            # Only compact public registry contact properties; retain provider
            # wording and do not infer who owns a website or IP address.
            card = entry.get("vcardArray")
            if isinstance(card, list) and len(card) == 2 and isinstance(card[1], list):
                entity["public_contact"] = [{"property": item[0], "value": item[3]} for item in card[1]
                                            if isinstance(item, list) and len(item) == 4 and isinstance(item[0], str) and
                                            item[0] in {"fn", "org", "email", "url", "tel", "adr"}][:cap]
            data["entities"].append(entity)
    for field in ("events", "nameservers", "entities"):
        raw = body.get(field, [])
        if isinstance(raw, list):
            data[field + "_total"] = len(raw)
            data[field + "_truncated"] = len(raw) > len(data[field])
    entity_kind = {"domain": "domain", "ip": "ip", "asn": "asn"}[kind]
    graph_entities = [_entity(entity_kind, normalized, [response.source_id])]
    edges = []
    if kind == "domain":
        for name in data["nameservers"]:
            graph_entities.append(_entity("domain", name, [response.source_id]))
            edges.append(_edge("domain", normalized, "rdap_nameserver", "domain", name, [response.source_id]))
    return result(data, entities=graph_entities, relationships=edges)


def tls(ctx, target: str, port=443):
    try:
        host = public_ip(target)
    except Failure as exc:
        if exc.code == "NON_PUBLIC_TARGET":
            raise
        host = domain_name(target)
    if isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535:
        raise Failure("INVALID_PORT", "TLS port must be an integer from 1 to 65535", "invalid")
    data = ctx.tls_info(host, port)
    if not isinstance(data, dict):
        raise _schema("TLS transport returned no certificate data")
    data = dict(data, target=host, port=port, scope="Certificate presented by this TLS endpoint at retrieval time")
    source_id = data.get("source_id")
    sources = [source_id] if source_id else []
    entity_kind = "ip" if _is_ip(host) else "domain"
    entities = [_entity(entity_kind, host, sources)]
    edges = []
    fingerprint = data.get("sha256") or data.get("certificate_sha256") or data.get("leaf_sha256")
    if isinstance(fingerprint, str):
        entities.append(_entity("certificate_sha256", fingerprint, sources))
        edges.append(_edge(entity_kind, host, "tls_presented_certificate", "certificate_sha256", fingerprint, sources))
    return result(data, entities=entities, relationships=edges)


def _is_ip(value):
    try:
        ipaddress.ip_address(value)
        return True
    except ValueError:
        return False


def ct(ctx, target: str):
    """List names from public certificate-log records, never claim active DNS."""
    name = domain_name(target)
    response = ctx.fetch("https://crt.sh/?" + urlencode({"q": "%." + name, "output": "json"}))
    rows = _json_response(ctx, response)
    if not isinstance(rows, list):
        raise _schema("Certificate search did not return an array", response.source_id)
    max_rows = max(1000, ctx.max_results * 10)
    names, certificates, invalid = {}, [], 0
    processed = rows[:max_rows]
    for row in processed:
        if not isinstance(row, dict) or not isinstance(row.get("name_value"), str) or not _integer(row.get("id"), 1):
            invalid += 1
            continue
        scoped_names = []
        for raw_name in row["name_value"].splitlines():
            wildcard = raw_name.startswith("*.")
            try:
                candidate = domain_name(raw_name[2:] if wildcard else raw_name)
            except Failure:
                continue
            if candidate != name and not candidate.endswith("." + name):
                continue
            displayed = "*." + candidate if wildcard else candidate
            scoped_names.append(displayed)
            if displayed not in names:
                names[displayed] = {"name": displayed, "wildcard": wildcard,
                                    "certificate_ids": [], "source_id": response.source_id}
            if len(names[displayed]["certificate_ids"]) < ctx.max_results:
                names[displayed]["certificate_ids"].append(row["id"])
        if scoped_names and len(certificates) < ctx.max_results:
            certificate = {"id": row["id"], "dns_names": sorted(set(scoped_names))[:ctx.max_results],
                           "source_id": response.source_id}
            for key in ("issuer_name", "common_name", "not_before", "not_after", "entry_timestamp", "serial_number"):
                if isinstance(row.get(key), str):
                    certificate[key] = row[key]
            certificates.append(certificate)
    if rows and invalid == len(processed):
        raise _schema("Certificate search records did not contain certificate IDs and DNS names", response.source_id)
    observed = [names[key] for key in sorted(names)[:ctx.max_results]]
    errors = [_schema(f"Ignored {invalid} malformed certificate records", response.source_id).as_dict()] if invalid else []
    entities = [_entity("domain", item["name"], [response.source_id]) for item in observed if not item["wildcard"]]
    edges = [_edge("domain", name, "certificate_log_name", "domain", item["name"], [response.source_id])
             for item in observed if not item["wildcard"] and item["name"] != name]
    return result({"target": name, "provider": "crt.sh", "source_id": response.source_id,
                   "names": observed, "certificates": certificates, "certificate_records_returned": len(rows),
                   "certificate_records_processed": len(processed), "names_total_in_processed_records": len(names),
                   "names_truncated": len(names) > len(observed),
                   "records_truncated": len(rows) > len(processed) or len(rows) > len(certificates),
                   "scope": "Historical certificate-log observations; names may be expired, wildcarded, or no longer resolve"},
                  status="partial" if invalid else "ok", errors=errors, entities=entities, relationships=edges)


def _txt_value(value):
    try:
        return "".join(shlex.split(value))
    except ValueError:
        return value


def email(ctx, target: str):
    """Check conventional mailbox syntax and domain mail records, not account existence."""
    if not isinstance(target, str) or len(target) > 254 or target.count("@") != 1:
        raise Failure("INVALID_EMAIL", "Supply one conventional local-part@domain address", "invalid")
    local, raw_domain = target.rsplit("@", 1)
    if not 1 <= len(local) <= 64 or local.startswith(".") or local.endswith(".") or ".." in local or \
       not re.fullmatch(r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+", local):
        raise Failure("INVALID_EMAIL", "Unsupported email local-part syntax; quoted and internationalized local parts are not handled", "invalid")
    name = domain_name(raw_domain)
    normalized = local + "@" + name
    checks = {"mx": dns(ctx, name, ["MX"]), "txt": dns(ctx, name, ["TXT"]),
              "dmarc": dns(ctx, "_dmarc." + name, ["TXT"])}
    mx_records = [row for row in checks["mx"]["data"]["records"] if row["type"] == "MX"]
    spf_records = [dict(row, text=_txt_value(row["value"])) for row in checks["txt"]["data"]["records"]
                   if row["type"] == "TXT" and _txt_value(row["value"]).lower().startswith("v=spf1")]
    dmarc_records = [dict(row, text=_txt_value(row["value"])) for row in checks["dmarc"]["data"]["records"]
                     if row["type"] == "TXT" and _txt_value(row["value"]).lower().startswith("v=dmarc1")]
    source_ids = list(dict.fromkeys(query["source_id"] for check in checks.values()
                                  for query in check["data"]["queries"] if query.get("source_id")))
    entities = [_entity("email", normalized, source_ids), _entity("domain", name, source_ids)]
    errors = [error for check in checks.values() for error in check["errors"]]
    status = _combined_status(list(checks.values()))
    return result({"address": normalized, "domain": name, "syntax_accepted": True,
                   "mailbox_existence": "not_checked", "mx_records": mx_records,
                   "null_mx": any(re.fullmatch(r"0\s+\.", row["value"].strip()) for row in mx_records),
                   "spf_records": spf_records, "dmarc_records": dmarc_records,
                   "checks": {key: value["data"] for key, value in checks.items()},
                   "scope": "Address syntax and domain DNS only; no signup, recovery, SMTP, or mailbox verification performed"},
                  status=status, errors=errors, entities=entities,
                  relationships=[_edge("email", normalized, "email_domain", "domain", name, source_ids)])
