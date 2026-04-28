# Postgres-to-Iceberg Benchmark

Benchmarks Iggy's Postgres source + Iceberg sink pipeline against the numbers from
[Supermetal's benchmark](https://thenewstack.io/postgres-to-iceberg-in-13-minutes/)
using TPC-H SF50 (300M+ rows).

## Reference Numbers (from article)

| Tool           | Snapshotting Time |
|----------------|-------------------|
| Supermetal     | 13 minutes        |
| Flink          | 90–116 minutes    |
| Kafka Connect  | 120 minutes       |
| Spark          | 200+ minutes      |

## Infrastructure

Matching the article's setup:

- AWS RDS Aurora Postgres 16 Serverless (48 ACUs max)
- AWS EKS 1.34 with m8i.xlarge nodes (4 CPU, 16 GB RAM)
- Single node: 3 CPU cores, 13 GB RAM
- AWS S3 for Iceberg storage
- REST catalog (Nessie) instead of AWS Glue

## Quick Start

```bash
# 1. Set up infrastructure
./setup-infra.sh

# 2. Load TPC-H SF50 data
export POSTGRES_CONNECTION_STRING="postgresql://user:pass@host:5432/tpch"
./load-tpch-data.sh

# 3. Create Iceberg tables
pip install 'pyiceberg[rest,s3]'
python3 create-iceberg-tables.py \
  --catalog-uri http://localhost:19120/api/v1 \
  --warehouse s3://iggy-benchmark-iceberg/

# 4. Run benchmark
./run-benchmark.sh
```

## Configurations

### Baseline (`connectors/postgres_source.toml` + `connectors/iceberg_sink.toml`)

Sequential polling, batch_size=50000. Expected: 3–6 hours.

### Optimized (`connectors/postgres_source_optimized.toml` + `connectors/iceberg_sink_optimized.toml`)

Uses all new features:

- **Snapshot mode**: Bulk-reads all existing data before switching to CDC
- **Parallel tables**: All 8 TPC-H tables read concurrently
- **Chunk splitting**: Large tables split into 100K-row ranges for parallel reads
- **Zstd compression**: Better Parquet compression than default snappy
- **512MB target files**: Matching Supermetal's file size configuration

Expected: 15–30 minutes (competitive with tuned Flink).

## New Features Added

### PostgreSQL Source Connector

| Feature | Config Key | Description |
|---------|-----------|-------------|
| Table namespace | `table_namespace` | Prefix for table names in records (enables Iceberg dynamic routing) |
| Parallel tables | `parallel_tables` | Process all tables concurrently via tokio tasks |
| Chunk splitting | `chunk_size` | Split large tables into ID-range chunks for parallel reads |
| Snapshot mode | `snapshot_mode = "full"` | Bulk-read all data before switching to CDC streaming |

### Iceberg Sink Connector

| Feature | Config Key | Description |
|---------|-----------|-------------|
| Target file size | `target_file_size_bytes` | Configurable Parquet rolling file size (default 512MB) |
| Compression | `parquet_compression` | Parquet codec: snappy, gzip, lz4, zstd, none |

## Architecture

```
Postgres (TPC-H SF50)
    |
    | [parallel chunked reads]
    v
PostgreSQL Source Connector
    |
    | [JSON serialization via simd_json]
    v
Iggy Broker (stream: tpch_benchmark, topic: tpch_data)
    |
    | [consume + decode]
    v
Iceberg Sink Connector
    |
    | [dynamic routing by table_name]
    | [JSON -> Arrow -> Parquet]
    v
Iceberg Tables (REST catalog + S3)
```

## Known Limitations vs Supermetal

1. **Broker in the middle**: Data flows through Iggy broker (extra hop). Supermetal writes directly.
2. **JSON serialization**: Records are JSON-serialized between source and sink. Supermetal likely uses binary.
3. **No phase-aware sink**: Supermetal switches append/MoR and file size between snapshot and CDC phases.
4. **REST catalog only**: Supermetal supports AWS Glue natively.
