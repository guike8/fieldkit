"""Loopback case application. UI, terminal, CLI and MCP use one evidence model."""
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import os
from pathlib import Path
import secrets
import stat
import sys
import threading
import uuid
from urllib.parse import parse_qs, urlsplit

from .cli import Config, bounded_float, bounded_int, capabilities
from .common import Failure, utc_now
from .manifest import COMMANDS, validate_job
from .mcp_server import _scoped_path
from .transport import strict_json
from .workbench_api import OPERATIONS, apply_operation, operation_schema
from .manifest import validate

ASSETS = Path(__file__).parent / "web_assets"


class CaseApplication:
    def __init__(self, workspace, case, *, config=None):
        from .workbench import Workbench
        self.workspace = Path(workspace).expanduser().resolve(strict=True)
        if not self.workspace.is_dir():
            raise Failure("APP_WORKSPACE", "Workspace must be an existing directory", "invalid")
        self.case = _scoped_path(self.workspace, str(case), must_exist=False)
        self.model = Workbench(self.case, config=config, job_policy=self.scope_job)
        self.token = secrets.token_urlsafe(32)
        self.jobs = {}
        self.lock = threading.Lock()
        self.pool = ThreadPoolExecutor(max_workers=2, thread_name_prefix="fieldkit-case")

    def close(self):
        self.pool.shutdown(wait=True, cancel_futures=True)

    def scope_job(self, command, target, options=None):
        job = {"command": command, "options": options or {}}
        if target is not None:
            job["target"] = target
        validate_job(job)
        if COMMANDS[command].get("local_target"):
            target = str(_scoped_path(self.workspace, target, must_exist=True))
        return command, target, options or {}

    def _run(self, identifier, operation, payload):
        with self.lock:
            self.jobs[identifier]["status"] = "running"
        try:
            if operation == "tools/run":
                command, target, options = self.scope_job(payload["command"], payload.get("target"), payload.get("options"))
                report = self.model.run_tool(command, target, options)
            else:
                state = self.model.snapshot()
                flow = next((row for row in state["flows"] if row["id"] == payload["flow_id"]), None)
                step = next((row for row in flow["steps"] if row["id"] == payload["step_id"]), None) if flow else None
                if not step:
                    raise Failure("FLOW_STEP_NOT_FOUND", "Research step does not exist", "invalid")
                self.scope_job(step["command"], step.get("target"), step.get("options"))
                report = self.model.run_step(payload["flow_id"], payload["step_id"])
            with self.lock:
                self.jobs[identifier].update(status="completed" if report.get("status") in {"ok", "not_found"} else "failed",
                    result_status=report.get("status"), run_id=report.get("run_id"), completed_at=utc_now(), errors=report.get("errors", []))
        except Exception as exc:
            error = exc.as_dict() if isinstance(exc, Failure) else {"code": "JOB_ERROR", "message": "Collector job failed", "retryable": False}
            with self.lock:
                self.jobs[identifier].update(status="failed", completed_at=utc_now(), error=error)

    def operation(self, name, payload):
        if name not in OPERATIONS:
            raise Failure("APP_OPERATION", "Unknown application operation", "invalid")
        validate(payload, operation_schema(name), "arguments")
        if name in {"tools/run", "flows/run"}:
            if name == "tools/run":
                self.scope_job(payload["command"], payload.get("target"), payload.get("options"))
            with self.lock:
                if sum(job["status"] in {"queued", "running"} for job in self.jobs.values()) >= 8:
                    raise Failure("JOB_QUEUE_FULL", "Wait for a running collector before adding another", "error")
                if len(self.jobs) >= 100:
                    removable = next((key for key, job in self.jobs.items() if job["status"] not in {"queued", "running"}), None)
                    if removable:
                        del self.jobs[removable]
                identifier = "job_" + uuid.uuid4().hex
                self.jobs[identifier] = {"id": identifier, "status": "queued", "operation": name,
                    "command": payload.get("command"), "target": payload.get("target"), "created_at": utc_now()}
            self.pool.submit(self._run, identifier, name, payload)
            return {"ok": True, "job_id": identifier, "state": self.model.snapshot()}, 202
        if name == "flows/steps":
            command, target, options = self.scope_job(payload["command"], payload.get("target"), payload.get("options"))
            payload = {"flow_id": payload["flow_id"], "command": command, "options": options, **({"target": target} if target else {})}
        if name == "flows":
            steps = []
            for step in payload.get("steps", []):
                command, target, options = self.scope_job(step["command"], step.get("target"), step.get("options"))
                steps.append({"command": command, "options": options, **({"target": target} if target else {})})
            payload = {**payload, "steps": steps}
        record = apply_operation(self.model, name, payload)
        return {"ok": True, "record": record, "state": self.model.snapshot()}, 200


class CaseHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, application):
        self.application = application
        super().__init__(address, CaseHandler)
        port = self.server_address[1]
        self.allowed_hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}


class CaseHandler(BaseHTTPRequestHandler):
    server_version = "FieldkitCase/0.4"

    def log_message(self, *args):
        return

    def _headers(self, status, media, length, *, attachment=None):
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(length))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
        if attachment:
            self.send_header("Content-Disposition", 'attachment; filename="' + attachment + '"')
        self.end_headers()

    def _send(self, payload, status=200):
        body = json.dumps(payload, ensure_ascii=True, allow_nan=False, separators=(",", ":")).encode()
        self._headers(status, "application/json; charset=utf-8", len(body))
        self.wfile.write(body)

    def _guard(self):
        host = self.headers.get("Host", "").lower()
        if host not in self.server.allowed_hosts:
            raise Failure("APP_HOST", "Unrecognized local application host", "invalid")
        origin = self.headers.get("Origin")
        if origin and origin not in {"http://" + allowed for allowed in self.server.allowed_hosts}:
            raise Failure("APP_ORIGIN", "Cross-origin application requests are not supported", "invalid")
        if self.headers.get("Sec-Fetch-Site") in {"cross-site", "same-site"}:
            raise Failure("APP_ORIGIN", "Open the application directly on its local origin", "invalid")

    def do_GET(self):
        try:
            self._guard()
            route = urlsplit(self.path)
            app = self.server.application
            if route.path == "/api/session":
                return self._send({"csrf_token": app.token, "mode": "local"})
            if route.path == "/api/state":
                return self._send(app.model.snapshot())
            if route.path == "/api/capabilities":
                return self._send(capabilities())
            if route.path == "/api/jobs":
                with app.lock:
                    jobs = list(app.jobs.values())
                return self._send({"jobs": jobs})
            if route.path == "/api/export":
                format = parse_qs(route.query).get("format", ["json"])[0]
                if format not in {"json", "markdown"}:
                    raise Failure("APP_EXPORT", "Export format must be json or markdown", "invalid")
                body = app.model.export(format=format).encode("utf-8")
                self._headers(200, "application/json" if format == "json" else "text/markdown; charset=utf-8", len(body), attachment="fieldkit-case." + ("json" if format == "json" else "md"))
                return self.wfile.write(body)
            if route.path.startswith("/api/artifacts/"):
                digest = route.path.removeprefix("/api/artifacts/")
                if len(digest) != 64 or any(ch not in "0123456789abcdef" for ch in digest):
                    raise Failure("APP_ARTIFACT", "Invalid artifact hash", "invalid")
                path = app.case / "blobs" / digest
                descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
                with os.fdopen(descriptor, "rb") as stream:
                    info = os.fstat(stream.fileno())
                    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 32 * 1024 * 1024:
                        raise Failure("APP_ARTIFACT", "Artifact cannot be served", "invalid")
                    body = stream.read(32 * 1024 * 1024 + 1)
                if hashlib.sha256(body).hexdigest() != digest:
                    raise Failure("ARTIFACT_INTEGRITY", "Stored artifact does not match its hash")
                self._headers(200, "application/octet-stream", len(body), attachment=digest + ".bin")
                return self.wfile.write(body)
            static = {"/": ("index.html", "text/html; charset=utf-8"), "/index.html": ("index.html", "text/html; charset=utf-8"),
                      "/app.js": ("app.js", "text/javascript; charset=utf-8"), "/styles.css": ("styles.css", "text/css; charset=utf-8")}
            if route.path in static:
                filename, media = static[route.path]
                body = (ASSETS / filename).read_bytes()
                self._headers(200, media, len(body))
                return self.wfile.write(body)
            self._send({"ok": False, "error": {"code": "NOT_FOUND", "message": "Route not found"}}, 404)
        except Failure as exc:
            self._send({"ok": False, "error": exc.as_dict()}, 403 if exc.code in {"APP_HOST", "APP_ORIGIN"} else 400)
        except FileNotFoundError:
            self._send({"ok": False, "error": {"code": "NOT_FOUND", "message": "Resource not found"}}, 404)
        except (OSError, ValueError, TypeError):
            self._send({"ok": False, "error": {"code": "APP_ERROR", "message": "Could not read case resource"}}, 500)

    def do_POST(self):
        try:
            self._guard()
            app = self.server.application
            if not secrets.compare_digest(self.headers.get("X-Fieldkit-Token", ""), app.token):
                raise Failure("APP_TOKEN", "Refresh the application before submitting a change", "invalid")
            route = urlsplit(self.path).path
            if self.headers.get("Content-Type", "").split(";", 1)[0] != "application/json":
                raise Failure("APP_CONTENT", "Application requests must use JSON", "invalid")
            length = self.headers.get("Content-Length", "")
            maximum = 6 * 1024 * 1024 if route == "/api/attachments" else 65536
            if not length.isdigit() or not 0 < int(length) <= maximum:
                raise Failure("APP_BODY_LIMIT", "Request exceeds the operation's size limit", "invalid")
            self.connection.settimeout(10)
            payload = strict_json(self.rfile.read(int(length)).decode("utf-8"))
            if route == "/api/attachments":
                if not isinstance(payload, dict) or set(payload) != {"name", "content_base64", "media_type"}:
                    raise Failure("ATTACHMENT_SCHEMA", "Supply attachment name, media_type, and content_base64", "invalid")
                if not isinstance(payload["name"], str) or not 1 <= len(payload["name"]) <= 255 or not isinstance(payload["media_type"], str) or len(payload["media_type"]) > 100:
                    raise Failure("ATTACHMENT_SCHEMA", "Attachment metadata is invalid", "invalid")
                body = base64.b64decode(payload["content_base64"], validate=True)
                if len(body) > 4 * 1024 * 1024:
                    raise Failure("ATTACHMENT_LIMIT", "Attachments are limited to 4 MiB", "invalid")
                record = app.model.add_attachment(payload["name"], body, payload["media_type"])
                return self._send({"ok": True, "record": record, "state": app.model.snapshot()})
            response, status = app.operation(route.removeprefix("/api/"), payload)
            self._send(response, status)
        except Failure as exc:
            self._send({"ok": False, "error": exc.as_dict()}, 403 if exc.code in {"APP_HOST", "APP_ORIGIN", "APP_TOKEN"} else 400)
        except (ValueError, UnicodeError, TypeError):
            self._send({"ok": False, "error": {"code": "APP_JSON", "message": "Invalid JSON or attachment encoding"}}, 400)
        except Exception:
            self._send({"ok": False, "error": {"code": "APP_ERROR", "message": "Case operation failed"}}, 500)


def main(argv=None):
    parser = argparse.ArgumentParser(prog="fieldkit-app", description="Local Fieldkit evidence and workflow application")
    parser.add_argument("--workspace", default=".")
    parser.add_argument("--case", default="cases/default")
    parser.add_argument("--port", type=bounded_int(1, 65535), default=8765)
    parser.add_argument("--host", default="127.0.0.1", choices=["127.0.0.1", "localhost"])
    parser.add_argument("--demo", action="store_true", help="Seed an empty case with synthetic examples")
    parser.add_argument("--timeout", type=bounded_float, default=10)
    args = parser.parse_args(argv)
    application = None
    try:
        application = CaseApplication(args.workspace, args.case, config=Config(timeout=args.timeout))
        if args.demo:
            application.model.seed_demo()
        server = CaseHTTPServer(("127.0.0.1", args.port), application)
        print(f"Fieldkit case app: http://127.0.0.1:{server.server_address[1]}/", file=sys.stderr, flush=True)
        try:
            server.serve_forever(poll_interval=0.2)
        finally:
            server.server_close()
        return 0
    except (Failure, OSError) as exc:
        message = exc.message if isinstance(exc, Failure) else "Could not open the local application"
        print(message, file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130
    finally:
        if application:
            application.close()


if __name__ == "__main__":
    raise SystemExit(main())
