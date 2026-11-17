require "test_helper"

class UserTest < ActiveSupport::TestCase
  test "email is required and unique" do
    assert_not User.new.valid?
    User.create!(email: "a@example.com")
    assert_raises(ActiveRecord::RecordInvalid) { User.create!(email: "a@example.com") }
  end

  test "the database is the branch PGBoxDB handed this session" do
    db = ActiveRecord::Base.connection.current_database
    assert db.start_with?("agent-") || ENV["PGBOXDB_FIXTURE_ANY_DB"], "expected an agent branch, got #{db}"
  end
end
