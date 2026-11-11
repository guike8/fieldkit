"""Run only NVIDIA's deterministic catalog importer inside its own container.

This is deployment tooling for the external service, not a Fieldkit runtime.
It deliberately omits the ingestion service's embedding/PII/semantic stages.
"""
import json
import os

for name in ("DEFAULT_MODELS_ENDPOINT", "REASONING_ENDPOINT", "NON_REASONING_ENDPOINT", "EMBED_ENDPOINT", "RERANK_ENDPOINT"):
    if os.environ.get(name) != "http://127.0.0.1:9/v1":
        raise RuntimeError("catalog-infra requires the closed-endpoint infrastructure configuration")

from auto_ontology.catalog import ingest_catalog
from auto_ontology.connectors.sqlite import SQLiteDatabase
from auto_ontology.dal import schema as s
from auto_ontology.dal.session import store
from auto_ontology.dal.terms import semantic_layer_calculated
from sqlalchemy import func, select

connection = os.environ.get("CONNECTION_STRINGS", "")
if connection != "sqlite:////fieldkit/fieldkit.sqlite":
    raise RuntimeError("catalog-infra requires the normalized Fieldkit projection")

connector = SQLiteDatabase(connection)
before_semantic = semantic_layer_calculated()
source_tables = connector.get_tables()
source_columns = connector.get_columns()
source_fks = connector.get_fks()
tables, columns = ingest_catalog(connector)

source_column = s.catalog_column.alias("fieldkit_fk_source")
stored_fks = store().query_read(
    select(func.count().label("count"))
    .select_from(
        s.column__foreign_key
        .join(source_column, source_column.c.id == s.column__foreign_key.c.source_column_id)
        .join(s.catalog_table, s.catalog_table.c.id == source_column.c.table_id)
        .join(s.catalog_schema, s.catalog_schema.c.id == s.catalog_table.c.schema_id)
        .join(s.catalog_database, s.catalog_database.c.id == s.catalog_schema.c.database_id)
    )
    .where(s.catalog_database.c.name == "fieldkit")
)
stored_fk_count = int(stored_fks[0]["count"])
if len(tables) != len(source_tables) or len(columns) != len(source_columns):
    raise RuntimeError("NVIDIA catalog did not retain the complete projection schema")
if stored_fk_count != len(source_fks):
    raise RuntimeError("NVIDIA catalog did not retain every declared Fieldkit foreign key")
after_semantic = semantic_layer_calculated()
if before_semantic != after_semantic:
    raise RuntimeError("Metadata-only ingestion unexpectedly changed compiled semantic state")

print(json.dumps({
    "database": "fieldkit",
    "tables": len(tables),
    "columns": len(columns),
    "declared_foreign_keys": len(source_fks),
    "stored_foreign_keys": stored_fk_count,
    "table_names": sorted(source_tables["table_name"].tolist()),
    "semantic_calculated": after_semantic,
    "embeddings_requested": False,
    "inference_requested": False,
}))
connector.close()
