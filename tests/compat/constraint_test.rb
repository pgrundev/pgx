# Brief §9 — constraints: the violation must be refused with the same SQLSTATE
# PostgreSQL uses, and the valid case must go through.
require_relative "compat_helper"

class PgboxdbCompat::ConstraintTest < PgboxdbCompat::Case
  category "CONSTRAINT"

  def test_not_null_violation_is_23502
    out = compare("NOT NULL violation") do |db|
      t = db.table(:nn)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, name text NOT NULL)")
      [ db.sqlstate("INSERT INTO #{t} (id, name) VALUES (1, NULL)"),
        db.sqlstate("INSERT INTO #{t} (id, name) VALUES (1, 'ok')"),
        db.value("SELECT count(*) FROM #{t}") ]
    end
    assert_equal [ "23502", nil, "1" ], out
  end

  def test_primary_key_duplicate_is_23505
    out = compare("PRIMARY KEY duplicate") do |db|
      t = db.table(:pk)
      db.exec("CREATE TABLE #{t} (id int PRIMARY KEY, name text)")
      db.exec("INSERT INTO #{t} VALUES (1, 'first')")
      [ db.sqlstate("INSERT INTO #{t} VALUES (1, 'second')"), db.rows("SELECT id, name FROM #{t}") ]
    end
    assert_equal [ "23505", [ %w[1 first] ] ], out
  end

  def test_unique_duplicate_is_23505
    out = compare("UNIQUE duplicate") do |db|
      t = db.table(:uq)
      db.exec("CREATE TABLE #{t} (id serial PRIMARY KEY, email text UNIQUE)")
      db.exec("INSERT INTO #{t} (email) VALUES ('a@example.com')")
      [ db.sqlstate("INSERT INTO #{t} (email) VALUES ('a@example.com')"),
        db.sqlstate("INSERT INTO #{t} (email) VALUES ('b@example.com')"),
        db.sqlstate("INSERT INTO #{t} (email) VALUES (NULL)"), # NULLs are distinct under UNIQUE
        db.value("SELECT count(*) FROM #{t}") ]
    end
    assert_equal [ "23505", nil, nil, "3" ], out
  end

  def test_foreign_key_valid_insert_and_invalid_rejected_with_23503
    out = compare("FOREIGN KEY") do |db|
      parent = db.table(:parent)
      child = db.table(:child)
      db.exec("CREATE TABLE #{parent} (id int PRIMARY KEY)")
      db.exec("CREATE TABLE #{child} (id serial PRIMARY KEY, parent_id int NOT NULL REFERENCES #{parent} (id))")
      db.exec("INSERT INTO #{parent} VALUES (1)")
      [ db.sqlstate("INSERT INTO #{child} (parent_id) VALUES (1)"),
        db.sqlstate("INSERT INTO #{child} (parent_id) VALUES (999)"),
        db.sqlstate("DELETE FROM #{parent} WHERE id = 1"), # referenced row: also 23503
        db.rows("SELECT parent_id FROM #{child}") ]
    end
    assert_equal [ nil, "23503", "23503", [ [ "1" ] ] ], out
  end
end
