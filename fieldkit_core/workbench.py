"""Shared investigation workspace for the TUI, app and agent clients.

Collector reports and source bytes remain in CaseStore's immutable ledger.
The wb_* tables hold analyst assessments, research plans and an action journal.
Correlations are reviewable leads; they never merge identities automatically.
"""
from __future__ import annotations

from contextlib import contextmanager
from dataclasses import replace
from datetime import datetime
import hashlib
import html
import itertools
import json
from pathlib import Path
import re
import sqlite3
import uuid

from .case import CaseStore, _graph
from .common import Failure, result, utc_now, web_url
from .manifest import COMMANDS, validate_job

_CAPS = {"evidence": 500, "entities": 1000, "links": 2000, "flows": 50, "notes": 500,
         "reviews": 2000, "correlation_reviews": 500}
_STATUSES = {"unreviewed", "corroborated", "contested", "discarded"}


def _json(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def _text(value, field, maximum, *, empty=False):
    if not isinstance(value, str) or len(value) > maximum or (not empty and not value.strip()) or "\x00" in value:
        raise Failure("WORKBENCH_INPUT", f"{field} must contain {'0' if empty else '1'}–{maximum} characters", "invalid")
    try:
        value.encode("utf-8")
    except UnicodeError as exc:
        raise Failure("WORKBENCH_INPUT", f"{field} must be valid UTF-8 text", "invalid") from exc
    return value.strip()


def _id(prefix):
    return prefix + "_" + uuid.uuid4().hex


def _entity_id(kind, value):
    # Match CaseStore's typed graph identity, without merging values across types.
    return "entity_" + hashlib.sha256(_json([kind, value]).encode()).hexdigest()


def _timestamp(value):
    if value is None or value == "":
        return None
    value = _text(value, "observed_at", 80)
    try:
        datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise Failure("WORKBENCH_INPUT", "observed_at must be an ISO date or timestamp", "invalid") from exc
    return value


class Workbench:
    def __init__(self, case_path, config=None, job_policy=None):
        from .cli import Config
        self.store = CaseStore(case_path)
        self.path = self.store.path
        self.config = replace(config or Config(), case=str(self.path))
        if job_policy is not None and not callable(job_policy):
            raise Failure("WORKBENCH_INPUT", "job_policy must be callable", "invalid")
        self.job_policy = job_policy
        with self._transaction() as connection:
            connection.execute("CREATE TABLE IF NOT EXISTS wb_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            connection.execute("CREATE TABLE IF NOT EXISTS wb_items(kind TEXT NOT NULL,id TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(kind,id))")
            connection.execute("CREATE TABLE IF NOT EXISTS wb_activity(id TEXT PRIMARY KEY,payload TEXT NOT NULL,previous_hash TEXT NOT NULL,event_hash TEXT NOT NULL)")
            if not connection.execute("SELECT 1 FROM wb_meta WHERE key='metadata'").fetchone():
                now = utc_now()
                metadata = {"case_id": _id("case"), "title": "Untitled investigation", "question": "",
                            "created_at": now, "updated_at": now, "is_demo": False}
                connection.execute("INSERT INTO wb_meta VALUES('metadata',?)", (_json(metadata),))

    @contextmanager
    def _transaction(self):
        connection = self.store._connection()
        try:
            connection.execute("BEGIN IMMEDIATE")
            yield connection
            connection.commit()
        except sqlite3.Error as exc:
            connection.rollback()
            raise Failure("WORKBENCH_DATABASE", "Could not update the investigation workspace") from exc
        except BaseException:
            connection.rollback()
            raise
        finally:
            connection.close()

    def _read(self, kind):
        connection = self.store._connection(readonly=True)
        try:
            rows = connection.execute("SELECT payload FROM wb_items WHERE kind=? ORDER BY created_at DESC,id LIMIT ?",
                                      (kind, _CAPS.get(kind, 500))).fetchall()
            return [json.loads(row[0]) for row in rows]
        except (sqlite3.Error, ValueError, RecursionError) as exc:
            raise Failure("WORKBENCH_READ", "Could not read the investigation workspace") from exc
        finally:
            connection.close()

    def _get(self, connection, kind, identifier):
        if not isinstance(identifier, str) or len(identifier) > 160:
            raise Failure("WORKBENCH_INPUT", "Invalid record identifier", "invalid")
        row = connection.execute("SELECT payload FROM wb_items WHERE kind=? AND id=?", (kind, identifier)).fetchone()
        if row is None:
            raise Failure("WORKBENCH_NOT_FOUND", f"Unknown {kind} record", "not_found")
        try:
            return json.loads(row[0])
        except (ValueError, RecursionError) as exc:
            raise Failure("WORKBENCH_CORRUPT", "An investigation record contains invalid JSON") from exc

    def _put(self, connection, kind, identifier, value):
        exists = connection.execute("SELECT 1 FROM wb_items WHERE kind=? AND id=?", (kind, identifier)).fetchone()
        if not exists and connection.execute("SELECT count(*) FROM wb_items WHERE kind=?", (kind,)).fetchone()[0] >= _CAPS[kind]:
            raise Failure("WORKBENCH_LIMIT", f"The workspace limit for {kind} is {_CAPS[kind]}", "invalid")
        connection.execute("INSERT INTO wb_items VALUES(?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET payload=excluded.payload",
                           (kind, identifier, _json(value), value.get("created_at", utc_now())))

    def _event(self, connection, action, title, details):
        previous = connection.execute("SELECT event_hash FROM wb_activity ORDER BY rowid DESC LIMIT 1").fetchone()
        previous_hash = previous[0] if previous else "0" * 64
        event = {"id": _id("activity"), "action": action, "title": title, "details": details, "created_at": utc_now()}
        digest = hashlib.sha256((previous_hash + _json(event)).encode()).hexdigest()
        connection.execute("INSERT INTO wb_activity VALUES(?,?,?,?)", (event["id"], _json(event), previous_hash, digest))
        metadata = json.loads(connection.execute("SELECT value FROM wb_meta WHERE key='metadata'").fetchone()[0])
        metadata["updated_at"] = event["created_at"]
        connection.execute("UPDATE wb_meta SET value=? WHERE key='metadata'", (_json(metadata),))
        return event

    def _evidence_exists(self, connection, identifier):
        if not isinstance(identifier, str) or len(identifier) > 160:
            return False
        if connection.execute("SELECT 1 FROM wb_items WHERE kind='evidence' AND id=?", (identifier,)).fetchone():
            return True
        if identifier.startswith("evidence_"):
            return connection.execute("SELECT 1 FROM runs WHERE run_id=?", (identifier[len("evidence_"):],)).fetchone() is not None
        return False

    def _references(self, connection, identifiers, *, required=False):
        if identifiers is None:
            identifiers = []
        if not isinstance(identifiers, list) or len(identifiers) > 50 or (required and not identifiers):
            raise Failure("WORKBENCH_EVIDENCE", "Supply up to 50 supporting evidence IDs" + ("; at least one is required" if required else ""), "invalid")
        if not all(isinstance(item, str) for item in identifiers):
            raise Failure("WORKBENCH_EVIDENCE", "Supporting evidence IDs must be strings", "invalid")
        references = list(dict.fromkeys(identifiers))
        if any(not self._evidence_exists(connection, identifier) for identifier in references):
            raise Failure("WORKBENCH_EVIDENCE", "A supporting evidence record does not exist in this case", "invalid")
        return references

    def update_case(self, title, question=""):
        title, question = _text(title, "title", 160), _text(question, "question", 4000, empty=True)
        with self._transaction() as connection:
            metadata = json.loads(connection.execute("SELECT value FROM wb_meta WHERE key='metadata'").fetchone()[0])
            metadata.update(title=title, question=question)
            connection.execute("UPDATE wb_meta SET value=? WHERE key='metadata'", (_json(metadata),))
            self._event(connection, "case.updated", "Investigation question updated", {"title": title, "question": question})
            return json.loads(connection.execute("SELECT value FROM wb_meta WHERE key='metadata'").fetchone()[0])

    def _record_manual(self, title, content, source_url, kind, observed_at, body, *, filename=None, media_type="text/plain"):
        from .cli import envelope
        title = _text(title, "title", 240)
        _text(content, "content", 20000, empty=True)
        kind = _text(kind, "kind", 40)
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,39}", kind):
            raise Failure("WORKBENCH_INPUT", "Evidence kind must be a short lowercase identifier", "invalid")
        source_url = _text(source_url, "source_url", 8192, empty=True)
        source_url = web_url(source_url) if source_url else ""
        observed_at = _timestamp(observed_at)
        now, identifier, source_id = utc_now(), _id("evidence"), _id("src")
        digest = hashlib.sha256(body).hexdigest()
        source = {"source_id": source_id, "kind": "analyst_supplied", "name": title, "url": source_url or None,
                  "observed_at": now, "sha256": digest, "bytes": len(body), "media_type": media_type,
                  "untrusted_content": True, "collection_method": "manual_import", "supplied_source_time": observed_at}
        entities = [{"type": "url", "value": source_url, "source_ids": [source_id]}] if source_url else []
        report = envelope("evidence.import" if filename else "evidence.note", source_url or title,
                          result({"evidence_id": identifier, "title": title, "content": content,
                                  "filename": filename, "observed_at": observed_at, "collection_method": "analyst_supplied"}, entities=entities))
        report["sources"] = [source]
        self.store.save(report, {digest: body})
        record = {"id": identifier, "title": title, "kind": kind, "content": content, "source_url": source_url,
                  "observed_at": observed_at, "collected_at": now, "sha256": digest, "artifact_sha256": digest,
                  "status": "unreviewed", "review_note": "", "run_id": report["run_id"], "source_ids": [source_id],
                  "created_at": now, "filename": filename, "media_type": media_type, "bytes": len(body),
                  "collection_method": "analyst_supplied", "is_demo": kind == "demo"}
        record.update(sources=[source], source_count=1, sources_truncated=False, hash_scope="source_bytes")
        with self._transaction() as connection:
            self._put(connection, "evidence", identifier, record)
            self._event(connection, "evidence.added", "Evidence added", {"evidence_id": identifier, "title": title, "sha256": digest})
        return record

    def add_evidence(self, title, content="", source_url="", kind="note", observed_at=None):
        _text(content, "content", 20000, empty=True)
        return self._record_manual(title, content, source_url, kind, observed_at, content.encode("utf-8"))

    def add_attachment(self, name, body, media_type="application/octet-stream"):
        if not isinstance(body, bytes) or len(body) > 8 * 1024 * 1024:
            raise Failure("WORKBENCH_ATTACHMENT", "Attachments must be bytes and no larger than 8 MiB", "invalid")
        name = _text(name, "name", 240).replace("\\", "/").split("/")[-1]
        if not name or name in {".", ".."}:
            raise Failure("WORKBENCH_ATTACHMENT", "Attachment needs a filename", "invalid")
        media_type = _text(media_type, "media_type", 120)
        return self._record_manual(name, "Imported attachment: " + name, "", "attachment", None, body,
                                   filename=name, media_type=media_type)

    def review_evidence(self, evidence_id, status, note=""):
        aliases = {"verified": "corroborated", "reviewed": "corroborated", "rejected": "contested", "archived": "discarded"}
        status = aliases.get(status, status) if isinstance(status, str) else status
        if not isinstance(status, str) or status not in _STATUSES:
            raise Failure("WORKBENCH_REVIEW", "Evidence status must be unreviewed, corroborated, contested, or discarded", "invalid")
        note = _text(note, "note", 4000, empty=True)
        with self._transaction() as connection:
            if not self._evidence_exists(connection, evidence_id):
                raise Failure("WORKBENCH_NOT_FOUND", "Unknown evidence record", "not_found")
            record = {"id": evidence_id, "status": status, "review_note": note, "reviewed_at": utc_now(), "scope": "analyst_assessment"}
            self._put(connection, "reviews", evidence_id, record)
            self._event(connection, "evidence.reviewed", "Evidence assessment recorded", {"evidence_id": evidence_id, "status": status, "note": note})
        return next((item for item in self._materialized()["evidence"] if item["id"] == evidence_id), record)

    def add_note(self, content, evidence_ids=None):
        content = _text(content, "content", 20000)
        with self._transaction() as connection:
            record = {"id": _id("note"), "content": content, "evidence_ids": self._references(connection, evidence_ids), "created_at": utc_now()}
            self._put(connection, "notes", record["id"], record)
            self._event(connection, "note.added", "Analyst note added", {"note_id": record["id"], "evidence_ids": record["evidence_ids"],
                         "content_sha256": hashlib.sha256(content.encode()).hexdigest()})
        return record

    def add_entity(self, kind, value, label=""):
        kind, value = _text(kind, "kind", 40), _text(value, "value", 2048)
        label = _text(label, "label", 160, empty=True)
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,39}", kind):
            raise Failure("WORKBENCH_INPUT", "Entity type must be a short lowercase identifier", "invalid")
        record = {"id": _entity_id(kind, value), "type": kind, "value": value, "label": label or value[:160],
                  "source_ids": [], "evidence_ids": [], "origin": "analyst", "created_at": utc_now()}
        with self._transaction() as connection:
            self._put(connection, "entities", record["id"], record)
            self._event(connection, "entity.added", "Entity annotated", {"entity_id": record["id"], "type": kind, "label": record["label"]})
        return record

    def add_link(self, source_id, target_id, relation, evidence_ids, note=""):
        relation = _text(relation, "relation", 80)
        note = _text(note, "note", 4000, empty=True)
        entities = {item["id"] for item in self._materialized()["entities"]}
        if not isinstance(source_id, str) or not isinstance(target_id, str) or source_id not in entities or target_id not in entities or source_id == target_id:
            raise Failure("WORKBENCH_LINK", "Link endpoints must be distinct existing entities", "invalid")
        with self._transaction() as connection:
            record = {"id": _id("link"), "source_id": source_id, "target_id": target_id, "relation": relation,
                      "evidence_ids": self._references(connection, evidence_ids, required=True), "source_ids": [],
                      "note": note, "status": "analyst_assertion", "created_at": utc_now()}
            self._put(connection, "links", record["id"], record)
            self._event(connection, "link.added", "Evidence-backed analyst link added", {"link_id": record["id"], "source_id": source_id,
                         "target_id": target_id, "relation": relation, "evidence_ids": record["evidence_ids"]})
        return record

    def _step(self, command, target=None, options=None):
        options = {} if options is None else options
        job = {"command": command, "options": options}
        if target is not None:
            job["target"] = target
        validate_job(job)
        if len(_json(job).encode()) > 20000:
            raise Failure("WORKBENCH_STEP", "Flow step exceeds the 20 KiB limit", "invalid")
        return {"id": _id("step"), "command": command, "target": target, "options": options, "status": "pending",
                "run_id": None, "error": None, "attempts": 0, "started_at": None, "completed_at": None}

    def create_flow(self, title, steps=None):
        title = _text(title, "title", 160)
        if steps is None:
            steps = []
        if not isinstance(steps, list) or len(steps) > 20:
            raise Failure("WORKBENCH_FLOW", "Flows accept up to 20 steps", "invalid")
        normalized = []
        for step in steps:
            if not isinstance(step, dict) or set(step) - {"command", "target", "options"} or "command" not in step:
                raise Failure("WORKBENCH_FLOW", "Each flow step needs a command, optional target and options", "invalid")
            normalized.append(self._step(**step))
        record = {"id": _id("flow"), "title": title, "status": "pending", "steps": normalized, "created_at": utc_now()}
        with self._transaction() as connection:
            self._put(connection, "flows", record["id"], record)
            self._event(connection, "flow.created", "Research flow created", {"flow_id": record["id"], "title": title, "step_count": len(normalized)})
        return record

    def add_step(self, flow_id, command, target=None, options=None):
        step = self._step(command, target, options)
        with self._transaction() as connection:
            flow = self._get(connection, "flows", flow_id)
            if len(flow["steps"]) >= 20:
                raise Failure("WORKBENCH_FLOW", "Flows accept up to 20 steps", "invalid")
            flow["steps"].append(step)
            flow["status"] = "pending"
            self._put(connection, "flows", flow_id, flow)
            self._event(connection, "flow.step_added", "Collection step planned", {"flow_id": flow_id, "step_id": step["id"], "command": command})
        return step

    def run_tool(self, command, target=None, options=None):
        """Run a validated collector; callers enforce their own local-file scope."""
        from .cli import execute
        if self.job_policy is not None:
            command, target, options = self.job_policy(command, target, {} if options is None else options)
        step = self._step(command, target, options)
        job = {"command": command, "options": step["options"]}
        if target is not None:
            job["target"] = target
        report = execute(job, self.config)
        with self._transaction() as connection:
            self._event(connection, "collector.completed", "Collector result recorded", {"command": command, "target": target,
                         "run_id": report["run_id"], "status": report["status"]})
        return report

    def run_step(self, flow_id, step_id):
        with self._transaction() as connection:
            flow = self._get(connection, "flows", flow_id)
            step = next((item for item in flow["steps"] if item["id"] == step_id), None)
            if step is None:
                raise Failure("WORKBENCH_NOT_FOUND", "Unknown flow step", "not_found")
            if step["status"] == "running":
                raise Failure("WORKBENCH_STEP_RUNNING", "This step is already running", "invalid")
            step.update(status="running", started_at=utc_now(), completed_at=None, error=None, attempts=step["attempts"] + 1)
            flow["status"] = "running"
            self._put(connection, "flows", flow_id, flow)
            self._event(connection, "flow.step_started", "Collection step started", {"flow_id": flow_id, "step_id": step_id, "command": step["command"]})
            command, target, options = step["command"], step["target"], step["options"]
        try:
            report = self.run_tool(command, target, options)
        except BaseException as exc:
            with self._transaction() as connection:
                flow = self._get(connection, "flows", flow_id)
                step = next(item for item in flow["steps"] if item["id"] == step_id)
                step.update(status="failed", completed_at=utc_now(), error="Collector interrupted or failed")
                flow["status"] = "failed"
                self._put(connection, "flows", flow_id, flow)
                self._event(connection, "flow.step_failed", "Collection step failed", {"flow_id": flow_id, "step_id": step_id, "exception_type": type(exc).__name__})
            raise
        with self._transaction() as connection:
            flow = self._get(connection, "flows", flow_id)
            step = next(item for item in flow["steps"] if item["id"] == step_id)
            complete = report["status"] in {"ok", "not_found"}
            step.update(status="completed" if complete else "failed", completed_at=utc_now(), run_id=report["run_id"],
                        error=None if complete else (report["errors"][0]["message"] if report.get("errors") else report["status"]))
            states = {item["status"] for item in flow["steps"]}
            flow["status"] = "running" if "running" in states else "failed" if "failed" in states else "completed" if states == {"completed"} else "pending"
            self._put(connection, "flows", flow_id, flow)
            self._event(connection, "flow.step_completed" if complete else "flow.step_failed", "Collection step finished",
                        {"flow_id": flow_id, "step_id": step_id, "run_id": report["run_id"], "status": step["status"]})
        return report

    def _materialized(self):
        summary, reports, records, artifact_records, selection = self.store._selection(500)
        report_hashes = {item["run_id"]: item["report_sha256"] for item in records}
        manual = self._read("evidence")
        by_run = {item["run_id"]: item["id"] for item in manual}
        reviews = {item["id"]: item for item in self._read("reviews")}
        evidence = list(manual)
        artifact_ids = {item["sha256"] for item in artifact_records}
        for report in reports:
            if report["run_id"] in by_run:
                continue
            sources, data = report.get("sources", []), report.get("data")
            first = sources[0] if sources else {}
            preview = json.dumps(data, ensure_ascii=False, allow_nan=False) if data is not None else "No data returned"
            urls = [source.get("url") for source in sources if isinstance(source.get("url"), str)]
            if not urls:
                urls = [url for source in sources for url in source.get("provider_request_urls", []) if isinstance(url, str)]
            digest = first.get("sha256") or report_hashes[report["run_id"]]
            observed_at = None
            if report["command"] in {"telegram-post", "bellingcat-telegram"} and isinstance(data, dict) and isinstance(data.get("message"), dict):
                try:
                    observed_at = _timestamp(data["message"].get("timestamp_observed"))
                except Failure:
                    pass
            source_details = [{key: source[key] for key in ("source_id", "kind", "name", "url", "observed_at", "sha256", "bytes", "http_status", "hash_scope") if key in source}
                              for source in sources[:16]]
            evidence.append({"id": "evidence_" + report["run_id"], "title": report["command"] + (" · " + str(report.get("target"))[:180] if report.get("target") else ""),
                             "kind": "collector", "content": preview[:12000], "content_truncated": len(preview) > 12000,
                             "source_url": urls[0] if urls else "", "observed_at": observed_at,
                             "collected_at": report.get("timing", {}).get("completed_at") or report.get("started_at"),
                             "sha256": digest, "artifact_sha256": digest if digest in artifact_ids else None,
                             "status": "unreviewed", "review_note": "", "run_id": report["run_id"],
                             "source_ids": [source.get("source_id") for source in sources if source.get("source_id")],
                             "collector_status": report["status"], "collection_method": "collector", "is_demo": False,
                             "report_sha256": report_hashes[report["run_id"]], "hash_scope": (first.get("hash_scope") or "source_bytes") if first.get("sha256") else "collector_report",
                             "sources": source_details, "source_count": len(sources), "sources_truncated": len(sources) > 16})
        evidence = sorted(evidence, key=lambda item: item.get("collected_at") or "", reverse=True)
        for item in evidence:
            if item["id"] in reviews:
                item.update({key: value for key, value in reviews[item["id"]].items() if key != "id"})
        graph = _graph(reports, total_runs=summary["run_count"])
        entities = {}
        for entry in graph["entities"]:
            entities[entry["entity_id"]] = {"id": entry["entity_id"], "type": entry["type"], "value": entry["value"],
                "label": str(entry["value"])[:160], "source_ids": entry["source_ids"],
                "evidence_ids": [by_run.get(run_id, "evidence_" + run_id) for run_id in entry["run_ids"]], "origin": "observed"}
        for entry in self._read("entities"):
            if entry["id"] in entities:
                entities[entry["id"]]["label"] = entry["label"]
            else:
                entities[entry["id"]] = entry
        links = []
        for entry in graph["relationships"]:
            source_id, target_id = entry["source"]["entity_id"], entry["target"]["entity_id"]
            identifier = "link_" + hashlib.sha256(_json([source_id, entry["relation"], target_id]).encode()).hexdigest()
            links.append({"id": identifier, "source_id": source_id, "target_id": target_id, "relation": entry["relation"],
                          "evidence_ids": [by_run.get(run_id, "evidence_" + run_id) for run_id in entry["run_ids"]],
                          "source_ids": entry["source_ids"], "status": "observed", "note": ""})
        links.extend(self._read("links"))
        for link in links:
            for identifier in (link["source_id"], link["target_id"]):
                if identifier in entities:
                    entities[identifier]["evidence_ids"] = list(dict.fromkeys(entities[identifier]["evidence_ids"] + link["evidence_ids"]))[:128]
        return {"evidence": evidence[:500], "entities": list(entities.values())[:2000], "links": links[:4000],
                "summary": summary, "selection": selection, "graph_truncated": graph["truncated"] or len(evidence) > 500}

    def _artifact_ids(self):
        connection = self.store._connection(readonly=True)
        try:
            return {row[0] for row in connection.execute("SELECT sha256 FROM artifacts")}
        finally:
            connection.close()

    def _correlation_candidates(self, state):
        entities = {item["id"]: item for item in state["entities"]}
        groups = {}
        for link in state["links"]:
            if link["relation"] == "links_to" and link["source_id"] in entities and link["target_id"] in entities:
                target = entities[link["target_id"]]
                if target["type"] in {"url", "profile_url", "domain", "email"}:
                    groups.setdefault(("shared_reference", str(target["value"])), []).append((link["source_id"], link["evidence_ids"]))
        for entity in entities.values():
            if entity["type"] in {"username", "handle"} and isinstance(entity["value"], str) and ":" in entity["value"]:
                scope, handle = entity["value"].split(":", 1)
                if re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", handle) and scope and entity["evidence_ids"]:
                    groups.setdefault(("shared_handle", handle), []).append((entity["id"], entity["evidence_ids"]))
        output = {}
        for (signal, value), members in sorted(groups.items()):
            unique = {}
            for identifier, references in members:
                unique[identifier] = list(dict.fromkeys(unique.get(identifier, []) + references))
            # Very common references are weak and can create quadratic noise.
            if not 2 <= len(unique) <= 20:
                continue
            for first, second in itertools.combinations(sorted(unique), 2):
                references = list(dict.fromkeys(unique[first] + unique[second]))[:50]
                if not references:
                    continue
                if signal == "shared_handle":
                    first_scope = str(entities[first]["value"]).split(":", 1)[0]
                    second_scope = str(entities[second]["value"]).split(":", 1)[0]
                    if first_scope == second_scope:
                        continue
                identifier = "correlation_" + hashlib.sha256(_json([first, second, signal, value]).encode()).hexdigest()
                output[identifier] = {"id": identifier, "source_id": first, "target_id": second, "signal": signal,
                    "value": value, "reason": ("Both records explicitly reference " if signal == "shared_reference" else "The same literal handle appears in different service scopes: ") + value,
                    "evidence_ids": references, "status": "candidate", "review_note": "",
                    "strength": "weak" if signal == "shared_handle" else "reference_match",
                    "interpretation": "Investigation lead only; shared signals do not establish identity or ownership"}
                if len(output) >= 200:
                    return list(output.values())
        return list(output.values())

    def correlations(self):
        return self._correlations(self._materialized())

    def _correlations(self, state):
        candidates = {item["id"]: item for item in self._correlation_candidates(state)}
        for review in self._read("correlation_reviews"):
            if review["id"] in candidates:
                candidates[review["id"]].update({key: value for key, value in review.items() if key not in {"candidate", "created_at"}})
            else:
                candidates[review["id"]] = {**review["candidate"], **{key: value for key, value in review.items() if key not in {"candidate", "created_at"}}, "stale": True}
        return list(candidates.values())[:500]

    def review_correlation(self, identifier, decision, note=""):
        decision = {"accept": "accepted", "reject": "rejected"}.get(decision, decision) if isinstance(decision, str) else decision
        if not isinstance(decision, str) or decision not in {"accepted", "rejected"}:
            raise Failure("WORKBENCH_REVIEW", "Correlation decision must be accepted or rejected", "invalid")
        note = _text(note, "note", 4000)
        candidate = next((item for item in self.correlations() if item["id"] == identifier), None)
        if candidate is None:
            raise Failure("WORKBENCH_NOT_FOUND", "Unknown correlation lead", "not_found")
        with self._transaction() as connection:
            self._references(connection, candidate["evidence_ids"], required=True)
            record = {"id": identifier, "status": decision, "review_note": note, "reviewed_at": utc_now(),
                      "candidate": candidate, "created_at": utc_now()}
            self._put(connection, "correlation_reviews", identifier, record)
            self._event(connection, "correlation.reviewed", "Correlation lead reviewed", {"correlation_id": identifier, "decision": decision, "note": note,
                         "evidence_ids": candidate["evidence_ids"]})
        return {**candidate, **{key: value for key, value in record.items() if key not in {"candidate", "created_at"}}}

    def snapshot(self):
        state = self._materialized()
        connection = self.store._connection(readonly=True)
        try:
            metadata = json.loads(connection.execute("SELECT value FROM wb_meta WHERE key='metadata'").fetchone()[0])
            activity = [{**json.loads(row["payload"]), "event_hash": row["event_hash"], "previous_hash": row["previous_hash"]}
                        for row in connection.execute("SELECT payload,event_hash,previous_hash FROM wb_activity ORDER BY rowid DESC LIMIT 150")]
        finally:
            connection.close()
        correlations = self._correlations(state)
        for item in correlations:
            if item["status"] == "accepted":
                state["links"].append({"id": "reviewed_" + item["id"], "source_id": item["source_id"], "target_id": item["target_id"],
                    "relation": "possible_association", "evidence_ids": item["evidence_ids"], "source_ids": [],
                    "status": "analyst_accepted_lead", "note": item["review_note"], "interpretation": "Not an identity merge"})
        flows, notes = self._read("flows"), self._read("notes")[:100]
        stats = {"evidence": len(state["evidence"]), "entities": len(state["entities"]), "links": len(state["links"]),
                 "flows": len(flows), "notes": len(notes), "correlations": len(correlations),
                 "unreviewed": sum(item["status"] == "unreviewed" for item in state["evidence"]),
                 "candidate_leads": sum(item["status"] == "candidate" for item in correlations), "runs": state["summary"]["run_count"]}
        stats["evidence_total"] = state["summary"]["run_count"]
        return {"schema_version": "1.0", "metadata": metadata, "case_path": str(self.path),
                "evidence": state["evidence"], "entities": state["entities"], "links": state["links"], "flows": flows,
                "correlations": correlations, "notes": notes, "activity": activity, "stats": stats,
                "selection": state["selection"], "truncated": state["graph_truncated"],
                "scope": "Evidence and analyst assessments remain separate. Correlations are reviewable leads, not identity conclusions."}

    def export(self, format="json"):
        if format not in {"json", "markdown"}:
            raise Failure("INVALID_FORMAT", "Workbench exports support json or markdown", "invalid")
        snapshot = self.snapshot()
        report_export = json.loads(self.store.export("json", limit=500))
        payload = {"format": "fieldkit-investigation-v1", "exported_at": utc_now(), "workspace": snapshot,
                   "collector_ledger": report_export, "integrity": self.store.verify()}
        if format == "json":
            return json.dumps(payload, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
        def cell(value):
            return html.escape(str(value)).replace("|", "\\|").replace("\n", " ").replace("\r", " ")
        lines = ["# " + cell(snapshot["metadata"]["title"]), "", "Investigation question: " + cell(snapshot["metadata"]["question"]),
                 "", "Exported: " + payload["exported_at"], "", "Correlations are analyst-reviewed leads, not identity or ownership conclusions.",
                 "", "## Evidence", "", "| Title | Assessment | Source | SHA-256 |", "| --- | --- | --- | --- |"]
        for item in snapshot["evidence"]:
            lines.append("| " + " | ".join(cell(item.get(key) or "") for key in ("title", "status", "source_url", "sha256")) + " |")
        for section, key in (("Research flows", "flows"), ("Analyst notes", "notes"), ("Correlation reviews", "correlations"), ("Activity journal", "activity")):
            lines.extend(["", "## " + section, ""])
            serialized = json.dumps(snapshot[key], ensure_ascii=False, indent=2)
            fence = "`" * max(3, max((len(match.group()) + 1 for match in re.finditer(r"`+", serialized)), default=3))
            lines.extend([fence + "json", serialized, fence])
        lines.extend(["", "## Integrity", "", "Stored report/artifact hashes verified: " + str(payload["integrity"]["ok"]).lower(),
                      "", "The JSON export additionally includes full collector reports and graph references. Source bytes remain in the case's content-addressed blobs directory."])
        return "\n".join(lines) + "\n"

    def seed_demo(self):
        current = self.snapshot()
        if current["metadata"]["is_demo"]:
            return current
        if current["stats"]["runs"] or current["evidence"] or current["entities"] or current["flows"] or current["notes"]:
            raise Failure("WORKBENCH_DEMO", "Demo evidence can only be seeded into an empty investigation", "invalid")
        self.update_case("Harbor Signal", "Which public records reference the same project site? Synthetic training case; no identity attribution.")
        with self._transaction() as connection:
            metadata = json.loads(connection.execute("SELECT value FROM wb_meta WHERE key='metadata'").fetchone()[0])
            metadata["is_demo"] = True
            connection.execute("UPDATE wb_meta SET value=? WHERE key='metadata'", (_json(metadata),))
        first = self.add_evidence("Profile A references the project site", "Synthetic evidence: sample profile A links to https://example.org/research. This is training data, not a collected account.",
                                  "https://example.org/profile-a", "demo", "2026-11-10T09:00:00Z")
        second = self.add_evidence("Profile B references the same site", "Synthetic evidence: sample profile B links to the same project site. Shared references are leads; ownership remains unestablished.",
                                   "https://example.org/profile-b", "demo", "2026-11-10T11:30:00Z")
        self.add_evidence("Research question and alternative explanation", "Synthetic brief: the two records may be independent contributors to the same project. Seek explicit cross-links before making an identity claim.", kind="demo")
        site = self.add_entity("url", "https://example.org/research", "Project website")
        a = self.add_entity("profile_url", "https://example.org/profile-a", "Sample profile A")
        b = self.add_entity("profile_url", "https://example.org/profile-b", "Sample profile B")
        first_handle = self.add_entity("username", "forum:harborwatch", "forum / harborwatch")
        second_handle = self.add_entity("username", "telegram:harborwatch", "Telegram / harborwatch")
        self.add_link(a["id"], site["id"], "links_to", [first["id"]], "Synthetic observed link")
        self.add_link(b["id"], site["id"], "links_to", [second["id"]], "Synthetic observed link")
        self.add_link(first_handle["id"], a["id"], "public_profile_at", [first["id"]])
        self.add_link(second_handle["id"], b["id"], "public_profile_at", [second["id"]])
        self.review_evidence(first["id"], "corroborated", "Synthetic evidence checked against the supplied training note.")
        self.add_note("Working hypothesis: these records reference the same project. Alternative: unrelated contributors. Shared handles are a weak signal. No identity merge is justified.", [first["id"], second["id"]])
        self.create_flow("Review a public channel", [{"command": "telegram-messages", "target": "telegram", "options": {"limit": 3}},
                                                     {"command": "http", "target": "https://example.org/", "options": {}}])
        return self.snapshot()
