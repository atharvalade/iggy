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

# TPC-H SF50 Data Generator + Loader for Postgres
# Generates ~50GB of data across 8 tables, loads via COPY

SCALE_FACTOR="${TPCH_SCALE_FACTOR:-50}"
POSTGRES_URL="${POSTGRES_CONNECTION_STRING:?Set POSTGRES_CONNECTION_STRING}"
WORK_DIR="/tmp/tpch-dbgen"
DATA_DIR="${WORK_DIR}/data"

echo "=== TPC-H SF${SCALE_FACTOR} Data Generation & Loading ==="
echo "Target: ${POSTGRES_URL%%@*}@***"

# --- Build dbgen ---
echo ""
echo "--- Building TPC-H dbgen ---"
if [ ! -f "${WORK_DIR}/dbgen" ]; then
    mkdir -p "$WORK_DIR"
    cd "$WORK_DIR"
    if [ ! -d "tpch-dbgen" ]; then
        git clone https://github.com/electrum/tpch-dbgen.git
    fi
    cd tpch-dbgen
    make -j"$(nproc)"
    cp dbgen dists.dss "$WORK_DIR/"
fi

# --- Generate data ---
echo ""
echo "--- Generating TPC-H SF${SCALE_FACTOR} data ---"
mkdir -p "$DATA_DIR"
cd "$WORK_DIR"
./dbgen -s "$SCALE_FACTOR" -f -b dists.dss
mv *.tbl "$DATA_DIR/" 2>/dev/null || true

echo "Generated files:"
ls -lh "$DATA_DIR/"

# --- Create schema ---
echo ""
echo "--- Creating TPC-H schema in Postgres ---"
psql "$POSTGRES_URL" <<'SQL'
DROP TABLE IF EXISTS lineitem CASCADE;
DROP TABLE IF EXISTS orders CASCADE;
DROP TABLE IF EXISTS partsupp CASCADE;
DROP TABLE IF EXISTS customer CASCADE;
DROP TABLE IF EXISTS part CASCADE;
DROP TABLE IF EXISTS supplier CASCADE;
DROP TABLE IF EXISTS nation CASCADE;
DROP TABLE IF EXISTS region CASCADE;

CREATE TABLE region (
    id SERIAL PRIMARY KEY,
    r_regionkey INTEGER NOT NULL,
    r_name CHAR(25) NOT NULL,
    r_comment VARCHAR(152)
);

CREATE TABLE nation (
    id SERIAL PRIMARY KEY,
    n_nationkey INTEGER NOT NULL,
    n_name CHAR(25) NOT NULL,
    n_regionkey INTEGER NOT NULL,
    n_comment VARCHAR(152)
);

CREATE TABLE supplier (
    id SERIAL PRIMARY KEY,
    s_suppkey INTEGER NOT NULL,
    s_name CHAR(25) NOT NULL,
    s_address VARCHAR(40) NOT NULL,
    s_nationkey INTEGER NOT NULL,
    s_phone CHAR(15) NOT NULL,
    s_acctbal DECIMAL(15,2) NOT NULL,
    s_comment VARCHAR(101)
);

CREATE TABLE part (
    id SERIAL PRIMARY KEY,
    p_partkey INTEGER NOT NULL,
    p_name VARCHAR(55) NOT NULL,
    p_mfgr CHAR(25) NOT NULL,
    p_brand CHAR(10) NOT NULL,
    p_type VARCHAR(25) NOT NULL,
    p_size INTEGER NOT NULL,
    p_container CHAR(10) NOT NULL,
    p_retailprice DECIMAL(15,2) NOT NULL,
    p_comment VARCHAR(23)
);

CREATE TABLE customer (
    id SERIAL PRIMARY KEY,
    c_custkey INTEGER NOT NULL,
    c_name VARCHAR(25) NOT NULL,
    c_address VARCHAR(40) NOT NULL,
    c_nationkey INTEGER NOT NULL,
    c_phone CHAR(15) NOT NULL,
    c_acctbal DECIMAL(15,2) NOT NULL,
    c_mktsegment CHAR(10) NOT NULL,
    c_comment VARCHAR(117)
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    o_orderkey INTEGER NOT NULL,
    o_custkey INTEGER NOT NULL,
    o_orderstatus CHAR(1) NOT NULL,
    o_totalprice DECIMAL(15,2) NOT NULL,
    o_orderdate DATE NOT NULL,
    o_orderpriority CHAR(15) NOT NULL,
    o_clerk CHAR(15) NOT NULL,
    o_shippriority INTEGER NOT NULL,
    o_comment VARCHAR(79)
);

CREATE TABLE partsupp (
    id SERIAL PRIMARY KEY,
    ps_partkey INTEGER NOT NULL,
    ps_suppkey INTEGER NOT NULL,
    ps_availqty INTEGER NOT NULL,
    ps_supplycost DECIMAL(15,2) NOT NULL,
    ps_comment VARCHAR(199)
);

CREATE TABLE lineitem (
    id SERIAL PRIMARY KEY,
    l_orderkey INTEGER NOT NULL,
    l_partkey INTEGER NOT NULL,
    l_suppkey INTEGER NOT NULL,
    l_linenumber INTEGER NOT NULL,
    l_quantity DECIMAL(15,2) NOT NULL,
    l_extendedprice DECIMAL(15,2) NOT NULL,
    l_discount DECIMAL(15,2) NOT NULL,
    l_tax DECIMAL(15,2) NOT NULL,
    l_returnflag CHAR(1) NOT NULL,
    l_linestatus CHAR(1) NOT NULL,
    l_shipdate DATE NOT NULL,
    l_commitdate DATE NOT NULL,
    l_receiptdate DATE NOT NULL,
    l_shipinstruct CHAR(25) NOT NULL,
    l_shipmode CHAR(10) NOT NULL,
    l_comment VARCHAR(44)
);

CREATE INDEX idx_lineitem_id ON lineitem(id);
CREATE INDEX idx_orders_id ON orders(id);
CREATE INDEX idx_partsupp_id ON partsupp(id);
CREATE INDEX idx_customer_id ON customer(id);
CREATE INDEX idx_part_id ON part(id);
CREATE INDEX idx_supplier_id ON supplier(id);
SQL

echo "Schema created."

# --- Load data via COPY ---
echo ""
echo "--- Loading TPC-H data via COPY ---"

load_table() {
    local table=$1
    local file=$2
    local cols=$3
    echo "Loading ${table}..."
    local start_time=$(date +%s)
    # TPC-H dbgen uses '|' delimiter with trailing '|'
    # sed removes the trailing delimiter before COPY
    sed 's/|$//' "$DATA_DIR/${file}" | psql "$POSTGRES_URL" \
        -c "\\COPY ${table}(${cols}) FROM STDIN WITH (FORMAT csv, DELIMITER '|')"
    local end_time=$(date +%s)
    local count=$(psql "$POSTGRES_URL" -t -c "SELECT COUNT(*) FROM ${table}")
    echo "  ${table}: ${count} rows loaded in $((end_time - start_time))s"
}

TOTAL_START=$(date +%s)

load_table "region" "region.tbl" \
    "r_regionkey,r_name,r_comment"

load_table "nation" "nation.tbl" \
    "n_nationkey,n_name,n_regionkey,n_comment"

load_table "supplier" "supplier.tbl" \
    "s_suppkey,s_name,s_address,s_nationkey,s_phone,s_acctbal,s_comment"

load_table "part" "part.tbl" \
    "p_partkey,p_name,p_mfgr,p_brand,p_type,p_size,p_container,p_retailprice,p_comment"

load_table "customer" "customer.tbl" \
    "c_custkey,c_name,c_address,c_nationkey,c_phone,c_acctbal,c_mktsegment,c_comment"

load_table "orders" "orders.tbl" \
    "o_orderkey,o_custkey,o_orderstatus,o_totalprice,o_orderdate,o_orderpriority,o_clerk,o_shippriority,o_comment"

load_table "partsupp" "partsupp.tbl" \
    "ps_partkey,ps_suppkey,ps_availqty,ps_supplycost,ps_comment"

load_table "lineitem" "lineitem.tbl" \
    "l_orderkey,l_partkey,l_suppkey,l_linenumber,l_quantity,l_extendedprice,l_discount,l_tax,l_returnflag,l_linestatus,l_shipdate,l_commitdate,l_receiptdate,l_shipinstruct,l_shipmode,l_comment"

TOTAL_END=$(date +%s)

echo ""
echo "=== Data loading complete in $((TOTAL_END - TOTAL_START))s ==="
echo ""
echo "Row counts:"
psql "$POSTGRES_URL" -c "
SELECT 'region' as table_name, COUNT(*) as rows FROM region
UNION ALL SELECT 'nation', COUNT(*) FROM nation
UNION ALL SELECT 'supplier', COUNT(*) FROM supplier
UNION ALL SELECT 'part', COUNT(*) FROM part
UNION ALL SELECT 'customer', COUNT(*) FROM customer
UNION ALL SELECT 'orders', COUNT(*) FROM orders
UNION ALL SELECT 'partsupp', COUNT(*) FROM partsupp
UNION ALL SELECT 'lineitem', COUNT(*) FROM lineitem
ORDER BY rows DESC;
"

# --- Analyze tables for optimizer ---
echo "--- Running ANALYZE ---"
psql "$POSTGRES_URL" -c "ANALYZE;"
echo "Done."
