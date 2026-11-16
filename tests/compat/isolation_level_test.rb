# Brief §10.5 — isolation levels. An unsupported level is a FAILURE here (the
# SET TRANSACTION error surfaces with its exact message), never a skip.
require_relative "compat_helper"

class PgboxdbCompat::IsolationLevelTest < PgboxdbCompat::Case
  category "TRANSACTION"

  # READ COMMITTED: each statement sees commits made before it started.
  def test_read_committed_sees_commits_between_statements
    out = compare("READ COMMITTED") do |db|
      t = db.table(:rc)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1)")
      b = db.conn(:b)
      b.exec("BEGIN")
      b.exec("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
      level = b.exec("SHOW transaction_isolation").getvalue(0, 0)
      first = b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0)
      db.exec("UPDATE #{t} SET v = 2 WHERE id = 1", conn_name: :a) # autocommit
      second = with_step_timeout("B second SELECT") { b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0) }
      b.exec("COMMIT")
      [ level, first, second ]
    end
    assert_equal [ "read committed", "1", "2" ], out
  end

  # REPEATABLE READ: B keeps its snapshot, and updating a row that another
  # committed transaction changed after the snapshot fails with 40001.
  def test_repeatable_read_snapshot_and_serialization_failure_on_concurrent_update
    out = compare("REPEATABLE READ") do |db|
      t = db.table(:rr)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1), (2, 1)")
      b = db.conn(:b)
      b.exec("BEGIN")
      b.exec("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
      level = b.exec("SHOW transaction_isolation").getvalue(0, 0)
      first = b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0) # takes the snapshot
      db.exec("UPDATE #{t} SET v = 2 WHERE id = 1", conn_name: :a)   # committed after B's snapshot
      db.exec("INSERT INTO #{t} VALUES (3, 1)", conn_name: :a)
      second = with_step_timeout("B SELECT under snapshot") { b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0) }
      count = b.exec("SELECT count(*) FROM #{t}").getvalue(0, 0)
      update_state = with_step_timeout("B UPDATE of a concurrently changed row") { db.sqlstate("UPDATE #{t} SET v = 99 WHERE id = 1", conn_name: :b) }
      b.exec("ROLLBACK")
      after = b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0)
      [ level, first, second, count, update_state, after ]
    end
    assert_equal [ "repeatable read", "1", "1", "2", "40001", "2" ], out
  end

  # SERIALIZABLE: write skew (two "doctors" both go off call after each reading
  # that two are on call). Serial execution keeps at least one on call, so a
  # serializable engine must refuse one of the two with 40001 (or serialize
  # them); the invariant must hold afterwards. The exact step at which 40001
  # surfaces is compared with the control.
  def test_serializable_prevents_write_skew
    out = compare("SERIALIZABLE write skew") do |db|
      t = db.table(:ser)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, on_call boolean NOT NULL)")
      db.exec("INSERT INTO #{t} VALUES (1, true), (2, true)")
      a = db.conn(:a)
      b = db.conn(:b)
      [ a, b ].each do |c|
        c.exec("BEGIN")
        c.exec("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
      end
      levels = [ a, b ].map { |c| c.exec("SHOW transaction_isolation").getvalue(0, 0) }
      reads = [ a, b ].map { |c| c.exec("SELECT count(*) FROM #{t} WHERE on_call").getvalue(0, 0) }
      ua = with_step_timeout("A UPDATE") { db.attempt("UPDATE #{t} SET on_call = false WHERE id = 1", conn_name: :a) }
      ub = with_step_timeout("B UPDATE") { db.attempt("UPDATE #{t} SET on_call = false WHERE id = 2", conn_name: :b) }
      ca = with_step_timeout("A COMMIT") { db.attempt("COMMIT", conn_name: :a) }
      cb = with_step_timeout("B COMMIT") { db.attempt("COMMIT", conn_name: :b) }
      [ a, b ].each { |c| c.exec("ROLLBACK") if c.transaction_status != PG::PQTRANS_IDLE }
      steps = { a_update: ua.first, b_update: ub.first, a_commit: ca.first, b_commit: cb.first }
      (@messages ||= {})[db.label] = [ ua, ub, ca, cb ].map(&:last).compact # error text, for the failure message only
      { levels: levels, reads: reads, steps: steps,
        serialization_failures: steps.values.count("40001"),
        still_on_call: db.value("SELECT count(*) FROM #{t} WHERE on_call").to_i }
    end
    assert_equal [ "serializable", "serializable" ], out[:levels]
    assert_equal %w[2 2], out[:reads]
    assert_equal [], out[:steps].values.compact - [ "40001" ], "unexpected SQLSTATE in write-skew steps: #{out[:steps].inspect} #{@messages.inspect}"
    assert_operator out[:serialization_failures], :>=, 1, "both transactions committed under SERIALIZABLE — write skew: #{out[:steps].inspect}"
    assert_operator out[:still_on_call], :>=, 1, "invariant broken: nobody is on call after both transactions finished"
  end
end
