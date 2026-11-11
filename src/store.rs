//! Backwards-compatible append-only collection ledger and case workspace.
use crate::model::{Collected, digest, id, now, text};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Store {
    pub path: PathBuf,
}

fn real(path: &Path, directory: bool) -> Result<()> {
    let m =
        fs::symlink_metadata(path).with_context(|| format!("Cannot inspect {}", path.display()))?;
    ensure!(
        !m.file_type().is_symlink() && if directory { m.is_dir() } else { m.is_file() },
        "Case paths must be real directories and regular files: {}",
        path.display()
    );
    #[cfg(unix)]
    if !directory {
        use std::os::unix::fs::MetadataExt;
        ensure!(m.nlink() == 1, "Case files cannot be hard linked");
    }
    Ok(())
}

pub fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .with_context(|| format!("Cannot create new file {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

impl Store {
    pub fn lock(&self, name: &str) -> Result<std::fs::File> {
        ensure!(
            name.ends_with(".lock")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "Invalid case lock name"
        );
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let path = self.path.join(name);
        let file = options.open(&path)?;
        real(&path, false)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("Another worker is updating this case operation")?;
        Ok(file)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::path::absolute(path)?;
        if fs::symlink_metadata(&raw).is_ok() {
            real(&raw, true)?;
        } else {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&raw)?;
        }
        // Resolve operating-system aliases such as macOS /var and /tmp.
        // App/MCP additionally validate every component inside their workspace.
        let path = raw.canonicalize()?;
        let blobs = path.join("blobs");
        if !blobs.exists() {
            fs::create_dir(&blobs)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&blobs, fs::Permissions::from_mode(0o700))?;
            }
        }
        real(&blobs, true)?;
        let database = path.join("case.sqlite3");
        if !database.exists() {
            private_file(&database, &[])?;
        }
        real(&database, false)?;
        let store = Self { path };
        let mut conn = store.connection()?;
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if count > 0 {
            let schema: String = conn
                .query_row(
                    "SELECT value FROM case_meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .context("Unrecognized case database")?;
            ensure!(schema == "1", "Unsupported case schema {schema}");
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS case_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS runs(run_id TEXT PRIMARY KEY,command TEXT NOT NULL,target TEXT,status TEXT NOT NULL,started_at TEXT,result_json TEXT NOT NULL,report_sha256 TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS artifacts(sha256 TEXT PRIMARY KEY,size_bytes INTEGER NOT NULL,relative_path TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS run_artifacts(run_id TEXT NOT NULL REFERENCES runs(run_id),sha256 TEXT NOT NULL REFERENCES artifacts(sha256),PRIMARY KEY(run_id,sha256));
            INSERT OR IGNORE INTO case_meta VALUES('schema_version','1');
            CREATE TABLE IF NOT EXISTS wb_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS wb_items(kind TEXT NOT NULL,id TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS wb_activity(id TEXT PRIMARY KEY,payload TEXT NOT NULL,previous_hash TEXT NOT NULL,event_hash TEXT NOT NULL);")?;
        let timestamp = now();
        let metadata = json!({"case_id":id("case"),"title":"Untitled investigation","question":"","created_at":timestamp,"updated_at":timestamp,"is_demo":false});
        tx.execute(
            "INSERT OR IGNORE INTO wb_meta VALUES('metadata',?1)",
            [serde_json::to_string(&metadata)?],
        )?;
        tx.commit()?;
        Ok(store)
    }

    pub fn connection(&self) -> Result<Connection> {
        real(&self.path, true)?;
        real(&self.path.join("blobs"), true)?;
        real(&self.path.join("case.sqlite3"), false)?;
        let conn = Connection::open(self.path.join("case.sqlite3"))?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    pub fn save_report(&self, collected: &Collected) -> Result<()> {
        let report = &collected.report;
        let run_id = text(report, "run_id");
        ensure!(
            !run_id.is_empty() && run_id.len() <= 160,
            "Report requires a bounded run ID"
        );
        let serialized = serde_json::to_string(report)?;
        ensure!(
            serialized.len() <= 32 * 1024 * 1024,
            "Report exceeds 32 MiB"
        );
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut refs = Vec::new();
        for source in report["sources"].as_array().into_iter().flatten() {
            if source["kind"] == "local_file" && source["raw_copied"] == false {
                continue;
            }
            if let Some(hash) = source["sha256"].as_str() {
                ensure!(valid_hash(hash), "Malformed source digest");
                refs.push(hash.to_owned());
            }
        }
        refs.extend(collected.blobs.keys().cloned());
        refs.sort();
        refs.dedup();
        for hash in &refs {
            ensure!(valid_hash(hash), "Malformed artifact digest");
            let blob = self.path.join("blobs").join(hash);
            if let Some(bytes) = collected.blobs.get(hash) {
                ensure!(
                    bytes.len() <= 32 * 1024 * 1024 && digest(bytes) == *hash,
                    "Artifact hash/size mismatch"
                );
                if !blob.exists() {
                    private_file(&blob, bytes)?;
                }
            }
            real(&blob, false)?;
            let bytes = fs::read(&blob)?;
            ensure!(
                bytes.len() <= 32 * 1024 * 1024 && digest(&bytes) == *hash,
                "Stored artifact hash mismatch"
            );
            tx.execute(
                "INSERT OR IGNORE INTO artifacts VALUES(?1,?2,?3)",
                params![hash, bytes.len() as i64, format!("blobs/{hash}")],
            )?;
            let record: (i64, String) = tx.query_row(
                "SELECT size_bytes,relative_path FROM artifacts WHERE sha256=?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(
                record.0 == bytes.len() as i64 && record.1 == format!("blobs/{hash}"),
                "Artifact metadata mismatch"
            );
        }
        tx.execute(
            "INSERT INTO runs VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                run_id,
                text(report, "command"),
                report["target"].as_str(),
                text(report, "status"),
                report["timing"]["started_at"].as_str(),
                serialized,
                digest(serialized.as_bytes())
            ],
        )?;
        for hash in refs {
            tx.execute(
                "INSERT INTO run_artifacts VALUES(?1,?2)",
                params![run_id, hash],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn reports(&self, limit: usize) -> Result<Vec<Value>> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT result_json,report_sha256 FROM runs ORDER BY rowid DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit.min(10000) as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut output = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let (body, hash) = row?;
            bytes += body.len();
            ensure!(
                bytes <= 64 * 1024 * 1024,
                "Case selection exceeds 64 MiB; use a smaller selection"
            );
            ensure!(
                digest(body.as_bytes()) == hash,
                "Stored report has been altered"
            );
            output.push(serde_json::from_str(&body)?);
        }
        Ok(output)
    }

    pub fn report_hash(&self, run_id: &str) -> Result<String> {
        Ok(self.connection()?.query_row(
            "SELECT report_sha256 FROM runs WHERE run_id=?1",
            [run_id],
            |r| r.get(0),
        )?)
    }

    pub fn list(&self, kind: &str) -> Result<Vec<Value>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT payload FROM wb_items WHERE kind=?1 ORDER BY created_at DESC,id LIMIT 10000",
        )?;
        let values = stmt.query_map([kind], |r| r.get::<_, String>(0))?;
        values.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn get(&self, kind: &str, identifier: &str) -> Result<Value> {
        let conn = self.connection()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT payload FROM wb_items WHERE kind=?1 AND id=?2",
                params![kind, identifier],
                |r| r.get(0),
            )
            .optional()?;
        let value = value.with_context(|| format!("Unknown {kind} record"))?;
        Ok(serde_json::from_str(&value)?)
    }

    pub fn put(&self, kind: &str, value: &Value) -> Result<()> {
        ensure!(
            !text(value, "id").is_empty(),
            "Workspace record needs an ID"
        );
        let body = serde_json::to_string(value)?;
        ensure!(body.len() <= 512 * 1024, "Workspace record exceeds 512 KiB");
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wb_items WHERE kind=?1 AND id=?2)",
            params![kind, text(value, "id")],
            |r| r.get(0),
        )?;
        if !exists {
            let count: i64 =
                tx.query_row("SELECT count(*) FROM wb_items WHERE kind=?1", [kind], |r| {
                    r.get(0)
                })?;
            ensure!(count < 10000, "Workspace record limit reached for {kind}");
        }
        tx.execute("INSERT INTO wb_items VALUES(?1,?2,?3,?4) ON CONFLICT(kind,id) DO UPDATE SET payload=excluded.payload", params![kind, text(value,"id"), body, value["created_at"].as_str().unwrap_or(&now())])?;
        tx.commit()?;
        Ok(())
    }

    pub fn meta(&self) -> Result<Value> {
        let body: String = self.connection()?.query_row(
            "SELECT value FROM wb_meta WHERE key='metadata'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn set_meta(&self, value: Value) -> Result<()> {
        self.connection()?.execute(
            "UPDATE wb_meta SET value=?1 WHERE key='metadata'",
            [serde_json::to_string(&value)?],
        )?;
        Ok(())
    }

    pub fn event(&self, action: &str, title: &str, details: Value) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT event_hash FROM wb_activity ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let previous = previous.unwrap_or_else(|| "0".repeat(64));
        let event = json!({"id":id("activity"),"action":action,"title":title,"details":details,"created_at":now()});
        let body = serde_json::to_string(&event)?;
        let hash = digest(format!("{previous}{body}").as_bytes());
        tx.execute(
            "INSERT INTO wb_activity VALUES(?1,?2,?3,?4)",
            params![text(&event, "id"), body, previous, hash],
        )?;
        let mut metadata: Value = serde_json::from_str(&tx.query_row(
            "SELECT value FROM wb_meta WHERE key='metadata'",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        metadata["updated_at"] = event["created_at"].clone();
        tx.execute(
            "UPDATE wb_meta SET value=?1 WHERE key='metadata'",
            [serde_json::to_string(&metadata)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn activity(&self) -> Result<Vec<Value>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT payload,previous_hash,event_hash FROM wb_activity ORDER BY rowid DESC LIMIT 150")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (body, previous, hash) = r?;
            let mut event: Value = serde_json::from_str(&body)?;
            event["previous_hash"] = json!(previous);
            event["event_hash"] = json!(hash);
            Ok(event)
        })
        .collect()
    }

    pub fn has_evidence(&self, evidence_id: &str) -> Result<bool> {
        if self.get("evidence", evidence_id).is_ok() {
            return Ok(true);
        }
        if let Some(run) = evidence_id.strip_prefix("evidence_") {
            return Ok(self.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1)",
                [run],
                |r| r.get(0),
            )?);
        }
        Ok(false)
    }

    pub fn verify(&self) -> Result<Value> {
        let conn = self.connection()?;
        let mut errors = Vec::new();
        let mut count = 0;
        let mut stmt = conn.prepare("SELECT run_id,result_json,report_sha256 FROM runs")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (run, body, hash) = row?;
            count += 1;
            if digest(body.as_bytes()) != hash {
                errors.push(json!({"code":"REPORT_HASH_MISMATCH","run_id":run}));
            }
            let report: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => {
                    errors.push(json!({"code":"INVALID_REPORT","run_id":run}));
                    continue;
                }
            };
            for source in report["sources"].as_array().into_iter().flatten() {
                if source["kind"] == "local_file" && source["raw_copied"] == false {
                    continue;
                }
                if let Some(hash) = source["sha256"].as_str() {
                    let linked: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM run_artifacts WHERE run_id=?1 AND sha256=?2)",
                        params![run, hash],
                        |r| r.get(0),
                    )?;
                    if !linked {
                        errors.push(
                            json!({"code":"MISSING_SOURCE_ARTIFACT","run_id":run,"sha256":hash}),
                        );
                    }
                }
            }
        }
        let mut stmt = conn.prepare("SELECT sha256,size_bytes,relative_path FROM artifacts")?;
        let mut artifacts = 0;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (hash, size, path) = row?;
            artifacts += 1;
            let verified = || -> Result<bool> {
                ensure!(
                    valid_hash(&hash) && path == format!("blobs/{hash}"),
                    "Invalid artifact path"
                );
                let blob = self.path.join("blobs").join(&hash);
                real(&blob, false)?;
                ensure!(
                    fs::metadata(&blob)?.len() <= 32 * 1024 * 1024,
                    "Artifact exceeds limit"
                );
                let bytes = fs::read(blob)?;
                Ok(bytes.len() as i64 == size && digest(&bytes) == hash)
            };
            if !verified().unwrap_or(false) {
                errors.push(json!({"code":"ARTIFACT_HASH_MISMATCH","sha256":hash}));
            }
        }
        let mut previous = "0".repeat(64);
        let mut stmt = conn
            .prepare("SELECT payload,previous_hash,event_hash FROM wb_activity ORDER BY rowid")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (body, recorded, hash) = row?;
            if previous != recorded || digest(format!("{recorded}{body}").as_bytes()) != hash {
                errors.push(json!({"code":"ACTIVITY_CHAIN_MISMATCH"}));
            }
            previous = hash;
        }
        Ok(
            json!({"ok":errors.is_empty(),"run_count":count,"artifact_count":artifacts,"errors":errors,"scope":"Hashes verify retained bytes and audit links, not source truth"}),
        )
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn scoped_path(workspace: &Path, supplied: &str, must_exist: bool) -> Result<PathBuf> {
    ensure!(!supplied.is_empty(), "Supply a workspace-local path");
    let workspace = workspace.canonicalize()?;
    let raw = Path::new(supplied);
    let path = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace.join(raw)
    };
    ensure!(
        path.starts_with(&workspace),
        "Path is outside configured workspace"
    );
    let mut current = workspace.clone();
    for part in path.strip_prefix(&workspace)?.components() {
        if let std::path::Component::Normal(name) = part {
            current.push(name);
        } else {
            bail!("Relative parent components are not permitted");
        }
        if let Ok(meta) = fs::symlink_metadata(&current) {
            ensure!(
                !meta.file_type().is_symlink(),
                "Workspace paths cannot traverse symlinks"
            );
        }
    }
    if must_exist {
        ensure!(path.exists(), "Workspace path does not exist");
    }
    Ok(path)
}
