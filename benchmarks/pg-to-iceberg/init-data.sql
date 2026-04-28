CREATE TABLE IF NOT EXISTS region (
    id SERIAL PRIMARY KEY,
    r_regionkey INTEGER NOT NULL,
    r_name VARCHAR(25) NOT NULL,
    r_comment VARCHAR(152)
);

CREATE TABLE IF NOT EXISTS nation (
    id SERIAL PRIMARY KEY,
    n_nationkey INTEGER NOT NULL,
    n_name VARCHAR(25) NOT NULL,
    n_regionkey INTEGER NOT NULL,
    n_comment VARCHAR(152)
);

CREATE TABLE IF NOT EXISTS supplier (
    id SERIAL PRIMARY KEY,
    s_suppkey INTEGER NOT NULL,
    s_name VARCHAR(25) NOT NULL,
    s_address VARCHAR(40) NOT NULL,
    s_nationkey INTEGER NOT NULL,
    s_phone VARCHAR(15) NOT NULL,
    s_acctbal NUMERIC(15,2) NOT NULL,
    s_comment VARCHAR(101)
);

CREATE TABLE IF NOT EXISTS part (
    id SERIAL PRIMARY KEY,
    p_partkey INTEGER NOT NULL,
    p_name VARCHAR(55) NOT NULL,
    p_mfgr VARCHAR(25) NOT NULL,
    p_brand VARCHAR(10) NOT NULL,
    p_type VARCHAR(25) NOT NULL,
    p_size INTEGER NOT NULL,
    p_container VARCHAR(10) NOT NULL,
    p_retailprice NUMERIC(15,2) NOT NULL,
    p_comment VARCHAR(23)
);

CREATE TABLE IF NOT EXISTS customer (
    id SERIAL PRIMARY KEY,
    c_custkey INTEGER NOT NULL,
    c_name VARCHAR(25) NOT NULL,
    c_address VARCHAR(40) NOT NULL,
    c_nationkey INTEGER NOT NULL,
    c_phone VARCHAR(15) NOT NULL,
    c_acctbal NUMERIC(15,2) NOT NULL,
    c_mktsegment VARCHAR(10) NOT NULL,
    c_comment VARCHAR(117)
);

CREATE TABLE IF NOT EXISTS orders (
    id SERIAL PRIMARY KEY,
    o_orderkey INTEGER NOT NULL,
    o_custkey INTEGER NOT NULL,
    o_orderstatus CHAR(1) NOT NULL,
    o_totalprice NUMERIC(15,2) NOT NULL,
    o_orderdate DATE NOT NULL,
    o_orderpriority VARCHAR(15) NOT NULL,
    o_clerk VARCHAR(15) NOT NULL,
    o_shippriority INTEGER NOT NULL,
    o_comment VARCHAR(79)
);

CREATE TABLE IF NOT EXISTS partsupp (
    id SERIAL PRIMARY KEY,
    ps_partkey INTEGER NOT NULL,
    ps_suppkey INTEGER NOT NULL,
    ps_availqty INTEGER NOT NULL,
    ps_supplycost NUMERIC(15,2) NOT NULL,
    ps_comment VARCHAR(199)
);

CREATE TABLE IF NOT EXISTS lineitem (
    id SERIAL PRIMARY KEY,
    l_orderkey INTEGER NOT NULL,
    l_partkey INTEGER NOT NULL,
    l_suppkey INTEGER NOT NULL,
    l_linenumber INTEGER NOT NULL,
    l_quantity NUMERIC(15,2) NOT NULL,
    l_extendedprice NUMERIC(15,2) NOT NULL,
    l_discount NUMERIC(15,2) NOT NULL,
    l_tax NUMERIC(15,2) NOT NULL,
    l_returnflag CHAR(1) NOT NULL,
    l_linestatus CHAR(1) NOT NULL,
    l_shipdate DATE NOT NULL,
    l_commitdate DATE NOT NULL,
    l_receiptdate DATE NOT NULL,
    l_shipinstruct VARCHAR(25) NOT NULL,
    l_shipmode VARCHAR(10) NOT NULL,
    l_comment VARCHAR(44)
);

INSERT INTO region (r_regionkey, r_name, r_comment)
SELECT g, 'REGION_' || g, 'comment for region ' || g
FROM generate_series(0, 4) g;

INSERT INTO nation (n_nationkey, n_name, n_regionkey, n_comment)
SELECT g, 'NATION_' || g, g % 5, 'comment for nation ' || g
FROM generate_series(0, 24) g;

INSERT INTO supplier (s_suppkey, s_name, s_address, s_nationkey, s_phone, s_acctbal, s_comment)
SELECT g, 'Supplier#' || lpad(g::text, 9, '0'), 'addr_' || g, g % 25,
       lpad((g % 25)::text, 2, '0') || '-' || lpad((g % 1000)::text, 3, '0') || '-' || lpad((g % 10000)::text, 4, '0'),
       (random() * 10000)::numeric(15,2), 'supplier comment ' || g
FROM generate_series(1, 10000) g;

INSERT INTO part (p_partkey, p_name, p_mfgr, p_brand, p_type, p_size, p_container, p_retailprice, p_comment)
SELECT g, 'Part_' || g, 'Mfgr#' || (g % 5 + 1), 'Brand#' || (g % 5 + 1) || (g % 5 + 1),
       'TYPE_' || g % 150, (g % 50) + 1, 'CONT_' || g % 40,
       (900 + g / 10)::numeric(15,2), 'part comment'
FROM generate_series(1, 200000) g;

INSERT INTO customer (c_custkey, c_name, c_address, c_nationkey, c_phone, c_acctbal, c_mktsegment, c_comment)
SELECT g, 'Customer#' || lpad(g::text, 9, '0'), 'address_' || g, g % 25,
       lpad((g % 25)::text, 2, '0') || '-' || lpad((g % 1000)::text, 3, '0') || '-' || lpad((g % 10000)::text, 4, '0'),
       (random() * 10000 - 1000)::numeric(15,2),
       (ARRAY['AUTOMOBILE','BUILDING','FURNITURE','MACHINERY','HOUSEHOLD'])[g % 5 + 1],
       'customer comment ' || g
FROM generate_series(1, 150000) g;

INSERT INTO orders (o_orderkey, o_custkey, o_orderstatus, o_totalprice, o_orderdate, o_orderpriority, o_clerk, o_shippriority, o_comment)
SELECT g, (g % 150000) + 1,
       (ARRAY['O','F','P'])[g % 3 + 1],
       (random() * 500000)::numeric(15,2),
       '1992-01-01'::date + (g % 2500),
       (ARRAY['1-URGENT','2-HIGH','3-MEDIUM','4-NOT SPECIFIED','5-LOW'])[g % 5 + 1],
       'Clerk#' || lpad((g % 1000)::text, 9, '0'),
       0,
       'order comment ' || (g % 1000)
FROM generate_series(1, 1500000) g;

INSERT INTO partsupp (ps_partkey, ps_suppkey, ps_availqty, ps_supplycost, ps_comment)
SELECT (g % 200000) + 1, (g % 10000) + 1, (random() * 9999)::int,
       (random() * 1000)::numeric(15,2), 'partsupp comment ' || (g % 1000)
FROM generate_series(1, 800000) g;

INSERT INTO lineitem (l_orderkey, l_partkey, l_suppkey, l_linenumber, l_quantity, l_extendedprice, l_discount, l_tax, l_returnflag, l_linestatus, l_shipdate, l_commitdate, l_receiptdate, l_shipinstruct, l_shipmode, l_comment)
SELECT (g % 1500000) + 1, (g % 200000) + 1, (g % 10000) + 1, (g % 7) + 1,
       (random() * 50 + 1)::numeric(15,2),
       (random() * 100000)::numeric(15,2),
       (random() * 0.1)::numeric(15,2),
       (random() * 0.08)::numeric(15,2),
       (ARRAY['A','R','N'])[g % 3 + 1],
       (ARRAY['O','F'])[g % 2 + 1],
       '1992-01-01'::date + (g % 2500),
       '1992-01-01'::date + (g % 2500) + 30,
       '1992-01-01'::date + (g % 2500) + 60,
       (ARRAY['DELIVER IN PERSON','COLLECT COD','TAKE BACK RETURN','NONE'])[g % 4 + 1],
       (ARRAY['TRUCK','MAIL','RAIL','SHIP','AIR','REG AIR','FOB'])[g % 7 + 1],
       'lineitem comment ' || (g % 500)
FROM generate_series(1, 6000000) g;

ANALYZE;
