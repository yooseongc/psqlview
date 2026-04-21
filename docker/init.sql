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
