# Local integration checks

Verified on 2026-11-11 with Docker Desktop 29.5.2 on Apple Silicon, using NVIDIA source commit `fc43a82600e18b19d159c52023f54f5572f537ac`.

- Shell syntax, Node module syntax and both Compose configurations validate.
- Four bootstrap contract scenarios pass: normal setup, compiler-enable failure without an orphan token, token-only bootstrap, and enabling compilation with an existing API token.
- Both upstream images build successfully for native `linux/arm64`. An initial `linux/amd64` emulated build crashed in a dependency's hatchling process; native architecture selection resolved it without changing upstream source.
- The actual NVIDIA `SQLiteDatabase` connector read a host-owned mode-0600 test projection from a read-only directory mount. An attempted write failed as read-only. The test container had networking disabled, and its fixture was removed afterward.
- The isolated authentication stack started with healthy backend/frontend services. Ingestion was not started, and model endpoints were replaced with a closed container-local address.
- A real bootstrap login created an API token, retained only in ignored mode-0600 `fieldkit.env`. Authenticated gateway checks returned:
  - `GET /api/configurations/semantic-compilation`: 200, `enabled=false`.
  - `GET /api/semantic-compilation/status`: 200, `calculated=false`, `running=false`.
  - `GET /api/datasources/dbs`: 200, empty catalog.
  - `POST /api/chat/completions`: 409, semantic layer not yet created.
- After the Rust client exported its normalized projection, `setup.sh catalog-infra` configured that actual SQLite source and ran NVIDIA's deterministic metadata-only catalog importer. All 13 source tables, 86 columns and 18 declared foreign keys were retained; 18 foreign keys were verified in NVIDIA's Postgres catalog. Model endpoints remained closed and ingestion stayed stopped.
- The actual Rust `fieldkit nvidia status` command then reported a healthy authenticated gateway, `catalog_present=true`, `configured_connection=true`, `databases=["fieldkit"]`, and `status=needs_compilation`. The semantic layer remained accurately uncompiled.
- The real `freeze` command disabled periodic compilation through the authenticated configuration API and stopped the ingestion worker. The gateway, projection connection and catalog remained available. Source review confirms that upstream chat gates on retained Terms, independently of the scheduler's enabled flag; no catalog or semantic-reset endpoint is used.
- Semantic-control fixtures cover a still-running partial layer, fresh successful completion, prior failures, timeout, and both token/admin freeze authentication paths. Shell cleanup fixtures verify that successful and failed setup commands both stop the worker. The ingestion worker is opt-in, has no restart policy, and has an independent container-side timeout.
- The independent watchdog was exercised in the actual upstream image with networking disabled: a sleeping fixture process was terminated after one second with GNU timeout's expected exit status 124.
- Upstream's semantic scheduler explicitly awaits the data scheduler's first pass, so setup's completed-semantic-pass wait cannot race the initial vector-ingestion attempt. A completed attempt can still have failed; these checks do not claim live embedding coverage without an inference key.

Live catalog embedding, semantic compilation and model-backed reasoning have not run: they require a configured inference credential. This validation establishes real service deployment, authentication, API shape and read-only projection access; it does not claim that live reasoning has completed.
