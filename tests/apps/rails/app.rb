# ActiveRecord (Rails 8.1) fixture: migrations, schema, seed, transactions,
# savepoints, rollback, RecordNotUnique/InvalidForeignKey, parallel threads, cleanup.
require "active_record"
require "uri"

url = ENV.fetch("DATABASE_URL")
ActiveRecord::Base.establish_connection(url)
conn = ActiveRecord::Base.connection

class CreateSchema < ActiveRecord::Migration[8.1]
  def change
    create_table :authors do |t|
      t.string :name, null: false
      t.timestamps
    end
    add_index :authors, :name, unique: true
    create_table :books do |t|
      t.references :author, null: false, foreign_key: true
      t.string :title, null: false
      t.jsonb :meta, default: {}
      t.timestamps
    end
    add_index :books, %i[author_id title], unique: true
  end
end

class AddField < ActiveRecord::Migration[8.1]
  def change
    add_column :books, :pages, :integer
    add_index :books, :pages
  end
end

class Author < ActiveRecord::Base; has_many :books; end
class Book < ActiveRecord::Base; belongs_to :author; end

errors = []
begin
  ActiveRecord::Migration.verbose = false
  CreateSchema.migrate(:up)
  AddField.migrate(:up)

  # rollback the last migration (schema change down)
  AddField.migrate(:down)
  raise "rollback failed" if conn.column_exists?(:books, :pages)
  AddField.migrate(:up)

  # seed
  a = Author.create!(name: "Ursula")
  a.books.create!(title: "A Wizard", meta: { year: 1968 }, pages: 205)
  a.books.create!(title: "The Tombs", meta: { year: 1970 }, pages: 180)

  # transaction + rollback
  Author.transaction do
    Author.create!(name: "temp")
    raise ActiveRecord::Rollback
  end
  raise "txn rollback failed" unless Author.count == 1

  # savepoint (nested transaction)
  Author.transaction do
    b = Author.create!(name: "Second")
    Author.transaction(requires_new: true) do
      b.update!(name: "Second-renamed")
      raise ActiveRecord::Rollback
    end
  end
  raise "savepoint kept bad data" if Author.exists?(name: "Second-renamed")
  raise "savepoint lost good data" unless Author.exists?(name: "Second")

  # unique violation from the DB
  begin
    Author.create!(name: "Ursula"); errors << "no RecordNotUnique"
  rescue ActiveRecord::RecordNotUnique
  end

  # FK violation from the DB
  begin
    Book.create!(author_id: 999999, title: "orphan"); errors << "no InvalidForeignKey"
  rescue ActiveRecord::InvalidForeignKey
  end

  # jsonb query
  raise "jsonb query failed" unless Book.where("meta->>'year' = ?", "1968").count == 1

  # parallel threads: each its own connection from the pool
  threads = 8.times.map do |k|
    Thread.new do
      ActiveRecord::Base.with_connection do
        5.times { |i| Author.create!(name: "t#{k}-#{i}") }
      end
    end
  end
  threads.each(&:join)
  expected = 2 + 8 * 5  # Ursula, Second, + 40
  raise "parallel insert count=#{Author.count} expected #{expected}" unless Author.count == expected

  # schema dump (what db:schema:load relies on)
  require "active_record/schema_dumper"
  io = StringIO.new
  ActiveRecord::SchemaDumper.dump(ActiveRecord::Base.connection_pool, io)
  raise "schema dump missing tables" unless io.string.include?("create_table") && io.string.include?("\"books\"")

  # cleanup
  Book.delete_all
  Author.delete_all
  raise "cleanup failed" unless Author.count.zero?

  if errors.empty?
    puts "APP rails: PASS (migrations+rollback, txn, savepoint, RecordNotUnique, InvalidForeignKey, jsonb, 8 parallel threads, schema dump, cleanup)"
  else
    puts "APP rails: FAIL (#{errors.join('; ')})"; exit 1
  end
rescue => e
  puts "APP rails: FAIL (#{e.class}: #{e.message})"
  warn e.backtrace.first(8).join("\n") if ENV["PGBOXDB_RAILS_TRACE"]
  exit 1
end
