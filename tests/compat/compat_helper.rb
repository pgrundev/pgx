# test/pgboxdb_compat/compat_helper.rb — base class for the PGBoxDB compatibility
# suite (brief §9 single-connection and §10 concurrent behaviour).
#
# The suite runs against a LIVE database given by PGBOXDB_COMPAT_URL and, when
# PGBOXDB_COMPAT_CONTROL_URL is also set, runs every behavioural step against
# real PostgreSQL too and asserts that PGBoxDB produced the same rows / the same
# SQLSTATE. Without PGBOXDB_COMPAT_URL every test skips, so `bin/test` stays green.
#
# Deliberately NOT built on test_helper / ActiveSupport::TestCase: the suite
# needs only minitest + pg, touches no fixture and no app database, and is run
# in-process by `pgboxdb:compat_branch` under the production environment.
#
# Isolation: every table a test creates is named by `table(:x)` → pgboxdb_ct_<hex>_x
# (hex is unique per test) and dropped in teardown on a fresh connection, even
# when the test left its own connection in an aborted transaction or blocked
# in another thread. The branch role is not a superuser; nothing here needs it.
require "minitest"
require "pg"
require "securerandom"
require "timeout"

module PgboxdbCompat
  CATEGORIES = %w[ENGINE_BUG SQL_COMPATIBILITY ORM_COMPATIBILITY TRANSACTION MVCC CONSTRAINT
                  PERFORMANCE MEMORY STORAGE ROUTING HOST_AGENT CONTROL_PLANE].freeze
  TABLE_PREFIX = "pgboxdb_ct_"
  STEP_TIMEOUT = 10 # seconds a concurrent step may wait before the test fails instead of hanging
  SKIP_MESSAGE = "PGBOXDB_COMPAT_URL is not set — export a postgres:// URL (see test/pgboxdb_compat/README.md) to run the PGBoxDB compatibility suite".freeze

  def self.url = ENV["PGBOXDB_COMPAT_URL"].to_s.strip.then { |u| u.empty? ? nil : u }
  def self.control_url = ENV["PGBOXDB_COMPAT_CONTROL_URL"].to_s.strip.then { |u| u.empty? ? nil : u }

  # A single PG::Error reduced to what the suite compares: the five-character
  # SQLSTATE (nil when the statement succeeded).
  def self.sqlstate(error)
    error.result&.error_field(PG::PG_DIAG_SQLSTATE) if error.respond_to?(:result)
  end

  # Shared, retried connection opener. The gateway may refuse the VERY FIRST
  # connection to a brand-new branch ("no such branch endpoint") while the
  # route settles; retry that once after 300 ms, never later connects.
  @first_connect_done = {}
  @connect_mutex = Mutex.new
  def self.connect(url)
    PG.connect(url, connect_timeout: 10)
  rescue PG::Error => e
    retry_once = @connect_mutex.synchronize { !@first_connect_done[url] && (@first_connect_done[url] = true) }
    raise unless retry_once
    sleep 0.3
    PG.connect(url, connect_timeout: 10)
  ensure
    @connect_mutex.synchronize { @first_connect_done[url] = true }
  end

  # Per-category pass/fail/skip counts, printed after the run (only when the
  # suite actually ran something, so unrelated `bin/test` runs stay quiet).
  module Registry
    @results = Hash.new { |h, k| h[k] = { pass: 0, fail: 0, skip: 0, failed: [] } }
    @mutex = Mutex.new

    def self.record(category, result)
      @mutex.synchronize do
        bucket = @results[category]
        if result.skipped? then bucket[:skip] += 1
        elsif result.passed? then bucket[:pass] += 1
        else
          bucket[:fail] += 1
          bucket[:failed] << "#{result.klass}##{result.name}"
        end
      end
    end

    def self.summary
      return nil if @results.empty?
      lines = [ "PGBoxDB compatibility suite — results by category (target: #{PgboxdbCompat.url ? 'PGBOXDB_COMPAT_URL' : 'unset'}, control: #{PgboxdbCompat.control_url ? 'PGBOXDB_COMPAT_CONTROL_URL' : 'none'})" ]
      @results.sort.each do |cat, b|
        lines << format("  %-18s pass %3d  fail %3d  skip %3d", cat, b[:pass], b[:fail], b[:skip])
        b[:failed].each { |f| lines << "    FAIL #{f}" }
      end
      lines.join("\n")
    end

    def self.reset! = @results.clear
  end

  Minitest.after_run do
    s = Registry.summary
    $stdout.puts("\n#{s}") if s
  end

  # One engine under test (PGBoxDB or the PostgreSQL control): named connections,
  # the test's unique table names, and the cleanup that runs no matter what.
  class Target
    attr_reader :label, :url, :tag

    def initialize(label, url, tag)
      @label = label
      @url = url
      @tag = tag
      @conns = {}
      @tables = []
      @mutex = Mutex.new
    end

    # Named connection, opened lazily; :a is the default.
    def conn(name = :a)
      @mutex.synchronize { @conns[name] ||= PgboxdbCompat.connect(@url) }
    end

    def table(name)
      full = "#{TABLE_PREFIX}#{@tag}_#{name}"
      @mutex.synchronize { @tables << full unless @tables.include?(full) }
      full
    end

    def exec(sql, params = nil, conn_name: :a)
      c = conn(conn_name)
      params ? c.exec_params(sql, params) : c.exec(sql)
    end

    # Rows as arrays of strings (what both engines put on the wire).
    def rows(sql, params = nil, conn_name: :a) = exec(sql, params, conn_name: conn_name).values
    def value(sql, params = nil, conn_name: :a) = rows(sql, params, conn_name: conn_name).dig(0, 0)

    # Run a statement, return [sqlstate, message]; nil sqlstate when it succeeded.
    def attempt(sql, params = nil, conn_name: :a)
      exec(sql, params, conn_name: conn_name)
      [ nil, nil ]
    rescue PG::Error => e
      [ PgboxdbCompat.sqlstate(e), e.message.lines.first.to_s.strip[0, 200] ]
    end

    def sqlstate(sql, params = nil, conn_name: :a) = attempt(sql, params, conn_name: conn_name).first

    # Interrupt a connection that is still blocked (a hung step) so teardown can
    # proceed, then close it.
    def close_all
      conns = @mutex.synchronize { @conns.values.tap { @conns.clear } }
      conns.each do |c|
        begin
          c.cancel if c.status == PG::CONNECTION_OK
        rescue StandardError
          nil
        end
        begin
          c.close
        rescue StandardError
          nil
        end
      end
    end

    def drop_tables
      tables = @mutex.synchronize { @tables.dup }
      return if tables.empty?
      c = PgboxdbCompat.connect(@url)
      tables.reverse_each do |t|
        c.exec("DROP TABLE IF EXISTS #{c.quote_ident(t)} CASCADE")
      rescue PG::Error
        nil
      end
    ensure
      c&.close
    end
  end

  class Case < Minitest::Test
    # Declared once per test class: `category "TRANSACTION"`.
    def self.category(name = nil)
      if name
        raise ArgumentError, "unknown category #{name.inspect}; one of #{CATEGORIES.join(', ')}" unless CATEGORIES.include?(name)
        @category = name
      end
      @category || (superclass.respond_to?(:category) ? superclass.category : nil)
    end

    def run
      result = super
      Registry.record(self.class.category || "UNCATEGORIZED", result)
      result
    end

    attr_reader :pgboxdb, :control

    def setup
      skip SKIP_MESSAGE unless PgboxdbCompat.url
      @tag = SecureRandom.hex(4)
      @pgboxdb = Target.new("pgboxdb", PgboxdbCompat.url, @tag)
      @control = PgboxdbCompat.control_url ? Target.new("control", PgboxdbCompat.control_url, @tag) : nil
    end

    def teardown
      [ @pgboxdb, @control ].compact.each do |t|
        t.close_all
        t.drop_tables
      end
    end

    def targets = [ @pgboxdb, @control ].compact

    # The comparison primitive. The block receives a Target and returns a plain
    # value (rows, a SQLSTATE, a hash of both…). It runs against PGBoxDB and, when a
    # control is configured, against PostgreSQL; the two values must be equal.
    # Returns the PGBoxDB value so the caller can also assert the known-correct
    # PostgreSQL behaviour directly (which is what runs without a control).
    def compare(label)
      pgboxdb_value = yield(@pgboxdb)
      if @control
        control_value = yield(@control)
        assert_equal control_value, pgboxdb_value,
                     "#{label}: PGBoxDB differs from PostgreSQL\n  postgresql: #{control_value.inspect[0, 400]}\n  pgboxdb:        #{pgboxdb_value.inspect[0, 400]}"
      end
      pgboxdb_value
    end

    # --- concurrency helpers (brief §10) -------------------------------------

    # Run a step on another thread; `join_step` fails the test (instead of
    # hanging the suite) when it does not finish within STEP_TIMEOUT.
    def in_thread(&block)
      Thread.new do
        Thread.current.report_on_exception = false
        block.call
      end
    end

    def join_step(thread, label, timeout: STEP_TIMEOUT)
      value = thread.join(timeout)
      flunk "#{label}: still blocked after #{timeout}s (hang)" if value.nil?
      thread.value
    end

    def monotonic = Process.clock_gettime(Process::CLOCK_MONOTONIC)

    # Wait until a thread is observably blocked in its statement: it is alive
    # and has not finished after `seconds`. Returns true when still blocked.
    def still_blocked_after?(thread, seconds = 1.0)
      thread.join(seconds).nil?
    end

    def with_step_timeout(label, &block)
      Timeout.timeout(STEP_TIMEOUT, &block)
    rescue Timeout::Error
      flunk "#{label}: did not return within #{STEP_TIMEOUT}s (hang)"
    end
  end
end
