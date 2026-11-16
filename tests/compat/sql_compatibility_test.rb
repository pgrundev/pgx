# Brief §9 — plain DML and DDL on one connection, compared with PostgreSQL.
require_relative "compat_helper"

class PgboxdbCompat::SqlCompatibilityTest < PgboxdbCompat::Case
  category "SQL_COMPATIBILITY"

  def test_select_returns_rows_in_order
    rows = compare("SELECT with ORDER BY") do |db|
      t = db.table(:items)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, label text, qty int)")
      db.exec("INSERT INTO #{t} VALUES (3, 'c', 30), (1, 'a', 10), (2, 'b', 20)")
      db.rows("SELECT id, label, qty, qty * 2 AS twice FROM #{t} WHERE qty >= 20 ORDER BY id")
    end
    assert_equal [ %w[2 b 20 40], %w[3 c 30 60] ], rows
  end

  def test_indexed_lookup_is_correct_and_explain_runs
    out = compare("indexed lookup rows") do |db|
      t = db.table(:idx)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, k text NOT NULL, v int)")
      db.exec("INSERT INTO #{t} SELECT g, 'k' || g, g * 10 FROM generate_series(1, 2000) g")
      db.exec("CREATE INDEX #{t}_k_idx ON #{t} (k)")
      db.exec("ANALYZE #{t}")
      db.rows("SELECT id, v FROM #{t} WHERE k = 'k1234'")
    end
    assert_equal [ %w[1234 12340] ], out

    # EXPLAIN must run; whether the planner picks the index on a 2000-row table
    # is compared with the control (PostgreSQL chooses an Index Scan here).
    index_used = compare("EXPLAIN uses the index") do |db|
      t = db.table(:idx)
      plan = db.rows("EXPLAIN SELECT id, v FROM #{t} WHERE k = 'k1234'").flatten.join("\n")
      plan.match?(/Index (Only )?Scan|Bitmap Index Scan/) ? "index" : "no-index: #{plan.lines.first.to_s.strip[0, 80]}"
    end
    assert_match(/\A(index|no-index)/, index_used)
  end

  def test_analyze_runs_and_statistics_appear
    out = compare("ANALYZE") do |db|
      t = db.table(:stats)
      db.exec("CREATE TABLE #{t} (id int, grp text)")
      db.exec("INSERT INTO #{t} SELECT g, 'g' || (g % 5) FROM generate_series(1, 500) g")
      db.exec("ANALYZE #{t}")
      db.rows("SELECT attname, n_distinct::text FROM pg_stats WHERE tablename = $1 ORDER BY attname", [ t ])
    end
    assert_equal [ [ "grp", "5" ], [ "id", "-1" ] ], out
  end

  def test_insert_returning_and_count
    out = compare("INSERT RETURNING") do |db|
      t = db.table(:ins)
      db.exec("CREATE TABLE #{t} (id serial PRIMARY KEY, name text NOT NULL, created boolean DEFAULT true)")
      inserted = db.rows("INSERT INTO #{t} (name) VALUES ($1), ($2) RETURNING id, name, created", %w[one two])
      r = db.exec("INSERT INTO #{t} (name) SELECT 'bulk' || g FROM generate_series(1, 50) g")
      [ inserted, r.cmd_tuples, db.value("SELECT count(*) FROM #{t}") ]
    end
    assert_equal [ [ %w[1 one t], %w[2 two t] ], 50, "52" ], out
  end

  def test_update_applies_and_reports_affected_rows
    out = compare("UPDATE") do |db|
      t = db.table(:upd)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, qty int NOT NULL, note text)")
      db.exec("INSERT INTO #{t} VALUES (1, 10, NULL), (2, 20, NULL), (3, 30, 'keep')")
      r = db.exec("UPDATE #{t} SET qty = qty + 1, note = coalesce(note, 'touched') WHERE id <= 2")
      [ r.cmd_tuples, db.rows("SELECT id, qty, note FROM #{t} ORDER BY id") ]
    end
    assert_equal [ 2, [ %w[1 11 touched], %w[2 21 touched], %w[3 30 keep] ] ], out
  end
end
