# NVIDIA Auto Ontology for Fieldkit

This runs the actual NVIDIA Auto Ontology service beside the Rust Fieldkit client. Fieldkit exports a normalized SQLite evidence projection; Auto Ontology ingests its catalog, compiles a semantic layer, and answers questions through its authenticated gateway. It is a separate service, using NVIDIA's upstream implementation rather than a direct model call labeled as Auto Ontology.

The isolated Compose project publishes only the gateway at `http://127.0.0.1:18760`. Postgres, the Python backend and the ingestion service remain on the private Compose network. Only `data/` is mounted into the service, read-only; the original case database and captured blobs are not mounted. Backend processes run with the host user's numeric UID/GID so the private projection remains readable without making it world-readable.

## Set up

From the Fieldkit repository:

```sh
./integrations/auto-ontology/setup.sh init
```

Edit `integrations/auto-ontology/.env` and fill in `NVIDIA_API_KEY`. The file contains generated database/admin passwords and an auth secret; it is private and ignored by Git. Inference uses the configured model endpoints and sends relevant projected records and metadata there. Set the endpoint/model variables to your own compatible deployments if required. No model calls run during `init` or `build`.

To prepare and verify authentication before configuring an inference key, use:

```sh
./integrations/auto-ontology/setup.sh build
./integrations/auto-ontology/setup.sh start-infra
./integrations/auto-ontology/setup.sh connect-infra
```

This starts Postgres, migrations, the backend and the gateway. It leaves ingestion stopped and replaces model endpoints with a closed container-local port. An existing `data/fieldkit.sqlite` is configured as the source connection; without that file the backend starts with no source. The token-only bootstrap saves a real API token without enabling compilation. After adding the key, run `start` and `connect` below; `connect` reuses that token and enables compilation.

After exporting a projection, metadata-only setup can also register its actual tables, columns and foreign keys in NVIDIA's catalog:

```sh
./fieldkit --case cases/my-investigation nvidia export
./integrations/auto-ontology/setup.sh catalog-infra
```

`catalog-infra` uses NVIDIA's `ingest_catalog(SQLiteDatabase(...))` function inside the external service. This deterministic catalog stage reads schema metadata and writes the real service catalog; it omits embedding, PII-model and semantic-compilation stages. The helper checks that model endpoints remain closed, keeps the ingestion service stopped, and verifies that every source column and declared foreign key reached the catalog. It never marks an uncompiled semantic layer as compiled.

```sh
./integrations/auto-ontology/setup.sh build
./fieldkit --case cases/my-investigation nvidia export
./integrations/auto-ontology/setup.sh start
./integrations/auto-ontology/setup.sh status
```

When the frontend is healthy:

```sh
./integrations/auto-ontology/setup.sh connect
./fieldkit --case cases/my-investigation nvidia status
```

`start` keeps the ingestion worker stopped. `connect` signs into the local bootstrap admin through the official authentication API, creates or reuses an API token in private `fieldkit.env`, and runs one initial catalog-embedding/semantic-compilation pass. It waits for a completed successful pass, then automatically disables scheduled compilation and stops the ingestion worker. A failure, timeout or interrupted setup also stops background work. The default setup timeout is 30 minutes, configurable with `FIELDKIT_AO_SETUP_TIMEOUT` (seconds, maximum 7200). The worker has the same independent container-side time limit and no restart policy, so it also terminates if the setup client is killed before cleanup can run.

Credentials and tokens are never printed by the helper. The Rust client loads its private connection file, or the file named by `FIELDKIT_AUTO_ONTOLOGY_ENV`. Environment variables can select an existing deployment instead: `AUTO_ONTOLOGY_API_URL`, `AUTO_ONTOLOGY_API_TOKEN`, `AUTO_ONTOLOGY_TARGET_DB`, and `AUTO_ONTOLOGY_TIMEOUT`.

The first catalog ingestion and semantic compilation require model/embedding calls and can take minutes. An uncompiled service returns HTTP 409 from chat; an `evidence` payload does not bypass compilation. Inspect status before requesting reasoning.

The compiler's `calculated` flag becomes true as soon as a Term exists, so setup also requires the worker to be idle and a fresh successful completion timestamp before treating the initial pass as complete.

Upstream's semantic scheduler awaits the data scheduler's first pass before compiling. Consequently, waiting for that fresh completed semantic pass also waits for the initial vector-ingestion attempt to finish, even when metadata was pre-populated by `catalog-infra`; freezing cannot cut off that attempt mid-flight. This ordering does not independently certify embedding coverage: upstream treats a failed ingestion attempt as finished too, so inspect service errors if subsequent questions cannot retrieve the catalog.

## Evidence and refreshing

The default source is `/fieldkit/fieldkit.sqlite`, so the database name is `fieldkit`. SQLite is supported through `CONNECTION_STRINGS`; upstream's UI-managed connection types do not currently include SQLite.

The projection must be updated in-place with a SQLite transaction. Upstream caches SQLite connections, so replacing the file by rename can leave the service reading an old inode. Mounting the directory also allows the service to see the current stable file. Fieldkit retains its original captures and makes any model-suggested next action pass its local evidence, scope, collector and budget checks.

Do not place captured page text or bios directly into Auto Ontology's `evidence` request field: upstream treats that field as authoritative SQL instructions. Fieldkit uses it for trusted schema guidance and constraints; external text remains attributed database rows.

## Operation

```sh
./integrations/auto-ontology/setup.sh logs ingestion-service
./integrations/auto-ontology/setup.sh freeze
./integrations/auto-ontology/setup.sh stop
```

`freeze` disables the periodic semantic compiler and stops the ingestion worker without resetting the catalog, embeddings, terms or source connection. The backend/gateway remain running and can answer explicit Fieldkit questions against the retained semantic layer and current in-place SQLite projection. The worker is in an opt-in Compose profile, so an ordinary Compose startup also leaves it stopped. This prevents NVIDIA's default 24-hour maintenance passes from running outside Fieldkit's investigation budgets. To explicitly refresh compilation, run `connect` again.

`stop` shuts down the gateway as well, preserving the project's Postgres volume and local projection. To inspect the service UI, use the gateway URL and the bootstrap admin credentials in private `.env`. User menu → API Tokens manages/revokes tokens.

Source is pinned to NVIDIA commit `fc43a82600e18b19d159c52023f54f5572f537ac`. Setup selects the native CPU architecture (`linux/arm64` on Apple Silicon, `linux/amd64` on x86); NVIDIA's base image supports both. Native builds avoid dependency-build crashes observed under amd64 emulation. NVIDIA Auto Ontology is Apache-2.0; the service is built from its own checkout and is not vendored into Fieldkit.

Upstream references: [repository](https://github.com/NVIDIA/auto-ontology), [authentication](https://github.com/NVIDIA/auto-ontology/blob/fc43a82600e18b19d159c52023f54f5572f537ac/docs/authentication.md), [conversation API](https://github.com/NVIDIA/auto-ontology/blob/fc43a82600e18b19d159c52023f54f5572f537ac/docs/conversation-api.md), [SQLite connector](https://github.com/NVIDIA/auto-ontology/blob/fc43a82600e18b19d159c52023f54f5572f537ac/auto_ontology/connectors/sqlite.py).
