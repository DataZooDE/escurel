# Object-store directory sources need a separate operator allow-list

Symptom: a `parquet_dir` relation under `s3://` was refused after the SQL credential-policy
change, even when DuckDB could read it before. `prepare_source` passed every directory glob to a
local-filesystem check requiring `ESCUREL_SQL_FILE_DIRS` and an absolute path.

Fix: check `s3://` and `gs://` directory URLs against `ESCUREL_SQL_OBJECT_PREFIXES` before the
local path check. Normalize configured prefixes to end in `/`; reject ambiguous authorities,
traversal, encoded paths, queries and fragments. Leave local file policy unchanged.

Recognition: a refusal naming `ESCUREL_SQL_OBJECT_PREFIXES` means the operator has not allowed
that remote directory. A subsequent DuckDB httpfs error instead means the index connection lacks
object-store credentials or endpoint access. DuckLake's S3/GCS secret is installed on that index
connection; lane-store S3 configuration is separate.
