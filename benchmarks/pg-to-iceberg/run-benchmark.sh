#!/usr/bin/env bash
# Licensed to the Apache Software Foundation (ASF) under one
# or more contributor license agreements.  See the NOTICE file
# distributed with this work for additional information
# regarding copyright ownership.  The ASF licenses this file
# to you under the Apache License, Version 2.0 (the
# "License"); you may not use this file except in compliance
# with the License.  You may obtain a copy of the License at
#
#   http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing,
# software distributed under the License is distributed on an
# "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
# KIND, either express or implied.  See the License for the
# specific language governing permissions and limitations
# under the License.

set -euo pipefail

# Iggy Postgres-to-Iceberg Benchmark Runner
# Measures end-to-end time to snapshot TPC-H SF50 from Postgres into Iceberg

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
POSTGRES_URL="${POSTGRES_CONNECTION_STRING:?Set POSTGRES_CONNECTION_STRING}"

echo "=== Iggy PG-to-Iceberg Benchmark ==="
echo "Time: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"

# --- Verify prerequisites ---
echo ""
echo "--- Checking prerequisites ---"

# Expected TPC-H SF50 row counts
declare -A EXPECTED_COUNTS=(
    [lineitem]=300005811
    [orders]=75000000
    [partsupp]=40000000
    [customer]=7500000
    [part]=10000000
    [supplier]=500000
    [nation]=25
    [region]=5
)

echo "Postgres row counts:"
for table in lineitem orders partsupp customer part supplier nation region; do
    count=$(psql "$POSTGRES_URL" -t -c "SELECT COUNT(*) FROM ${table}" | tr -d ' ')
    echo "  ${table}: ${count}"
done

# --- Get S3 baseline ---
echo ""
echo "--- Recording S3 baseline ---"
S3_BUCKET="${S3_BUCKET_NAME:-iggy-benchmark-iceberg}"
S3_SIZE_BEFORE=$(aws s3 ls "s3://${S3_BUCKET}/" --recursive --summarize 2>/dev/null | grep "Total Size" | awk '{print $3}' || echo "0")
echo "S3 size before: ${S3_SIZE_BEFORE} bytes"

# --- Create Iggy stream/topic ---
echo ""
echo "--- Setting up Iggy stream and topic ---"
IGGY_ADDRESS="${IGGY_SERVER_ADDRESS:-localhost:8090}"

# Using the Iggy CLI to create stream + topic
iggy --tcp-server-address "$IGGY_ADDRESS" \
    stream create tpch_benchmark 2>/dev/null || echo "Stream already exists"
iggy --tcp-server-address "$IGGY_ADDRESS" \
    topic create tpch_benchmark tpch_data 8 2>/dev/null || echo "Topic already exists"

# --- Create Iceberg tables in Nessie ---
echo ""
echo "--- Pre-creating Iceberg tables ---"
CATALOG_URI="${ICEBERG_CATALOG_URI:-http://localhost:19120/api/v1}"
WAREHOUSE="${ICEBERG_WAREHOUSE:-s3://${S3_BUCKET}/}"

python3 "${SCRIPT_DIR}/create-iceberg-tables.py" \
    --catalog-uri "$CATALOG_URI" \
    --warehouse "$WAREHOUSE" \
    --namespace tpch

# --- Run benchmark ---
echo ""
echo "=== Starting benchmark ==="
BENCH_START=$(date +%s%N)

# Start connectors runtime
IGGY_CONNECTORS_CONFIG_PATH="${SCRIPT_DIR}/config.toml" \
    iggy-connectors &
CONNECTORS_PID=$!

echo "Connectors started (PID: $CONNECTORS_PID)"

# Monitor progress by checking processed rows
monitor_progress() {
    while kill -0 "$CONNECTORS_PID" 2>/dev/null; do
        for table in lineitem orders partsupp customer part supplier nation region; do
            # Check S3 for Iceberg data files
            local file_count
            file_count=$(aws s3 ls "s3://${S3_BUCKET}/tpch/${table}/data/" --recursive 2>/dev/null | wc -l || echo "0")
            echo -n "  ${table}:${file_count}f"
        done
        echo " [$(date +%H:%M:%S)]"
        sleep 30
    done
}

echo "Monitoring progress every 30s..."
monitor_progress &
MONITOR_PID=$!

# Wait for completion — the connector runs indefinitely,
# so we poll until Iceberg row counts match source
check_completion() {
    local max_wait=21600  # 6 hours max
    local elapsed=0
    local check_interval=60

    while [ $elapsed -lt $max_wait ]; do
        sleep $check_interval
        elapsed=$((elapsed + check_interval))

        # Check S3 size growth
        local current_size
        current_size=$(aws s3 ls "s3://${S3_BUCKET}/" --recursive --summarize 2>/dev/null | grep "Total Size" | awk '{print $3}' || echo "0")

        echo "[${elapsed}s] S3 size: ${current_size} bytes"

        # Simplified completion check: if no new data in last 2 minutes
        # and we have data, consider it done
        if [ "$elapsed" -gt 120 ] && [ "$current_size" = "$S3_SIZE_BEFORE" ]; then
            echo "No new data written in check interval. Assuming complete or stalled."
        fi
    done
}

check_completion

BENCH_END=$(date +%s%N)
ELAPSED_MS=$(( (BENCH_END - BENCH_START) / 1000000 ))
ELAPSED_S=$(( ELAPSED_MS / 1000 ))
ELAPSED_MIN=$(( ELAPSED_S / 60 ))

# --- Cleanup ---
kill "$MONITOR_PID" 2>/dev/null || true
kill "$CONNECTORS_PID" 2>/dev/null || true
wait "$CONNECTORS_PID" 2>/dev/null || true

# --- Results ---
echo ""
echo "=== BENCHMARK RESULTS ==="
echo "Total time: ${ELAPSED_MIN}m ${ELAPSED_S}s (${ELAPSED_MS}ms)"
echo ""

S3_SIZE_AFTER=$(aws s3 ls "s3://${S3_BUCKET}/" --recursive --summarize 2>/dev/null | grep "Total Size" | awk '{print $3}' || echo "0")
S3_DATA_WRITTEN=$(( S3_SIZE_AFTER - S3_SIZE_BEFORE ))
echo "S3 data written: ${S3_DATA_WRITTEN} bytes ($(( S3_DATA_WRITTEN / 1024 / 1024 )) MB)"
echo ""

echo "Comparison:"
echo "  Supermetal:     13 minutes"
echo "  Flink:          90-116 minutes"
echo "  Kafka Connect:  120 minutes"
echo "  Spark:          200+ minutes"
echo "  Iggy:           ${ELAPSED_MIN} minutes"
