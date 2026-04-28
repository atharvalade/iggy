#!/usr/bin/env python3
"""Create Iceberg tables in Nessie catalog for TPC-H benchmark."""
from pyiceberg.catalog import load_catalog
from pyiceberg.schema import Schema
from pyiceberg.types import (
    NestedField, IntegerType, LongType, StringType,
    DecimalType, DateType, DoubleType
)

catalog = load_catalog(
    "nessie",
    **{
        "type": "rest",
        "uri": "http://localhost:19120/iceberg",
        "s3.endpoint": "http://localhost:9000",
        "s3.access-key-id": "minioadmin",
        "s3.secret-access-key": "minioadmin",
        "s3.region": "us-east-1",
        "warehouse": "s3://iceberg-benchmark/warehouse",
    },
)

try:
    catalog.create_namespace("tpch")
except Exception:
    pass

tables = {
    "region": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "r_regionkey", IntegerType()),
        NestedField(3, "r_name", StringType()),
        NestedField(4, "r_comment", StringType()),
    ),
    "nation": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "n_nationkey", IntegerType()),
        NestedField(3, "n_name", StringType()),
        NestedField(4, "n_regionkey", IntegerType()),
        NestedField(5, "n_comment", StringType()),
    ),
    "supplier": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "s_suppkey", IntegerType()),
        NestedField(3, "s_name", StringType()),
        NestedField(4, "s_address", StringType()),
        NestedField(5, "s_nationkey", IntegerType()),
        NestedField(6, "s_phone", StringType()),
        NestedField(7, "s_acctbal", DoubleType()),
        NestedField(8, "s_comment", StringType()),
    ),
    "part": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "p_partkey", IntegerType()),
        NestedField(3, "p_name", StringType()),
        NestedField(4, "p_mfgr", StringType()),
        NestedField(5, "p_brand", StringType()),
        NestedField(6, "p_type", StringType()),
        NestedField(7, "p_size", IntegerType()),
        NestedField(8, "p_container", StringType()),
        NestedField(9, "p_retailprice", DoubleType()),
        NestedField(10, "p_comment", StringType()),
    ),
    "customer": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "c_custkey", IntegerType()),
        NestedField(3, "c_name", StringType()),
        NestedField(4, "c_address", StringType()),
        NestedField(5, "c_nationkey", IntegerType()),
        NestedField(6, "c_phone", StringType()),
        NestedField(7, "c_acctbal", DoubleType()),
        NestedField(8, "c_mktsegment", StringType()),
        NestedField(9, "c_comment", StringType()),
    ),
    "orders": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "o_orderkey", IntegerType()),
        NestedField(3, "o_custkey", IntegerType()),
        NestedField(4, "o_orderstatus", StringType()),
        NestedField(5, "o_totalprice", DoubleType()),
        NestedField(6, "o_orderdate", StringType()),
        NestedField(7, "o_orderpriority", StringType()),
        NestedField(8, "o_clerk", StringType()),
        NestedField(9, "o_shippriority", IntegerType()),
        NestedField(10, "o_comment", StringType()),
    ),
    "partsupp": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "ps_partkey", IntegerType()),
        NestedField(3, "ps_suppkey", IntegerType()),
        NestedField(4, "ps_availqty", IntegerType()),
        NestedField(5, "ps_supplycost", DoubleType()),
        NestedField(6, "ps_comment", StringType()),
    ),
    "lineitem": Schema(
        NestedField(1, "id", IntegerType()),
        NestedField(2, "l_orderkey", IntegerType()),
        NestedField(3, "l_partkey", IntegerType()),
        NestedField(4, "l_suppkey", IntegerType()),
        NestedField(5, "l_linenumber", IntegerType()),
        NestedField(6, "l_quantity", DoubleType()),
        NestedField(7, "l_extendedprice", DoubleType()),
        NestedField(8, "l_discount", DoubleType()),
        NestedField(9, "l_tax", DoubleType()),
        NestedField(10, "l_returnflag", StringType()),
        NestedField(11, "l_linestatus", StringType()),
        NestedField(12, "l_shipdate", StringType()),
        NestedField(13, "l_commitdate", StringType()),
        NestedField(14, "l_receiptdate", StringType()),
        NestedField(15, "l_shipinstruct", StringType()),
        NestedField(16, "l_shipmode", StringType()),
        NestedField(17, "l_comment", StringType()),
    ),
}

for name, schema in tables.items():
    identifier = ("tpch", name)
    try:
        catalog.drop_table(identifier)
    except Exception:
        pass
    try:
        catalog.create_table(identifier=identifier, schema=schema)
        print(f"Created table: tpch.{name}")
    except Exception as e:
        print(f"Table tpch.{name}: {e}")

print("\nListing tables:")
for t in catalog.list_tables("tpch"):
    print(f"  {t}")
