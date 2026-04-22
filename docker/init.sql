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
