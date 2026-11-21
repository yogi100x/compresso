-- Library schema: books, loans, and the rules that guard them.
-- Written for the SQL scanner tests (E17-42); Postgres dialect.

\connect library
\set ON_ERROR_STOP on

BEGIN;

CREATE EXTENSION IF NOT EXISTS "pgcrypto" WITH SCHEMA public;
CREATE SCHEMA IF NOT EXISTS lending;

CREATE TYPE lending.loan_state AS ENUM ('open', 'returned', 'lost');
CREATE DOMAIN isbn AS text CHECK (VALUE ~ '^[0-9-]{10,17}$');

/* Books are shared by every branch. /* Nested comments are legal; */
   a semicolon here; does not end anything. */
CREATE TABLE IF NOT EXISTS public.books (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    title text NOT NULL,
    isbn isbn UNIQUE,
    note text DEFAULT 'it''s; fine'
);

CREATE TABLE lending.loans (
    id bigint GENERATED ALWAYS AS IDENTITY,
    book_id uuid NOT NULL REFERENCES public.books (id),
    state lending.loan_state NOT NULL DEFAULT 'open',
    opened_on date NOT NULL
) PARTITION BY RANGE (opened_on);

CREATE TABLE lending.loans_2026 PARTITION OF lending.loans
    FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');

CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS books_title_key
    ON public.books USING btree (lower(title));

CREATE OR REPLACE FUNCTION lending.touch_loan()
RETURNS trigger
LANGUAGE plpgsql
AS $fn$
BEGIN
    -- A body may hold semicolons, quotes, and other dollar tags.
    EXECUTE $q$ SELECT 'x; y' $q$;
    NEW.opened_on := coalesce(NEW.opened_on, current_date);
    RETURN NEW;
END;
$fn$;

CREATE OR REPLACE FUNCTION lending.open_loans(p_book uuid)
RETURNS SETOF lending.loans
STABLE
AS $$ SELECT * FROM lending.loans WHERE book_id = p_book AND state = 'open' $$
LANGUAGE sql;

CREATE OR REPLACE FUNCTION lending.open_loans(p_book uuid, p_since date)
RETURNS SETOF lending.loans
STABLE
AS $$ SELECT * FROM lending.loans WHERE book_id = p_book AND opened_on >= p_since $$
LANGUAGE sql;

CREATE PROCEDURE lending.close_all()
LANGUAGE sql
BEGIN ATOMIC
    UPDATE lending.loans SET state = 'returned';
    SELECT CASE WHEN true THEN 1 END;
END;

CREATE TRIGGER loans_touch
    BEFORE INSERT ON lending.loans
    FOR EACH ROW EXECUTE FUNCTION lending.touch_loan();

ALTER TABLE public.books ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Readers see every book" ON public.books
    FOR SELECT USING (true);

CREATE POLICY books_write ON public.books
    FOR UPDATE USING (current_user = 'librarian');

CREATE SEQUENCE IF NOT EXISTS lending.ticket_seq START 100;

CREATE MATERIALIZED VIEW lending.open_counts AS
    SELECT book_id, count(*) FROM lending.loans GROUP BY book_id;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'librarian') THEN
        CREATE ROLE librarian;
    END IF;
END
$$;

COPY public.books (id, title, isbn, note) FROM stdin;
00000000-0000-0000-0000-000000000001	A Tale; of Two	978-0-00-000000-1	it's $$ /* not code
\.

COMMENT ON TABLE public.books IS 'Every title the library holds.';

COMMIT;
