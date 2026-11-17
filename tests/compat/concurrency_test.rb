# Brief §10 — two or more connections: MVCC visibility, row locks, deadlock
# detection. Every wait runs on a thread with a timeout so a PGBoxDB hang fails
# the test instead of hanging the suite.
require_relative "compat_helper"

class PgboxdbCompat::ConcurrencyTest < PgboxdbCompat::Case
  category "MVCC"

  # 1. A's uncommitted UPDATE is invisible to B; visible once A commits.
  def test_mvcc_visibility_of_uncommitted_and_committed_update
    out = compare("MVCC visibility") do |db|
      t = db.table(:vis)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1)")
      a = db.conn(:a)
      b = db.conn(:b)
      a.exec("BEGIN")
      a.exec("UPDATE #{t} SET v = 2 WHERE id = 1")
      seen_by_a = a.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0)
      seen_by_b = with_step_timeout("B SELECT while A holds an uncommitted UPDATE") do
        b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0)
      end
      a.exec("COMMIT")
      seen_after = b.exec("SELECT v FROM #{t} WHERE id = 1").getvalue(0, 0)
      [ seen_by_a, seen_by_b, seen_after ]
    end
    assert_equal %w[2 1 2], out
  end

  # 2. B's UPDATE of the row A is updating blocks until A commits, then applies
  #    on top of A's value.
  def test_concurrent_update_of_same_row_blocks_then_applies_on_top
    out = compare("concurrent UPDATE") do |db|
      t = db.table(:cu)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1)")
      a = db.conn(:a)
      b = db.conn(:b)
      a.exec("BEGIN")
      a.exec("UPDATE #{t} SET v = v + 10 WHERE id = 1")
      tb = in_thread { b.exec("UPDATE #{t} SET v = v * 2 WHERE id = 1"); monotonic }
      blocked = still_blocked_after?(tb, 1.0)
      commit_at = monotonic
      a.exec("COMMIT")
      b_done_at = join_step(tb, "B UPDATE after A COMMIT")
      [ blocked, b_done_at > commit_at, db.value("SELECT v FROM #{t} WHERE id = 1") ]
    end
    assert_equal [ true, true, "22" ], out, "[B blocked while A held the row, B finished after A's commit, final value (1+10)*2]"
  end

  # 3. SELECT FOR UPDATE: NOWAIT fails fast (55P03), SKIP LOCKED skips the
  #    locked row, a plain FOR UPDATE waits for A's commit.
  def test_select_for_update_nowait_skip_locked_and_blocking
    out = compare("SELECT FOR UPDATE") do |db|
      t = db.table(:fu)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1), (2, 2)")
      a = db.conn(:a)
      b = db.conn(:b)
      a.exec("BEGIN")
      a.exec("SELECT id FROM #{t} WHERE id = 1 FOR UPDATE")
      nowait = with_step_timeout("FOR UPDATE NOWAIT") { db.sqlstate("SELECT id FROM #{t} WHERE id = 1 FOR UPDATE NOWAIT", conn_name: :b) }
      skipped = with_step_timeout("FOR UPDATE SKIP LOCKED") { db.rows("SELECT id FROM #{t} WHERE id = 1 FOR UPDATE SKIP LOCKED", conn_name: :b) }
      not_skipped = with_step_timeout("FOR UPDATE SKIP LOCKED on a free row") { db.rows("SELECT id FROM #{t} WHERE id IN (1, 2) ORDER BY id FOR UPDATE SKIP LOCKED", conn_name: :b) }
      tb = in_thread { rows = b.exec("SELECT id, v FROM #{t} WHERE id = 1 FOR UPDATE").values; [ rows, monotonic ] }
      blocked = still_blocked_after?(tb, 1.0)
      commit_at = monotonic
      a.exec("COMMIT")
      rows, done_at = join_step(tb, "B FOR UPDATE after A COMMIT")
      [ nowait, skipped, not_skipped, blocked, done_at > commit_at, rows ]
    end
    assert_equal [ "55P03", [], [ [ "2" ] ], true, true, [ %w[1 1] ] ], out
  end

  # 4. Deadlock: exactly one session gets 40P01, the other completes, and the
  #    runtime stays healthy for both connections and a fresh third one.
  def test_deadlock_is_detected_and_runtime_stays_healthy
    out = compare("deadlock") do |db|
      t = db.table(:dl)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 0), (2, 0)")
      a = db.conn(:a)
      b = db.conn(:b)
      a.exec("BEGIN")
      a.exec("UPDATE #{t} SET v = v + 1 WHERE id = 1")
      b.exec("BEGIN")
      b.exec("UPDATE #{t} SET v = v + 1 WHERE id = 2")
      ta = in_thread { db.attempt("UPDATE #{t} SET v = v + 1 WHERE id = 2", conn_name: :a) }
      sleep 0.3 # A is now waiting on B's row; B closing the cycle makes the deadlock
      tb = in_thread { db.attempt("UPDATE #{t} SET v = v + 1 WHERE id = 1", conn_name: :b) }
      sa = join_step(ta, "A waiting on row 2")
      sb = join_step(tb, "B waiting on row 1")
      states = [ sa.first, sb.first ]
      victims = states.count("40P01")
      survivor_ok = states.count(nil)
      # Victim rolls back, survivor commits; both must then be usable.
      if sa.first == "40P01"
        a.exec("ROLLBACK")
        b.exec("COMMIT")
      elsif sb.first == "40P01"
        b.exec("COMMIT")
        a.exec("ROLLBACK")
      end
      healthy = [ db.value("SELECT 1", conn_name: :a), db.value("SELECT 1", conn_name: :b), db.value("SELECT 1", conn_name: :fresh) ]
      { victims: victims, completed: survivor_ok, other_errors: states.compact - [ "40P01" ],
        healthy: healthy, rows: db.rows("SELECT id, v FROM #{t} ORDER BY id") }
    end
    assert_equal({ victims: 1, completed: 1, other_errors: [], healthy: %w[1 1 1], rows: [ %w[1 1], %w[2 1] ] }, out)
  end
end
