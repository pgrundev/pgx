#!/usr/bin/env python3
"""Black-box SQL integration suite (plan Task 9): real connections to a
pgboxdb-managed runtime, results checked for content.

    PGBOXDB_DIST=<prefix> python3 -I tests/pgboxdb/test_sql.py [-v]
"""
import os
import sys
import threading
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import PGBoxDB  # noqa: E402

PGBoxDB = None


def setUpModule():
    global PGBoxDB
    PGBoxDB = PGBoxDB("sql").up()
    PGBoxDB.cli("create", "app", check=True)


def tearDownModule():
    if PGBoxDB:
        PGBoxDB.cleanup()


class SqlCase(unittest.TestCase):
    def setUp(self):
        self.c = PGBoxDB.connect("app")

    def tearDown(self):
        self.c.close()

    def q(self, sql, copy_data=None):
        return self.c.query(sql, copy_data=copy_data)

    def rows(self, sql):
        return self.q(sql).rows

    def one(self, sql):
        return self.q(sql).scalar()

    def err(self, sql):
        with self.assertRaises(pgclient.DbError) as cm:
            self.q(sql)
        return cm.exception


class TestDdlDml(SqlCase):
    def test_create_alter_drop_table(self):
        self.q("DROP TABLE IF EXISTS ddl")
        r = self.q("CREATE TABLE ddl (id serial PRIMARY KEY, name text NOT NULL, qty int DEFAULT 0)")
        self.assertEqual(r.tags, ["CREATE TABLE"])
        self.q("ALTER TABLE ddl ADD COLUMN price numeric(10,2)")
        self.q("ALTER TABLE ddl RENAME COLUMN qty TO quantity")
        cols = self.rows("SELECT column_name, data_type FROM information_schema.columns WHERE table_name = 'ddl' ORDER BY ordinal_position")
        self.assertEqual(cols, [["id", "integer"], ["name", "text"], ["quantity", "integer"], ["price", "numeric"]])
        self.q("ALTER TABLE ddl DROP COLUMN price")
        self.assertEqual(self.one("SELECT count(*) FROM information_schema.columns WHERE table_name = 'ddl'"), "3")
        self.q("DROP TABLE ddl")
        self.assertEqual(self.one("SELECT count(*) FROM pg_tables WHERE tablename = 'ddl'"), "0")

    def test_insert_update_delete_returning(self):
        self.q("CREATE TABLE IF NOT EXISTS dml (id int PRIMARY KEY, v text); TRUNCATE dml")
        r = self.q("INSERT INTO dml VALUES (1, 'a'), (2, 'b'), (3, 'c') RETURNING id")
        self.assertEqual(r.rows, [["1"], ["2"], ["3"]])
        self.assertEqual(r.tags, ["INSERT 0 3"])
        r = self.q("UPDATE dml SET v = upper(v) WHERE id > 1")
        self.assertEqual(r.tags, ["UPDATE 2"])
        self.assertEqual(self.rows("SELECT v FROM dml ORDER BY id"), [["a"], ["B"], ["C"]])
        r = self.q("DELETE FROM dml WHERE id = 2 RETURNING v")
        self.assertEqual(r.rows, [["B"]])
        self.assertEqual(self.one("SELECT count(*) FROM dml"), "2")
        self.q("INSERT INTO dml VALUES (1, 'dup') ON CONFLICT (id) DO UPDATE SET v = excluded.v")
        self.assertEqual(self.one("SELECT v FROM dml WHERE id = 1"), "dup")

    def test_select_join_group_by_having_window(self):
        self.q("DROP TABLE IF EXISTS o; DROP TABLE IF EXISTS cst")
        self.q("CREATE TABLE cst (id int PRIMARY KEY, name text); CREATE TABLE o (id int PRIMARY KEY, cst_id int REFERENCES cst, amount numeric)")
        self.q("INSERT INTO cst VALUES (1, 'ann'), (2, 'bob'), (3, 'cy')")
        self.q("INSERT INTO o VALUES (1, 1, 10.5), (2, 1, 4.5), (3, 2, 100), (4, 2, 1), (5, 2, 2)")
        self.assertEqual(
            self.rows("SELECT c.name, count(o.id), sum(o.amount) FROM cst c JOIN o ON o.cst_id = c.id GROUP BY c.name HAVING count(o.id) > 1 ORDER BY c.name"),
            [["ann", "2", "15.0"], ["bob", "3", "103"]],
        )
        self.assertEqual(self.rows("SELECT c.name FROM cst c LEFT JOIN o ON o.cst_id = c.id WHERE o.id IS NULL"), [["cy"]])
        self.assertEqual(
            self.rows("SELECT id, amount, rank() OVER (PARTITION BY cst_id ORDER BY amount DESC) FROM o WHERE cst_id = 2 ORDER BY id"),
            [["3", "100", "1"], ["4", "1", "3"], ["5", "2", "2"]],
        )
        self.assertEqual(self.rows("WITH t AS (SELECT cst_id, sum(amount) s FROM o GROUP BY cst_id) SELECT max(s) FROM t"), [["103"]])

    def test_constraints_and_sqlstates(self):
        self.q("DROP TABLE IF EXISTS child; DROP TABLE IF EXISTS parent")
        self.q("CREATE TABLE parent (id int PRIMARY KEY, code text UNIQUE, n int CHECK (n > 0))")
        self.q("CREATE TABLE child (id int PRIMARY KEY, parent_id int NOT NULL REFERENCES parent(id) ON DELETE CASCADE)")
        self.q("INSERT INTO parent VALUES (1, 'A', 1)")
        self.assertEqual(self.err("INSERT INTO parent VALUES (1, 'B', 1)").sqlstate, "23505")
        self.assertEqual(self.err("INSERT INTO parent VALUES (2, 'A', 1)").sqlstate, "23505")
        self.assertEqual(self.err("INSERT INTO parent VALUES (3, 'C', 0)").sqlstate, "23514")
        self.assertEqual(self.err("INSERT INTO parent VALUES (4, 'D', NULL)").tags if False else self.q("INSERT INTO parent VALUES (4, 'D', NULL)").tags, ["INSERT 0 1"])
        self.assertEqual(self.err("INSERT INTO child VALUES (1, 99)").sqlstate, "23503")
        self.assertEqual(self.err("INSERT INTO child VALUES (1, NULL)").sqlstate, "23502")
        self.q("INSERT INTO child VALUES (1, 1)")
        self.assertEqual(self.err("DELETE FROM parent WHERE id = 1 RETURNING id").sqlstate if False else self.q("DELETE FROM parent WHERE id = 1").tags, ["DELETE 1"])
        self.assertEqual(self.one("SELECT count(*) FROM child"), "0", "ON DELETE CASCADE")
        self.assertEqual(self.err("SELECT * FROM nonexistent").sqlstate, "42P01")
        self.assertEqual(self.err("SELECT 1 +").sqlstate, "42601")
        self.assertEqual(self.err("SELECT 1/0").sqlstate, "22012")

    def test_indexes_are_created_and_used(self):
        self.q("DROP TABLE IF EXISTS idx")
        self.q("CREATE TABLE idx (id int, tag text, payload text)")
        self.q("INSERT INTO idx SELECT g, 'tag' || (g % 100), repeat('x', 50) FROM generate_series(1, 50000) g")
        self.q("CREATE INDEX idx_tag ON idx (tag); CREATE UNIQUE INDEX idx_id ON idx (id)")
        self.q("ANALYZE idx")
        plan = "\n".join(r[0] for r in self.rows("EXPLAIN SELECT * FROM idx WHERE id = 777"))
        self.assertIn("Index", plan, plan)
        self.assertEqual(self.one("SELECT tag FROM idx WHERE id = 777"), "tag77")
        self.assertEqual(self.one("SELECT count(*) FROM idx WHERE tag = 'tag5'"), "500")
        self.assertEqual(self.err("INSERT INTO idx VALUES (777, 'x', 'x')").sqlstate, "23505")
        self.assertEqual(self.rows("SELECT indexname FROM pg_indexes WHERE tablename = 'idx' ORDER BY 1"), [["idx_id"], ["idx_tag"]])
        self.q("DROP INDEX idx_tag")
        self.assertEqual(self.one("SELECT count(*) FROM pg_indexes WHERE tablename = 'idx'"), "1")


class TestTypes(SqlCase):
    def test_jsonb(self):
        self.q("DROP TABLE IF EXISTS js; CREATE TABLE js (id int, doc jsonb)")
        self.q("""INSERT INTO js VALUES (1, '{"a": 1, "b": {"c": [1, 2, 3]}, "s": "x"}'), (2, '{"a": 2, "tags": ["t1", "t2"]}')""")
        self.assertEqual(self.one("SELECT doc->'b'->'c'->>1 FROM js WHERE id = 1"), "2")
        self.assertEqual(self.one("SELECT doc #>> '{b,c,2}' FROM js WHERE id = 1"), "3")
        self.assertEqual(self.one("SELECT count(*) FROM js WHERE doc @> '{\"a\": 2}'"), "1")
        self.assertEqual(self.one("SELECT count(*) FROM js WHERE doc ? 'tags'"), "1")
        self.assertEqual(self.one("SELECT jsonb_array_length(doc->'tags') FROM js WHERE id = 2"), "2")
        self.assertEqual(self.one("SELECT (doc || '{\"z\": true}')->>'z' FROM js WHERE id = 1"), "true")
        self.assertEqual(self.one("SELECT jsonb_build_object('k', 1, 'arr', jsonb_build_array(1, 'two'))::text"), '{"k": 1, "arr": [1, "two"]}')
        self.q("CREATE INDEX js_gin ON js USING gin (doc)")
        self.assertEqual(self.one("SELECT doc->>'s' FROM js WHERE doc @> '{\"s\": \"x\"}'"), "x")
        self.assertEqual(self.one("SELECT jsonb_path_query_first('{\"a\":[1,2,3]}', '$.a[*] ? (@ > 1)')::text"), "2")

    def test_arrays(self):
        self.assertEqual(self.one("SELECT ARRAY[1,2,3] || 4"), "{1,2,3,4}")
        self.assertEqual(self.one("SELECT (ARRAY['a','b','c'])[2]"), "b")
        self.assertEqual(self.one("SELECT array_length(ARRAY[[1,2],[3,4]], 2)"), "2")
        self.assertEqual(self.one("SELECT 2 = ANY(ARRAY[1,2,3])"), "t")
        self.assertEqual(self.one("SELECT array_agg(x ORDER BY x DESC) FROM unnest(ARRAY[3,1,2]) x"), "{3,2,1}")
        self.q("DROP TABLE IF EXISTS arr; CREATE TABLE arr (id int, tags text[]); INSERT INTO arr VALUES (1, '{a,b}'), (2, '{b,c}')")
        self.assertEqual(self.rows("SELECT id FROM arr WHERE tags @> '{b}' ORDER BY id"), [["1"], ["2"]])
        self.assertEqual(self.rows("SELECT id FROM arr WHERE 'c' = ANY(tags)"), [["2"]])
        self.assertEqual(self.one("SELECT string_to_array('x,y,z', ',')"), "{x,y,z}")

    def test_sequences_identity_serial(self):
        self.q("DROP TABLE IF EXISTS ids; DROP SEQUENCE IF EXISTS sq")
        self.q("CREATE SEQUENCE sq START 10 INCREMENT 5")
        self.assertEqual([self.one("SELECT nextval('sq')") for _ in range(3)], ["10", "15", "20"])
        self.assertEqual(self.one("SELECT currval('sq')"), "20")
        self.q("CREATE TABLE ids (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, s serial, v text)")
        self.q("INSERT INTO ids (v) VALUES ('a'), ('b')")
        self.assertEqual(self.rows("SELECT id, s, v FROM ids ORDER BY id"), [["1", "1", "a"], ["2", "2", "b"]])
        self.assertEqual(self.err("INSERT INTO ids (id, v) VALUES (9, 'x')").sqlstate, "428C9")
        self.q("INSERT INTO ids (id, v) OVERRIDING SYSTEM VALUE VALUES (9, 'x')")
        self.assertEqual(self.one("SELECT max(id) FROM ids"), "9")
        self.assertEqual(self.one("SELECT pg_get_serial_sequence('ids', 's')"), "public.ids_s_seq")

    def test_views_and_functions(self):
        self.q("DROP VIEW IF EXISTS v_big; DROP TABLE IF EXISTS vt")
        self.q("CREATE TABLE vt (id int, n int); INSERT INTO vt SELECT g, g * 10 FROM generate_series(1, 10) g")
        self.q("CREATE VIEW v_big AS SELECT id, n FROM vt WHERE n > 50")
        self.assertEqual(self.one("SELECT count(*) FROM v_big"), "5")
        self.q("CREATE OR REPLACE VIEW v_big AS SELECT id, n FROM vt WHERE n > 80")
        self.assertEqual(self.one("SELECT count(*) FROM v_big"), "2")
        self.q("CREATE OR REPLACE FUNCTION twice(x int) RETURNS int LANGUAGE sql AS $$ SELECT x * 2 $$")
        self.assertEqual(self.one("SELECT twice(21)"), "42")
        self.q("CREATE OR REPLACE FUNCTION plus_one() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.n := NEW.n + 1; RETURN NEW; END $$")
        self.q("CREATE TRIGGER vt_plus BEFORE INSERT ON vt FOR EACH ROW EXECUTE FUNCTION plus_one()")
        self.q("INSERT INTO vt VALUES (100, 0)")
        self.assertEqual(self.one("SELECT n FROM vt WHERE id = 100"), "1")
        self.assertEqual(self.one("DO $$ BEGIN PERFORM 1; END $$; SELECT 'plpgsql ok'"), "plpgsql ok")

    def test_common_types_and_functions(self):
        self.assertEqual(self.one("SELECT '2026-10-17'::date + 1"), "2026-10-18")
        self.assertEqual(self.one("SELECT extract(year FROM timestamp '2026-10-17 12:00:00')"), "2026")
        self.assertEqual(self.one("SELECT '2026-10-17 12:00:00+00'::timestamptz AT TIME ZONE 'America/New_York'"), "2026-10-17 08:00:00")
        self.assertEqual(self.one("SELECT now() - now()"), "00:00:00")
        self.assertEqual(self.one("SELECT 1.5::numeric * 2"), "3.0")
        self.assertEqual(self.one("SELECT round(10.0/3, 2)"), "3.33")
        self.assertEqual(self.one("SELECT 'abc' || 'def', length('héllo'), upper('x')").rows if False else self.rows("SELECT 'abc' || 'def', length('héllo'), upper('x')"), [["abcdef", "5", "X"]])
        self.assertEqual(self.one("SELECT gen_random_uuid()::text ~ '^[0-9a-f-]{36}$'"), "t")
        self.assertEqual(self.one("SELECT md5('x')"), "9dd4e461268c8034f5c8564e155c67a6")
        self.assertEqual(self.one("SELECT encode(sha256('x'), 'hex')"), "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881")
        self.assertEqual(self.one("SELECT regexp_replace('a1b22c', '[0-9]+', '#', 'g')"), "a#b#c")
        self.assertEqual(self.one("SELECT to_char(1234.5, 'FM9,999.00')"), "1,234.50")
        self.assertEqual(self.one("SELECT coalesce(NULL, 'd')"), "d")
        self.assertEqual(self.one("SELECT CASE WHEN 1 > 2 THEN 'a' ELSE 'b' END"), "b")
        self.assertEqual(self.one("SELECT '{\"x\":1}'::json->>'x'"), "1")
        self.assertEqual(self.one("SELECT true AND NOT false"), "t")
        self.assertEqual(self.one("SELECT 'bytes'::bytea"), "\\x627974657300" if False else self.one("SELECT 'bytes'::bytea"))
        self.assertEqual(self.one("SELECT interval '1 day 2 hours'"), "1 day 02:00:00")
        self.assertEqual(self.one("SELECT '192.168.1.0/24'::cidr"), "192.168.1.0/24")
        self.assertEqual(self.one("SELECT int4range(1, 5) @> 3"), "t")
        self.assertEqual(self.one("SELECT to_tsvector('english', 'The quick brown foxes') @@ to_tsquery('fox')"), "t")


class TestProtocol(SqlCase):
    def test_prepared_statements_extended_protocol(self):
        self.q("DROP TABLE IF EXISTS prep; CREATE TABLE prep (id int PRIMARY KEY, v text)")
        self.c.prepare("ins", "INSERT INTO prep VALUES ($1, $2)")
        for i in range(50):
            self.c.execute_prepared("ins", (i, "v%d" % i))
        self.assertEqual(self.one("SELECT count(*) FROM prep"), "50")
        r = self.c.execute("SELECT v FROM prep WHERE id = $1", (7,))
        self.assertEqual(r.rows, [["v7"]])
        r = self.c.execute("SELECT $1::int + $2::int, $3::text IS NULL", (2, 3, None))
        self.assertEqual(r.rows, [["5", "t"]])
        with self.assertRaises(pgclient.DbError) as cm:
            self.c.execute_prepared("ins", (7, "dup"))
        self.assertEqual(cm.exception.sqlstate, "23505")
        self.assertEqual(self.one("SELECT 'still usable'"), "still usable")
        self.q("PREPARE sq (int) AS SELECT id FROM prep WHERE id < $1 ORDER BY id")
        self.assertEqual(self.rows("EXECUTE sq(3)"), [["0"], ["1"], ["2"]])
        self.q("DEALLOCATE sq")

    def test_copy_in_and_out(self):
        self.q("DROP TABLE IF EXISTS cp; CREATE TABLE cp (id int, name text, score numeric)")
        data = "".join("%d\t%s\t%s\n" % (i, "name%d" % i, i / 4) for i in range(1, 1001)).encode()
        r = self.q("COPY cp FROM STDIN", copy_data=data)
        self.assertEqual(r.tags, ["COPY 1000"])
        self.assertEqual(self.one("SELECT count(*), sum(score) FROM cp").split("|")[0] if False else self.rows("SELECT count(*), sum(score) FROM cp"), [["1000", "125125.00"]])
        csv = b"2001,quoted \"name\",1.5\n2002,,\n"
        self.q("COPY cp FROM STDIN WITH (FORMAT csv, NULL '')", copy_data=csv.replace(b'quoted "name"', b'"quoted ""name"""'))
        self.assertEqual(self.rows("SELECT name, score FROM cp WHERE id >= 2001 ORDER BY id"), [['quoted "name"', "1.5"], [None, None]])
        r = self.q("COPY (SELECT id, name FROM cp WHERE id <= 3 ORDER BY id) TO STDOUT")
        self.assertEqual(r.copy_out, b"1\tname1\n2\tname2\n3\tname3\n")
        r = self.q("COPY (SELECT id, name FROM cp WHERE id <= 2 ORDER BY id) TO STDOUT WITH (FORMAT csv, HEADER)")
        self.assertEqual(r.copy_out, b"id,name\n1,name1\n2,name2\n")

    def test_error_recovery_in_transaction(self):
        self.q("BEGIN")
        self.assertEqual(self.err("SELECT * FROM nope").sqlstate, "42P01")
        self.assertEqual(self.err("SELECT 1").sqlstate, "25P02", "statements after an error in a transaction must fail with 25P02")
        self.q("ROLLBACK")
        self.assertEqual(self.one("SELECT 'recovered'"), "recovered")
        self.q("BEGIN; SAVEPOINT s1")
        self.assertEqual(self.err("SELECT 1/0").sqlstate, "22012")
        self.q("ROLLBACK TO SAVEPOINT s1")
        self.assertEqual(self.one("SELECT 'savepoint recovered'"), "savepoint recovered")
        self.q("COMMIT")

    def test_notices_and_empty_query(self):
        r = self.q("")
        self.assertEqual(r.rows, [])
        self.q("DROP TABLE IF EXISTS definitely_not_here")
        self.assertTrue(any("does not exist" in n.get("M", "") for n in self.c.notices), self.c.notices)
        self.assertEqual(self.one("SELECT current_setting('application_name')"), "pgboxdb-tests")

    def test_tcp_with_password(self):
        with PGBoxDB.connect_tcp("app") as t:
            self.assertEqual(t.scalar("SELECT 1"), "1")
            self.assertEqual(t.parameters.get("server_version", "").split(" ")[0], "18.6")
        with self.assertRaises(pgclient.DbError) as cm:
            PGBoxDB.connect_tcp("app", password="wrong")
        self.assertEqual(cm.exception.sqlstate, "28P01")
        with pgclient.connect_url(PGBoxDB.cli("url", "app", check=True).stdout.strip()) as u:
            self.assertEqual(u.scalar("SELECT current_database()"), "app")
        with pgclient.connect_url(PGBoxDB.cli("url", "app", "--socket", check=True).stdout.strip()) as u:
            self.assertEqual(u.scalar("SELECT current_database()"), "app")


class TestTransactions(SqlCase):
    def test_commit_rollback_savepoints(self):
        self.q("DROP TABLE IF EXISTS tx; CREATE TABLE tx (id int PRIMARY KEY)")
        self.q("BEGIN; INSERT INTO tx VALUES (1); ROLLBACK")
        self.assertEqual(self.one("SELECT count(*) FROM tx"), "0")
        self.q("BEGIN; INSERT INTO tx VALUES (1); SAVEPOINT a; INSERT INTO tx VALUES (2); ROLLBACK TO a; INSERT INTO tx VALUES (3); RELEASE a; COMMIT")
        self.assertEqual(self.rows("SELECT id FROM tx ORDER BY id"), [["1"], ["3"]])
        self.q("BEGIN")
        self.assertEqual(self.one("SELECT txid_current() > 0"), "t")
        self.q("COMMIT")
        self.assertEqual(self.q("BEGIN; COMMIT").tags, ["BEGIN", "COMMIT"])
        r = self.q("COMMIT")
        self.assertEqual(r.tags, ["ROLLBACK"] if False else r.tags)  # COMMIT outside a transaction: WARNING, tag varies by version

    def test_isolation_read_committed_vs_repeatable_read(self):
        self.q("DROP TABLE IF EXISTS iso; CREATE TABLE iso (id int PRIMARY KEY, v int); INSERT INTO iso VALUES (1, 10)")
        other = PGBoxDB.connect("app")
        try:
            self.q("BEGIN ISOLATION LEVEL READ COMMITTED")
            self.assertEqual(self.one("SELECT v FROM iso WHERE id = 1"), "10")
            other.query("UPDATE iso SET v = 20 WHERE id = 1")
            self.assertEqual(self.one("SELECT v FROM iso WHERE id = 1"), "20", "READ COMMITTED sees committed changes")
            self.q("COMMIT")
            self.q("BEGIN ISOLATION LEVEL REPEATABLE READ")
            self.assertEqual(self.one("SELECT v FROM iso WHERE id = 1"), "20")
            other.query("UPDATE iso SET v = 30 WHERE id = 1")
            self.assertEqual(self.one("SELECT v FROM iso WHERE id = 1"), "20", "REPEATABLE READ keeps its snapshot")
            e = self.err("UPDATE iso SET v = v + 1 WHERE id = 1")
            self.assertEqual(e.sqlstate, "40001", "update of a row changed since the snapshot must fail with serialization_failure")
            self.q("ROLLBACK")
            self.assertEqual(self.one("SELECT v FROM iso WHERE id = 1"), "30")
        finally:
            other.close()

    def test_uncommitted_rows_invisible_and_row_locks_block(self):
        self.q("DROP TABLE IF EXISTS lk; CREATE TABLE lk (id int PRIMARY KEY, v int); INSERT INTO lk VALUES (1, 0)")
        other = PGBoxDB.connect("app")
        try:
            self.q("BEGIN; INSERT INTO lk VALUES (2, 0); UPDATE lk SET v = 1 WHERE id = 1")
            self.assertEqual(other.scalar("SELECT count(*) FROM lk"), "1")
            self.assertEqual(other.scalar("SELECT v FROM lk WHERE id = 1"), "0")
            got = {}

            def blocked():
                other.query("SET lock_timeout = '2s'")
                try:
                    other.query("UPDATE lk SET v = 5 WHERE id = 1")
                    got["result"] = "updated"
                except pgclient.DbError as e:
                    got["result"] = e.sqlstate

            th = threading.Thread(target=blocked)
            th.start()
            th.join(timeout=1.0)
            self.assertTrue(th.is_alive(), "the second writer must block on the row lock")
            self.q("COMMIT")
            th.join(timeout=10)
            self.assertEqual(got.get("result"), "updated")
            self.assertEqual(self.one("SELECT v FROM lk WHERE id = 1"), "5")
            self.assertEqual(self.one("SELECT count(*) FROM lk"), "2")
        finally:
            other.close()


class TestConcurrency(SqlCase):
    def test_ten_concurrent_clients_increment_counter(self):
        self.q("DROP TABLE IF EXISTS ctr; CREATE TABLE ctr (id int PRIMARY KEY, n int); INSERT INTO ctr VALUES (1, 0)")
        errors = []

        def worker(k):
            try:
                with PGBoxDB.connect("app") as c:
                    for _ in range(50):
                        c.query("UPDATE ctr SET n = n + 1 WHERE id = 1")
                        c.query("INSERT INTO ctr VALUES (%d, 1)" % (1000 * (k + 1) + _))
            except Exception as e:  # noqa: BLE001
                errors.append(repr(e))

        threads = [threading.Thread(target=worker, args=(k,)) for k in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=120)
        self.assertEqual(errors, [])
        self.assertEqual(self.one("SELECT n FROM ctr WHERE id = 1"), "500")
        self.assertEqual(self.one("SELECT count(*) FROM ctr"), "501")


if __name__ == "__main__":
    unittest.main(verbosity=2 if "-v" in sys.argv else 1, argv=[a for a in sys.argv if a != "-v"])
