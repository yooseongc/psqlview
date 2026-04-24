-- Test fixtures for psqlview integration tests.
-- Loaded by all pg14/15/16/17 containers via /docker-entrypoint-initdb.d.
-- Must remain compatible with PostgreSQL 14+.

CREATE SCHEMA IF NOT EXISTS psqlview_test;

CREATE TABLE IF NOT EXISTS psqlview_test.users (
    id          BIGSERIAL PRIMARY KEY,
    email       TEXT NOT NULL UNIQUE,
    display_name TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    balance     NUMERIC(12, 2) NOT NULL DEFAULT 0,
    metadata    JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS psqlview_test.orders (
    id          BIGSERIAL PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES psqlview_test.users(id),
    amount      NUMERIC(10, 2) NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pending','paid','cancelled')),
    placed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS orders_user_id_idx ON psqlview_test.orders(user_id);

CREATE OR REPLACE VIEW psqlview_test.paid_orders AS
    SELECT * FROM psqlview_test.orders WHERE status = 'paid';

INSERT INTO psqlview_test.users(email, display_name, balance, metadata) VALUES
    ('alice@example.com', 'Alice',   100.00, '{"role":"admin"}'),
    ('bob@example.com',   'Bob',       0.00, '{}'),
    ('carol@example.com', 'Carol',   -12.50, '{"tags":["vip"]}')
ON CONFLICT (email) DO NOTHING;

INSERT INTO psqlview_test.orders(user_id, amount, status) VALUES
    (1, 19.99,  'paid'),
    (1, 5.00,   'pending'),
    (2, 42.00,  'cancelled')
ON CONFLICT DO NOTHING;

-- Per-type fixture for convert_cell round-trip coverage.
-- Two rows only: values (id=1) and NULLs (id=2), so tests can match by id.
CREATE TABLE IF NOT EXISTS psqlview_test.all_types (
    id            INT PRIMARY KEY,
    c_bool        BOOLEAN,
    c_int2        SMALLINT,
    c_int4        INTEGER,
    c_int8        BIGINT,
    c_float4      REAL,
    c_float8      DOUBLE PRECISION,
    c_numeric     NUMERIC(10, 2),
    c_text        TEXT,
    c_date        DATE,
    c_time        TIME,
    c_timestamp   TIMESTAMP,
    c_timestamptz TIMESTAMPTZ,
    c_json        JSON,
    c_jsonb       JSONB,
    c_uuid        UUID,
    c_bytea       BYTEA,
    c_inet        INET
);

INSERT INTO psqlview_test.all_types VALUES
    (1, true, -32768, -2147483648, 9223372036854775807,
     1.5::real, 2.5::double precision, 123.45,
     'hello',
     DATE '2026-04-22', TIME '12:34:56',
     TIMESTAMP '2026-04-22 12:34:56',
     TIMESTAMPTZ '2026-04-22 12:34:56+00',
     '{"k":1}'::json, '{"k":1}'::jsonb,
     '00000000-0000-0000-0000-000000000001'::uuid,
     '\x010203'::bytea,
     '10.0.0.1'::inet),
    (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)
ON CONFLICT (id) DO NOTHING;

-- Bulk fixture for exercising pagination and schema-tree navigation.
-- Keeps the focused psqlview_test fixtures above untouched so existing
-- unit/integration tests keep their assumptions.
CREATE SCHEMA IF NOT EXISTS psqlview_bulk;

-- 50 small tables so the schema tree spills past a screenful.
DO $$
DECLARE
    i INT;
BEGIN
    FOR i IN 1..50 LOOP
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS psqlview_bulk.t%s (
                id BIGSERIAL PRIMARY KEY,
                label TEXT NOT NULL,
                value NUMERIC(14, 2) NOT NULL DEFAULT 0,
                tag TEXT,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                note TEXT
            )',
            lpad(i::text, 3, '0')
        );
    END LOOP;
END
$$;

-- Populate a couple of the tables with enough rows to exercise paging.
INSERT INTO psqlview_bulk.t001 (label, value, tag, note)
SELECT 'row-' || g,
       (g % 1000)::numeric + 0.25,
       CASE (g % 4) WHEN 0 THEN 'alpha'
                    WHEN 1 THEN 'beta'
                    WHEN 2 THEN 'gamma'
                    ELSE 'delta' END,
       'note for row ' || g
FROM generate_series(1, 500) AS s(g)
ON CONFLICT DO NOTHING;

INSERT INTO psqlview_bulk.t002 (label, value, tag, note)
SELECT 'item-' || g,
       g::numeric,
       'tag-' || (g % 10),
       NULL
FROM generate_series(1, 2000) AS s(g)
ON CONFLICT DO NOTHING;

-- A wide(r) table: more columns, 1000 rows. Useful for horizontal
-- column paging and Ctrl+Left/Ctrl+Right.
CREATE TABLE IF NOT EXISTS psqlview_bulk.events (
    id         BIGSERIAL PRIMARY KEY,
    ts         TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind       TEXT NOT NULL,
    actor      TEXT NOT NULL,
    target     TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    level      TEXT NOT NULL DEFAULT 'info',
    message    TEXT NOT NULL,
    payload    JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_ip  INET,
    session    UUID,
    flagged    BOOLEAN NOT NULL DEFAULT false
);

INSERT INTO psqlview_bulk.events
    (ts, kind, actor, target, duration_ms, level, message, payload, source_ip, session, flagged)
SELECT
    now() - (g || ' minutes')::interval,
    CASE (g % 5) WHEN 0 THEN 'login'
                 WHEN 1 THEN 'query'
                 WHEN 2 THEN 'export'
                 WHEN 3 THEN 'schema_load'
                 ELSE 'cancel' END,
    'user_' || (g % 20),
    CASE WHEN g % 3 = 0 THEN NULL ELSE 'target_' || (g % 50) END,
    (g % 2500),
    CASE WHEN g % 17 = 0 THEN 'error'
         WHEN g % 11 = 0 THEN 'warn'
         ELSE 'info' END,
    'synthetic event number ' || g,
    jsonb_build_object('i', g, 'bucket', g % 8),
    ('10.0.0.' || (g % 255))::inet,
    gen_random_uuid(),
    (g % 37 = 0)
FROM generate_series(1, 1000) AS s(g)
ON CONFLICT DO NOTHING;
