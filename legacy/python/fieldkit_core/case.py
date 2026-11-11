"""Private, append-only case records and content-addressed source artifacts."""
from __future__ import annotations

import hashlib
import html
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import time

from .common import Failure, utc_now


_SCHEMA = "1"
_DIGEST = re.compile(r"[a-f0-9]{64}")
_MAX_ARTIFACT_BYTES = 32 * 1024 * 1024
_MAX_REPORT_BYTES = 32 * 1024 * 1024
_MAX_SELECTION_BYTES = 32 * 1024 * 1024
_MAX_GRAPH_ENTITIES = 2000
_MAX_GRAPH_RELATIONSHIPS = 4000
_MAX_GRAPH_PROVENANCE = 128


def _canonical(value) -> str:
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def _failure(code: str, message: str, *, details=None) -> Failure:
    return Failure(code, message, "error", details=details)


def _directory(path: Path, *, create: bool) -> None:
    if create:
        try:
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
        except OSError as exc:
            raise _failure("CASE_DIRECTORY", "Could not create the case directory") from exc
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise Failure("CASE_NOT_FOUND", "Case workspace does not exist", "not_found") from exc
    except OSError as exc:
        raise _failure("CASE_DIRECTORY", "Could not inspect the case directory") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise Failure("CASE_PATH_UNSAFE", "Case directories must be real directories, not symlinks", "invalid")


def _regular(path: Path, *, missing_ok: bool = False) -> os.stat_result | None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        if missing_ok:
            return None
        raise _failure("CASE_MISSING_FILE", "A case database or artifact is missing", details={"path": str(path)})
    except OSError as exc:
        raise _failure("CASE_FILE_ACCESS", "Could not inspect a case file", details={"path": str(path)}) from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise Failure("CASE_PATH_UNSAFE", "Case files must be regular files without symlinks or hard links", "invalid",
                      details={"path": str(path)})
    return info


def _limit(value: int, maximum: int = 10000) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= maximum:
        raise Failure("INVALID_LIMIT", f"Limit must be an integer from 1 to {maximum}", "invalid")
    return value


def _source_refs(report: dict) -> set[str]:
    sources = report.get("sources", [])
    if not isinstance(sources, list):
        raise Failure("INVALID_REPORT", "Report sources must be an array", "invalid")
    references = set()
    for source in sources:
        if not isinstance(source, dict):
            raise Failure("INVALID_REPORT", "Each report source must be an object", "invalid")
        digest = source.get("sha256")
        if digest is None:
            continue
        if not isinstance(digest, str) or _DIGEST.fullmatch(digest) is None:
            raise Failure("INVALID_REPORT", "Source SHA-256 is malformed", "invalid")
        if source.get("kind") == "local_file" and source.get("raw_copied") is False:
            continue
        references.add(digest)
    return references


def _entity_key(value: dict) -> tuple[str, str] | None:
    if not isinstance(value, dict) or not isinstance(value.get("type"), str):
        return None
    kind, content = value["type"], value.get("value")
    if not kind or len(kind) > 128 or isinstance(content, bool) or not isinstance(content, (str, int, float)):
        return None
    try:
        encoded = _canonical(content)
    except (ValueError, TypeError):
        return None
    if len(encoded) > 4096:
        return None
    return kind, encoded


def _graph(reports: list[dict], *, total_runs: int) -> dict:
    entities, relationships = {}, {}
    omitted_entities = omitted_relationships = invalid = 0
    provenance_truncated = False

    def merge_provenance(entry, source_ids, run_id):
        nonlocal provenance_truncated
        for key, values in (("source_ids", source_ids), ("run_ids", [run_id])):
            for value in values:
                if not isinstance(value, str) or len(value) > 256 or value in entry[key]:
                    continue
                if len(entry[key]) < _MAX_GRAPH_PROVENANCE:
                    entry[key].append(value)
                else:
                    provenance_truncated = True

    def add_entity(value, source_ids, run_id):
        nonlocal omitted_entities, invalid
        key = _entity_key(value)
        if key is None:
            invalid += 1
            return None
        if key not in entities:
            if len(entities) >= _MAX_GRAPH_ENTITIES:
                omitted_entities += 1
                return None
            identifier = "entity_" + hashlib.sha256(_canonical([key[0], json.loads(key[1])]).encode("utf-8")).hexdigest()
            entities[key] = {"entity_id": identifier, "type": key[0], "value": json.loads(key[1]),
                             "source_ids": [], "run_ids": []}
        merge_provenance(entities[key], source_ids, run_id)
        return key

    for report in reports:
        run_id = str(report.get("run_id", ""))
        for entity in report.get("entities", []) if isinstance(report.get("entities", []), list) else []:
            if isinstance(entity, dict):
                sources = entity.get("source_ids", [])
                add_entity(entity, sources if isinstance(sources, list) else [], run_id)
            else:
                invalid += 1
        for relationship in report.get("relationships", []) if isinstance(report.get("relationships", []), list) else []:
            if not isinstance(relationship, dict) or not isinstance(relationship.get("relation"), str) or not 1 <= len(relationship["relation"]) <= 128:
                invalid += 1
                continue
            sources = relationship.get("source_ids", [])
            sources = sources if isinstance(sources, list) else []
            origin = add_entity(relationship.get("source", relationship.get("from")), sources, run_id)
            target = add_entity(relationship.get("target", relationship.get("to")), sources, run_id)
            if origin is None or target is None:
                omitted_relationships += 1
                continue
            key = origin, relationship["relation"], target
            if key not in relationships:
                if len(relationships) >= _MAX_GRAPH_RELATIONSHIPS:
                    omitted_relationships += 1
                    continue
                relationships[key] = {"source": {"type": entities[origin]["type"], "value": entities[origin]["value"],
                                                   "entity_id": entities[origin]["entity_id"]},
                                      "relation": relationship["relation"],
                                      "target": {"type": entities[target]["type"], "value": entities[target]["value"],
                                                   "entity_id": entities[target]["entity_id"]},
                                      "source_ids": [], "run_ids": []}
            merge_provenance(relationships[key], sources, run_id)
    return {"entities": list(entities.values()), "relationships": list(relationships.values()),
            "scope": "Explicit entities and relationships from selected runs; no inferred identity or ownership",
            "runs_included": len(reports), "total_runs": total_runs,
            "truncated": total_runs > len(reports) or bool(omitted_entities or omitted_relationships or provenance_truncated),
            "limits": {"entities": _MAX_GRAPH_ENTITIES, "relationships": _MAX_GRAPH_RELATIONSHIPS,
                       "provenance_per_item": _MAX_GRAPH_PROVENANCE},
            "omitted_entities": omitted_entities, "omitted_relationships": omitted_relationships,
            "invalid_graph_items": invalid, "provenance_truncated": provenance_truncated}


class CaseStore:
    def __init__(self, path, create: bool = True):
        self.path = Path(os.path.abspath(os.fspath(path)))
        self.database_path = self.path / "case.sqlite3"
        self.blobs_path = self.path / "blobs"
        _directory(self.path, create=create)
        _directory(self.blobs_path, create=create)
        if create:
            try:
                descriptor = os.open(self.database_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
            except FileExistsError:
                _regular(self.database_path)
            except OSError as exc:
                raise _failure("CASE_DATABASE", "Could not create the case database") from exc
            else:
                os.close(descriptor)
            self._initialize()
        else:
            _regular(self.database_path)
            connection = self._connection(readonly=True)
            try:
                self._check_schema(connection)
            finally:
                connection.close()

    def close(self):
        """Connections are scoped to operations; supplied for context-manager clients."""

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _connection(self, *, readonly=False):
        _directory(self.path, create=False)
        _directory(self.blobs_path, create=False)
        _regular(self.database_path)
        connection = None
        try:
            connection = sqlite3.connect(self.database_path.as_uri() + "?mode=ro", uri=True, timeout=10) if readonly else sqlite3.connect(str(self.database_path), timeout=10)
            connection.row_factory = sqlite3.Row
            connection.execute("PRAGMA busy_timeout=10000")
            connection.execute("PRAGMA foreign_keys=ON")
            if readonly:
                connection.execute("PRAGMA query_only=ON")
            return connection
        except sqlite3.Error as exc:
            if connection is not None:
                connection.close()
            raise _failure("CASE_DATABASE", "Could not open the case database") from exc

    def _check_schema(self, connection):
        try:
            row = connection.execute("SELECT value FROM case_meta WHERE key='schema_version'").fetchone()
            required = {"runs", "artifacts", "run_artifacts", "case_meta"}
            tables = {entry[0] for entry in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if row is None or row[0] != _SCHEMA or not required <= tables:
                raise ValueError("Unknown case schema")
        except (sqlite3.Error, ValueError) as exc:
            raise Failure("CASE_SCHEMA", "Directory does not contain a compatible Fieldkit case", "invalid") from exc

    def _initialize(self):
        # WAL selection can race another constructor; retry only the transient
        # lock condition. All actual run writes remain transactional.
        for attempt in range(20):
            connection = self._connection()
            try:
                tables = {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")}
                if tables:
                    self._check_schema(connection)
                connection.execute("PRAGMA journal_mode=WAL")
                connection.execute("PRAGMA synchronous=FULL")
                connection.execute("BEGIN IMMEDIATE")
                connection.execute("CREATE TABLE IF NOT EXISTS case_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
                connection.execute("CREATE TABLE IF NOT EXISTS runs(run_id TEXT PRIMARY KEY,command TEXT NOT NULL,target TEXT,status TEXT NOT NULL,started_at TEXT,result_json TEXT NOT NULL,report_sha256 TEXT NOT NULL)")
                connection.execute("CREATE TABLE IF NOT EXISTS artifacts(sha256 TEXT PRIMARY KEY,size_bytes INTEGER NOT NULL,relative_path TEXT NOT NULL)")
                connection.execute("CREATE TABLE IF NOT EXISTS run_artifacts(run_id TEXT NOT NULL REFERENCES runs(run_id),sha256 TEXT NOT NULL REFERENCES artifacts(sha256),PRIMARY KEY(run_id,sha256))")
                connection.execute("INSERT OR IGNORE INTO case_meta VALUES('schema_version',?)", (_SCHEMA,))
                connection.commit()
                return
            except sqlite3.OperationalError as exc:
                connection.rollback()
                if "locked" not in str(exc).lower() or attempt == 19:
                    raise _failure("CASE_DATABASE", "Could not initialize the case database") from exc
                time.sleep(0.025)
            except BaseException:
                connection.rollback()
                raise
            finally:
                connection.close()

    def _blob_hash(self, digest: str) -> tuple[str, int]:
        if not isinstance(digest, str) or _DIGEST.fullmatch(digest) is None:
            raise _failure("CASE_ARTIFACT_CORRUPT", "Malformed artifact digest")
        path = self.blobs_path / digest
        info = _regular(path)
        if info.st_size > _MAX_ARTIFACT_BYTES:
            raise _failure("CASE_ARTIFACT_CORRUPT", "Artifact exceeds the case size limit", details={"sha256": digest})
        try:
            descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
            with os.fdopen(descriptor, "rb") as stream:
                current = os.fstat(stream.fileno())
                if not stat.S_ISREG(current.st_mode) or current.st_nlink != 1:
                    raise Failure("CASE_PATH_UNSAFE", "Artifact is not a regular private file", "invalid")
                hasher, size = hashlib.sha256(), 0
                while True:
                    chunk = stream.read(min(65536, _MAX_ARTIFACT_BYTES - size + 1))
                    if not chunk:
                        break
                    size += len(chunk)
                    if size > _MAX_ARTIFACT_BYTES:
                        raise _failure("CASE_ARTIFACT_CORRUPT", "Artifact grew beyond the case size limit")
                    hasher.update(chunk)
            return hasher.hexdigest(), size
        except OSError as exc:
            raise _failure("CASE_ARTIFACT_ACCESS", "Could not read an evidence artifact", details={"sha256": digest}) from exc

    def _write_blob(self, digest: str, body: bytes):
        path = self.blobs_path / digest
        try:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
        except FileExistsError:
            actual, size = self._blob_hash(digest)
            if actual != digest or size != len(body):
                raise _failure("CASE_ARTIFACT_CORRUPT", "Existing artifact failed its hash check", details={"sha256": digest})
            return
        except OSError as exc:
            raise _failure("CASE_ARTIFACT_WRITE", "Could not create an evidence artifact") from exc
        try:
            view, offset = memoryview(body), 0
            while offset < len(view):
                wrote = os.write(descriptor, view[offset:])
                if wrote <= 0:
                    raise OSError("Short artifact write")
                offset += wrote
            os.fsync(descriptor)
        except OSError as exc:
            # A failed partial blob remains detectable rather than replacing or
            # deleting a path that another process could now be using.
            raise _failure("CASE_ARTIFACT_WRITE", "Could not finish writing an evidence artifact") from exc
        finally:
            os.close(descriptor)

    def save(self, report: dict, artifacts: dict[str, bytes]) -> dict:
        if not isinstance(report, dict) or not isinstance(artifacts, dict):
            raise Failure("INVALID_REPORT", "A case run needs a report object and artifact mapping", "invalid")
        run_id = report.get("run_id")
        command, status = report.get("command"), report.get("status")
        if not isinstance(run_id, str) or not 1 <= len(run_id) <= 128 or not isinstance(command, str) or not command or not isinstance(status, str) or not status:
            raise Failure("INVALID_REPORT", "Report requires run_id, command, and status strings", "invalid")
        try:
            serialized = _canonical(report)
            encoded = serialized.encode("utf-8")
        except (TypeError, ValueError, UnicodeError, RecursionError) as exc:
            raise Failure("INVALID_REPORT", "Report must contain finite, UTF-8 JSON data", "invalid") from exc
        if len(encoded) > _MAX_REPORT_BYTES:
            raise Failure("REPORT_TOO_LARGE", "Case report exceeds 32 MiB", "invalid")
        total = 0
        for digest, body in artifacts.items():
            if not isinstance(digest, str) or _DIGEST.fullmatch(digest) is None or not isinstance(body, bytes):
                raise Failure("INVALID_ARTIFACT", "Artifacts require lowercase SHA-256 keys and byte values", "invalid")
            total += len(body)
            if total > _MAX_ARTIFACT_BYTES:
                raise Failure("ARTIFACT_BUDGET", "Artifacts for one run exceed 32 MiB", "invalid")
            if hashlib.sha256(body).hexdigest() != digest:
                raise Failure("ARTIFACT_HASH", "Artifact content does not match its SHA-256 key", "invalid")
        references = _source_refs(report)
        report_sha256 = hashlib.sha256(encoded).hexdigest()
        timing = report.get("timing", {})
        started = report.get("started_at") or (timing.get("started_at") if isinstance(timing, dict) else None)
        if started is not None and not isinstance(started, str):
            raise Failure("INVALID_REPORT", "Report started_at must be a timestamp string", "invalid")
        target = report.get("target")
        target_text = target if isinstance(target, str) else _canonical(target)
        connection = self._connection()
        try:
            connection.execute("BEGIN IMMEDIATE")
            if connection.execute("SELECT 1 FROM runs WHERE run_id=?", (run_id,)).fetchone():
                raise Failure("RUN_EXISTS", "Case already contains this run_id; existing runs are immutable", "invalid")
            all_digests = references | set(artifacts)
            for digest in references - set(artifacts):
                row = connection.execute("SELECT size_bytes,relative_path FROM artifacts WHERE sha256=?", (digest,)).fetchone()
                if row is None:
                    raise Failure("MISSING_ARTIFACT", "A report source has no supplied or stored artifact", "invalid",
                                  details={"sha256": digest})
                actual, size = self._blob_hash(digest)
                if actual != digest or row["size_bytes"] != size or row["relative_path"] != "blobs/" + digest:
                    raise _failure("CASE_ARTIFACT_CORRUPT", "Referenced case artifact failed validation", details={"sha256": digest})
            for digest, body in artifacts.items():
                self._write_blob(digest, body)
                existing = connection.execute("SELECT size_bytes,relative_path FROM artifacts WHERE sha256=?", (digest,)).fetchone()
                if existing and (existing["size_bytes"] != len(body) or existing["relative_path"] != "blobs/" + digest):
                    raise _failure("CASE_ARTIFACT_CORRUPT", "Stored artifact metadata does not match its content")
                connection.execute("INSERT OR IGNORE INTO artifacts VALUES(?,?,?)", (digest, len(body), "blobs/" + digest))
            connection.execute("INSERT INTO runs VALUES(?,?,?,?,?,?,?)", (run_id, command, target_text, status, started, serialized, report_sha256))
            connection.executemany("INSERT INTO run_artifacts VALUES(?,?)", ((run_id, digest) for digest in sorted(all_digests)))
            connection.commit()
            return {"run_id": run_id, "report_sha256": report_sha256}
        except sqlite3.Error as exc:
            connection.rollback()
            raise _failure("CASE_WRITE", "Could not commit the case run") from exc
        except BaseException:
            connection.rollback()
            raise
        finally:
            connection.close()

    def _selection(self, limit: int):
        _limit(limit)
        connection = self._connection(readonly=True)
        try:
            self._check_schema(connection)
            total = connection.execute("SELECT count(*) FROM runs").fetchone()[0]
            summary = {"run_count": total,
                       "artifact_count": connection.execute("SELECT count(*) FROM artifacts").fetchone()[0],
                       "artifact_bytes": connection.execute("SELECT coalesce(sum(size_bytes),0) FROM artifacts").fetchone()[0],
                       "by_status": dict(connection.execute("SELECT status,count(*) FROM runs GROUP BY status")),
                       "by_command": dict(connection.execute("SELECT command,count(*) FROM runs GROUP BY command"))}
            reports, records, artifacts, total_bytes = [], [], {}, 0
            query = "SELECT run_id,command,status,started_at,report_sha256,length(result_json) AS json_length,CASE WHEN length(result_json)<=? THEN result_json END AS result_json FROM runs ORDER BY rowid DESC LIMIT ?"
            byte_limited = False
            for row in connection.execute(query, (_MAX_REPORT_BYTES, limit)):
                if row["result_json"] is None:
                    raise _failure("CASE_REPORT_CORRUPT", "Stored report exceeds the case size limit", details={"run_id": row["run_id"]})
                raw = row["result_json"].encode("utf-8")
                if total_bytes + len(raw) > _MAX_SELECTION_BYTES:
                    byte_limited = True
                    break
                total_bytes += len(raw)
                if hashlib.sha256(raw).hexdigest() != row["report_sha256"]:
                    raise _failure("CASE_REPORT_CORRUPT", "Stored report failed its hash check", details={"run_id": row["run_id"]})
                try:
                    report = json.loads(row["result_json"])
                except (ValueError, RecursionError) as exc:
                    raise _failure("CASE_REPORT_CORRUPT", "Stored report contains invalid JSON") from exc
                if not isinstance(report, dict) or report.get("run_id") != row["run_id"]:
                    raise _failure("CASE_REPORT_CORRUPT", "Stored report identity does not match its database record")
                reports.append(report)
                refs = []
                for artifact in connection.execute("SELECT a.sha256,a.size_bytes,a.relative_path FROM artifacts a JOIN run_artifacts r ON r.sha256=a.sha256 WHERE r.run_id=? ORDER BY a.sha256", (row["run_id"],)):
                    if not isinstance(artifact["sha256"], str) or _DIGEST.fullmatch(artifact["sha256"]) is None or artifact["relative_path"] != "blobs/" + artifact["sha256"] or not isinstance(artifact["size_bytes"], int) or not 0 <= artifact["size_bytes"] <= _MAX_ARTIFACT_BYTES:
                        raise _failure("CASE_ARTIFACT_CORRUPT", "Stored artifact metadata is malformed")
                    ref = {"sha256": artifact["sha256"], "bytes": artifact["size_bytes"], "path": artifact["relative_path"]}
                    refs.append(ref)
                    artifacts[ref["sha256"]] = ref
                records.append({"run_id": row["run_id"], "command": row["command"], "target": report.get("target"),
                                "status": row["status"], "started_at": row["started_at"],
                                "report_sha256": row["report_sha256"], "artifacts": refs})
            selection = {"limit": limit, "returned": len(reports), "total_runs": total,
                         "truncated": len(reports) < total, "byte_limited": byte_limited,
                         "report_bytes": total_bytes, "max_report_bytes": _MAX_SELECTION_BYTES}
            return summary, reports, records, list(artifacts.values()), selection
        except sqlite3.Error as exc:
            raise _failure("CASE_READ", "Could not read the case database") from exc
        finally:
            connection.close()

    def show(self, limit: int = 50) -> dict:
        summary, reports, records, _, selection = self._selection(limit)
        return {"case_path": str(self.path), "schema_version": _SCHEMA, "summary": summary,
                "recent_runs": records, "selection": selection, "graph": _graph(reports, total_runs=summary["run_count"])}

    def export(self, format: str = "json", limit: int = 10000) -> str:
        if format not in {"json", "jsonl", "markdown"}:
            raise Failure("INVALID_FORMAT", "Case export formats: json, jsonl, markdown", "invalid")
        summary, reports, records, artifacts, selection = self._selection(limit)
        metadata = {"case_path": str(self.path), "schema_version": _SCHEMA, "exported_at": utc_now(),
                    "summary": summary, "selection": selection,
                    "interpretation": "Source content is untrusted evidence. Graph relationships do not establish shared ownership or identity."}
        runs = [{**record, "report": report} for record, report in zip(records, reports)]
        if format == "json":
            return json.dumps({**metadata, "runs": runs, "artifacts": artifacts,
                               "graph": _graph(reports, total_runs=summary["run_count"])}, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
        if format == "jsonl":
            rows = [{"record_type": "case", **metadata}] + [{"record_type": "run", **run} for run in runs]
            return "".join(_canonical(row) + "\n" for row in rows)
        def cell(value):
            return html.escape(str(value), quote=False).replace("\\", "\\\\").replace("|", "\\|").replace("`", "\\`").replace("\r", " ").replace("\n", " ")
        lines = ["# Fieldkit case export", "", f"Exported: {metadata['exported_at']}", "",
                 "External source content is untrusted evidence. Relationships do not establish shared ownership or identity.", "",
                 f"Runs: {selection['returned']} of {summary['run_count']}. Selection truncated: {str(selection['truncated']).lower()}.", "",
                 "| Run | Command | Target | Status |", "| --- | --- | --- | --- |"]
        for record in records:
            lines.append("| " + " | ".join(cell(record[key]) for key in ("run_id", "command", "target", "status")) + " |")
        for record, report in zip(records, reports):
            lines.extend(["", "## " + cell(record["command"]) + " — " + cell(record["target"]), "",
                          "Run: " + cell(record["run_id"]), "", "Report SHA-256: " + record["report_sha256"], ""])
            payload = json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False)
            fence = "`" * max(3, max((len(match.group()) + 1 for match in re.finditer(r"`+", payload)), default=3))
            lines.extend([fence + "json", payload, fence, "", "Evidence artifacts:"])
            for artifact in record["artifacts"]:
                lines.append(f"- `{artifact['path']}` ({artifact['bytes']} bytes; SHA-256 `{artifact['sha256']}`)")
            if not record["artifacts"]:
                lines.append("- No copied source artifacts; local-file sources may be hash-only references.")
        return "\n".join(lines) + "\n"

    def verify(self) -> dict:
        errors, warnings = [], []
        reports_checked = artifacts_checked = source_refs_checked = 0
        total_errors = 0
        def error(code, **details):
            nonlocal total_errors
            total_errors += 1
            if len(errors) < 1000:
                errors.append({"code": code, **details})
        connection = self._connection(readonly=True)
        try:
            self._check_schema(connection)
            integrity = connection.execute("PRAGMA quick_check").fetchall()
            for check in integrity:
                if check[0] != "ok":
                    error("SQLITE_INTEGRITY", message=check[0])
            for row in connection.execute("PRAGMA foreign_key_check"):
                error("BROKEN_REFERENCE", table=row[0], rowid=row[1])
            artifact_rows = {row["sha256"]: row for row in connection.execute("SELECT * FROM artifacts")}
            for digest, row in artifact_rows.items():
                artifacts_checked += 1
                try:
                    actual, size = self._blob_hash(digest)
                    if actual != digest or size != row["size_bytes"] or row["relative_path"] != "blobs/" + digest:
                        error("ARTIFACT_HASH", sha256=digest)
                except Failure as exc:
                    error(exc.code, sha256=digest, message=exc.message)
            query = "SELECT run_id,command,status,report_sha256,CASE WHEN length(result_json)<=? THEN result_json END AS result_json FROM runs"
            for row in connection.execute(query, (_MAX_REPORT_BYTES,)):
                reports_checked += 1
                if row["result_json"] is None:
                    error("REPORT_TOO_LARGE", run_id=row["run_id"])
                    continue
                raw = row["result_json"].encode("utf-8")
                if hashlib.sha256(raw).hexdigest() != row["report_sha256"]:
                    error("REPORT_HASH", run_id=row["run_id"])
                try:
                    report = json.loads(row["result_json"])
                    if not isinstance(report, dict):
                        raise ValueError("Report is not an object")
                    if report.get("run_id") != row["run_id"] or report.get("command") != row["command"] or report.get("status") != row["status"]:
                        error("REPORT_METADATA", run_id=row["run_id"])
                    references = _source_refs(report)
                    linked = {item[0] for item in connection.execute("SELECT sha256 FROM run_artifacts WHERE run_id=?", (row["run_id"],))}
                    source_refs_checked += len(references)
                    for digest in references:
                        if digest not in linked or digest not in artifact_rows:
                            error("SOURCE_ARTIFACT_REFERENCE", run_id=row["run_id"], sha256=digest)
                except (Failure, ValueError, TypeError, RecursionError):
                    error("REPORT_JSON", run_id=row["run_id"])
            for path in self.blobs_path.iterdir():
                if path.name not in artifact_rows:
                    if len(warnings) < 1000:
                        warnings.append({"code": "UNTRACKED_BLOB", "path": "blobs/" + path.name})
            return {"case_path": str(self.path), "ok": total_errors == 0, "verified_at": utc_now(),
                    "reports_checked": reports_checked, "artifacts_checked": artifacts_checked,
                    "source_refs_checked": source_refs_checked, "errors": errors, "error_count": total_errors,
                    "errors_truncated": total_errors > len(errors), "warnings": warnings,
                    "interpretation": "Checks stored hashes and references; does not authenticate source truth or reread external/local hash-only sources."}
        except sqlite3.Error as exc:
            raise _failure("CASE_VERIFY", "Could not verify the case database") from exc
        finally:
            connection.close()
