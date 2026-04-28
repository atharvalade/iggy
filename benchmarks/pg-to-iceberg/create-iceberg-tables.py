#!/usr/bin/env python3
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

"""Pre-create Iceberg tables in Nessie catalog for TPC-H benchmark.

Requires: pip install pyiceberg[rest,s3]
"""

import argparse
import sys

try:
    from pyiceberg.catalog import load_catalog
    from pyiceberg.schema import Schema
    from pyiceberg.types import (
        BooleanType,
        DateType,
        DecimalType,
        IntegerType,
        LongType,
        NestedField,
        StringType,
    )
except ImportError:
    print("Install pyiceberg: pip install 'pyiceberg[rest,s3]'", file=sys.stderr)
    sys.exit(1)

TPCH_SCHEMAS = {
    "region": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "r_regionkey", IntegerType(), required=True),
        NestedField(3, "r_name", StringType(), required=True),
        NestedField(4, "r_comment", StringType()),
        NestedField(5, "table_name", StringType()),
        NestedField(6, "operation_type", StringType()),
        NestedField(7, "timestamp", StringType()),
    ),
    "nation": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "n_nationkey", IntegerType(), required=True),
        NestedField(3, "n_name", StringType(), required=True),
        NestedField(4, "n_regionkey", IntegerType(), required=True),
        NestedField(5, "n_comment", StringType()),
        NestedField(6, "table_name", StringType()),
        NestedField(7, "operation_type", StringType()),
        NestedField(8, "timestamp", StringType()),
    ),
    "supplier": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "s_suppkey", IntegerType(), required=True),
        NestedField(3, "s_name", StringType(), required=True),
        NestedField(4, "s_address", StringType(), required=True),
        NestedField(5, "s_nationkey", IntegerType(), required=True),
        NestedField(6, "s_phone", StringType(), required=True),
        NestedField(7, "s_acctbal", DecimalType(15, 2), required=True),
        NestedField(8, "s_comment", StringType()),
        NestedField(9, "table_name", StringType()),
        NestedField(10, "operation_type", StringType()),
        NestedField(11, "timestamp", StringType()),
    ),
    "part": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "p_partkey", IntegerType(), required=True),
        NestedField(3, "p_name", StringType(), required=True),
        NestedField(4, "p_mfgr", StringType(), required=True),
        NestedField(5, "p_brand", StringType(), required=True),
        NestedField(6, "p_type", StringType(), required=True),
        NestedField(7, "p_size", IntegerType(), required=True),
        NestedField(8, "p_container", StringType(), required=True),
        NestedField(9, "p_retailprice", DecimalType(15, 2), required=True),
        NestedField(10, "p_comment", StringType()),
        NestedField(11, "table_name", StringType()),
        NestedField(12, "operation_type", StringType()),
        NestedField(13, "timestamp", StringType()),
    ),
    "customer": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "c_custkey", IntegerType(), required=True),
        NestedField(3, "c_name", StringType(), required=True),
        NestedField(4, "c_address", StringType(), required=True),
        NestedField(5, "c_nationkey", IntegerType(), required=True),
        NestedField(6, "c_phone", StringType(), required=True),
        NestedField(7, "c_acctbal", DecimalType(15, 2), required=True),
        NestedField(8, "c_mktsegment", StringType(), required=True),
        NestedField(9, "c_comment", StringType()),
        NestedField(10, "table_name", StringType()),
        NestedField(11, "operation_type", StringType()),
        NestedField(12, "timestamp", StringType()),
    ),
    "orders": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "o_orderkey", IntegerType(), required=True),
        NestedField(3, "o_custkey", IntegerType(), required=True),
        NestedField(4, "o_orderstatus", StringType(), required=True),
        NestedField(5, "o_totalprice", DecimalType(15, 2), required=True),
        NestedField(6, "o_orderdate", StringType(), required=True),
        NestedField(7, "o_orderpriority", StringType(), required=True),
        NestedField(8, "o_clerk", StringType(), required=True),
        NestedField(9, "o_shippriority", IntegerType(), required=True),
        NestedField(10, "o_comment", StringType()),
        NestedField(11, "table_name", StringType()),
        NestedField(12, "operation_type", StringType()),
        NestedField(13, "timestamp", StringType()),
    ),
    "partsupp": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "ps_partkey", IntegerType(), required=True),
        NestedField(3, "ps_suppkey", IntegerType(), required=True),
        NestedField(4, "ps_availqty", IntegerType(), required=True),
        NestedField(5, "ps_supplycost", DecimalType(15, 2), required=True),
        NestedField(6, "ps_comment", StringType()),
        NestedField(7, "table_name", StringType()),
        NestedField(8, "operation_type", StringType()),
        NestedField(9, "timestamp", StringType()),
    ),
    "lineitem": Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "l_orderkey", IntegerType(), required=True),
        NestedField(3, "l_partkey", IntegerType(), required=True),
        NestedField(4, "l_suppkey", IntegerType(), required=True),
        NestedField(5, "l_linenumber", IntegerType(), required=True),
        NestedField(6, "l_quantity", DecimalType(15, 2), required=True),
        NestedField(7, "l_extendedprice", DecimalType(15, 2), required=True),
        NestedField(8, "l_discount", DecimalType(15, 2), required=True),
        NestedField(9, "l_tax", DecimalType(15, 2), required=True),
        NestedField(10, "l_returnflag", StringType(), required=True),
        NestedField(11, "l_linestatus", StringType(), required=True),
        NestedField(12, "l_shipdate", StringType(), required=True),
        NestedField(13, "l_commitdate", StringType(), required=True),
        NestedField(14, "l_receiptdate", StringType(), required=True),
        NestedField(15, "l_shipinstruct", StringType(), required=True),
        NestedField(16, "l_shipmode", StringType(), required=True),
        NestedField(17, "l_comment", StringType()),
        NestedField(18, "table_name", StringType()),
        NestedField(19, "operation_type", StringType()),
        NestedField(20, "timestamp", StringType()),
    ),
}


def main():
    parser = argparse.ArgumentParser(description="Create Iceberg tables for TPC-H benchmark")
    parser.add_argument("--catalog-uri", required=True, help="REST catalog URI")
    parser.add_argument("--warehouse", required=True, help="Warehouse location (s3://...)")
    parser.add_argument("--namespace", default="tpch", help="Iceberg namespace")
    args = parser.parse_args()

    catalog = load_catalog(
        "nessie",
        **{
            "type": "rest",
            "uri": args.catalog_uri,
            "warehouse": args.warehouse,
        },
    )

    try:
        catalog.create_namespace(args.namespace)
        print(f"Created namespace: {args.namespace}")
    except Exception:
        print(f"Namespace '{args.namespace}' already exists")

    for table_name, schema in TPCH_SCHEMAS.items():
        full_name = f"{args.namespace}.{table_name}"
        try:
            catalog.create_table(full_name, schema=schema)
            print(f"Created table: {full_name}")
        except Exception as e:
            print(f"Table '{full_name}' already exists or error: {e}")


if __name__ == "__main__":
    main()
