# Brief §9 — transaction semantics on one connection: savepoints, error
# recovery, full rollback, transactional temp tables.
require_relative "compat_helper"

class PgboxdbCompat::TransactionTest < PgboxdbCompat::Case
  category "TRANSACTION"

  def test_savepoint_and_rollback_to_savepoint_keeps_earlier_rows
    out = compare("ROLLBACK TO SAVEPOINT") do |db|
      t = db.table(:sp)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, phase text)")
      db.exec("BEGIN")
      db.exec("INSERT INTO #{t} VALUES (1, 'before')")
      db.exec("SAVEPOINT sp1")
      db.exec("INSERT INTO #{t} VALUES (2, 'after')")
      inside = db.rows("SELECT id FROM #{t} ORDER BY id")
      db.exec("ROLLBACK TO SAVEPOINT sp1")
      after_rollback = db.rows("SELECT id, phase FROM #{t} ORDER BY id")
      db.exec("RELEASE SAVEPOINT sp1")
      db.exec("COMMIT")
      [ inside, after_rollback, db.rows("SELECT id, phase FROM #{t} ORDER BY id") ]
    end
    assert_equal [ [ [ "1" ], [ "2" ] ], [ %w[1 before] ], [ %w[1 before] ] ], out
  end

  def test_statement_error_then_rollback_to_savepoint_makes_transaction_usable_again
    out = compare("error recovery with savepoint") do |db|
      t = db.table(:rec)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY)")
      db.exec("BEGIN")
      db.exec("INSERT INTO #{t} VALUES (1)")
      db.exec("SAVEPOINT s")
      dup = db.sqlstate("INSERT INTO #{t} VALUES (1)")
      aborted = db.sqlstate("SELECT 1")               # 25P02: in failed transaction block
      db.exec("ROLLBACK TO SAVEPOINT s")
      usable = db.sqlstate("INSERT INTO #{t} VALUES (2)")
      db.exec("COMMIT")
      [ dup, aborted, usable, db.rows("SELECT id FROM #{t} ORDER BY id") ]
    end
    assert_equal [ "23505", "25P02", nil, [ [ "1" ], [ "2" ] ] ], out
  end

  def test_statement_error_without_savepoint_aborts_transaction_until_rollback
    out = compare("aborted transaction without savepoint") do |db|
      t = db.table(:abort)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY)")
      db.exec("BEGIN")
      db.exec("INSERT INTO #{t} VALUES (1)")
      err = db.sqlstate("INSERT INTO #{t} VALUES (1)")
      select_state = db.sqlstate("SELECT 1")
      insert_state = db.sqlstate("INSERT INTO #{t} VALUES (2)")
      savepoint_state = db.sqlstate("SAVEPOINT late")  # too late: also refused
      rollback_state = db.sqlstate("ROLLBACK")
      after = db.sqlstate("SELECT 1")
      [ err, select_state, insert_state, savepoint_state, rollback_state, after, db.rows("SELECT id FROM #{t}") ]
    end
    assert_equal [ "23505", "25P02", "25P02", "25P02", nil, nil, [] ], out
  end

  def test_commit_of_an_aborted_transaction_rolls_back
    out = compare("COMMIT in aborted transaction") do |db|
      t = db.table(:cab)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY)")
      db.exec("BEGIN")
      db.exec("INSERT INTO #{t} VALUES (1)")
      db.sqlstate("SELECT 1/0")
      # PostgreSQL answers COMMIT with the command tag ROLLBACK (no error).
      state, _msg = db.attempt("COMMIT")
      [ state, db.value("SELECT count(*) FROM #{t}") ]
    end
    assert_equal [ nil, "0" ], out
  end

  def test_full_rollback_discards_every_change
    out = compare("ROLLBACK") do |db|
      t = db.table(:rb)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, v int)")
      db.exec("INSERT INTO #{t} VALUES (1, 1)")
      db.exec("BEGIN")
      db.exec("INSERT INTO #{t} VALUES (2, 2)")
      db.exec("UPDATE #{t} SET v = 100 WHERE id = 1")
      db.exec("DELETE FROM #{t} WHERE id = 1")
      inside = db.rows("SELECT id, v FROM #{t} ORDER BY id")
      db.exec("ROLLBACK")
      [ inside, db.rows("SELECT id, v FROM #{t} ORDER BY id") ]
    end
    assert_equal [ [ %w[2 2] ], [ %w[1 1] ] ], out
  end

  def test_transactional_create_temp_table_is_gone_after_rollback
    out = compare("CREATE TEMP TABLE in a rolled-back transaction") do |db|
      name = "#{PgboxdbCompat::TABLE_PREFIX}#{db.tag}_tmp"
      before = db.value("SELECT to_regclass('pg_temp.#{name}') IS NULL")
      db.exec("BEGIN")
      db.exec("CREATE TEMP TABLE #{name} (id int)")
      db.exec("INSERT INTO #{name} VALUES (1)")
      inside = db.value("SELECT to_regclass('pg_temp.#{name}') IS NOT NULL")
      inside_rows = db.value("SELECT count(*) FROM #{name}")
      db.exec("ROLLBACK")
      after = db.value("SELECT to_regclass('pg_temp.#{name}') IS NULL")
      [ before, inside, inside_rows, after ]
    end
    assert_equal %w[t t 1 t], out
  end
end
