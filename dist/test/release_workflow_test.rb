# Regression tests for .github/workflows/release.yml.
#
#   ruby dist/test/release_workflow_test.rb [path/to/release.yml]
#
# Replays the `draft` job step by step in a temporary workspace, with each
# `uses:` action emulated the way it behaves on a runner:
#
# * actions/checkout: the workspace is cleaned (`git clean -ffdx`, the
#   action's default `clean: true`) and the checked-out files written;
# * actions/download-artifact: one directory per build job's artifact;
# * softprops/action-gh-release: its `files` globs are resolved, and the
#   run fails like the action does with `fail_on_unmatched_files`.
#
# `run:` steps execute for real (bash -eo pipefail, the runner default) with the job's environment. The
# replay must end with exactly the supported assets attached. The v0.1.3
# bug (a checkout after the download wiped artifacts/, giving a draft with
# no assets) fails it, and so does each mutation in MUTATIONS, so the test
# proves it would catch the bug class, not only today's file.
#
# Also checks the build matrix and upload names against what the draft job
# expects, and unit-tests dist/release/version.sh.
#
# Ruby stdlib only (YAML via Psych), available on the GitHub ubuntu runners.
require "yaml"
require "fileutils"
require "tmpdir"
require "digest"
require "open3"

ROOT = File.expand_path("../..", __dir__)
WORKFLOW = ARGV[0] || File.join(ROOT, ".github/workflows/release.yml")
VERSION = "9.8.7"
TAG = "v#{VERSION}"
SUPPORTED = %w[darwin-arm64 linux-amd64 linux-arm64].freeze
BUILT = (SUPPORTED + %w[darwin-amd64]).freeze
EXPECTED = (SUPPORTED.flat_map { |t|
  ["artifacts/pgboxdb-#{t}/pgboxdb-#{VERSION}-#{t}.tar.gz",
   "artifacts/pgboxdb-#{t}/pgboxdb-#{VERSION}-#{t}.tar.gz.sha256"]
} + ["checksums.txt"]).sort.freeze

$failures = 0
$checks = 0
def check(name, ok, detail = nil)
  $checks += 1
  if ok
    puts "ok   #{name}"
  else
    $failures += 1
    puts "FAIL #{name}#{detail ? ": #{detail}" : ""}"
  end
end

class ReplayError < StandardError; end

# The subset of GitHub expressions the draft job uses.
def expand(str, ctx)
  return str unless str.is_a?(String)
  str.gsub(/\$\{\{\s*(.*?)\s*\}\}/) do
    expr = Regexp.last_match(1)
    ctx.fetch(expr) { raise ReplayError, "unknown expression ${{ #{expr} }}" }
  end
end

def sha256_line(path)
  "#{Digest::SHA256.file(path).hexdigest}  #{File.basename(path)}\n"
end

# What download-artifact produces: artifacts/<artifact name>/<files>.
def write_artifacts(dir, targets, corrupt: nil, stray: false)
  if stray
    d = File.join(dir, "pgboxdb-linux-amd64")
    FileUtils.mkdir_p(d)
    File.write(File.join(d, "pgboxdb-0.0.1-linux-amd64.tar.gz"), "stale\n")
  end
  targets.each do |t|
    d = File.join(dir, "pgboxdb-#{t}")
    FileUtils.mkdir_p(d)
    tgz = File.join(d, "pgboxdb-#{VERSION}-#{t}.tar.gz")
    File.binwrite(tgz, "fake tarball for #{t}\n" * 32)
    line = sha256_line(tgz)
    line = line.sub(/\A./) { |c| c == "0" ? "1" : "0" } if corrupt == t
    File.write("#{tgz}.sha256", line)
  end
end

# Replays the job. Returns the sorted list of files the release step would
# attach; raises ReplayError where the real run would fail.
def replay(job, artifacts: BUILT, corrupt: nil, stray: false, release: true)
  Dir.mktmpdir("pgboxdb-release-replay") do |ws|
    ctx = {
      "needs.guard.outputs.version" => VERSION,
      "github.ref_name" => TAG,
    }
    env = ENV.to_h.merge("GITHUB_WORKSPACE" => ws, "GITHUB_REF_NAME" => TAG,
                         "GITHUB_EVENT_NAME" => "push")
    (job["env"] || {}).each { |k, v| env[k] = expand(v.to_s, ctx) }

    (job["steps"] || []).each_with_index do |step, i|
      label = step["name"] || step["uses"] || "step #{i + 1}"
      with = step["with"] || {}
      if (uses = step["uses"])
        case uses
        when %r{\Aactions/checkout@}
          unless with["clean"] == false
            Dir.children(ws).each { |c| FileUtils.rm_rf(File.join(ws, c)) }
          end
          paths = if with["sparse-checkout"]
                    with["sparse-checkout"].split("\n").map(&:strip).reject(&:empty?)
                  else
                    ["dist"]
                  end
          paths.each do |p|
            src = File.join(ROOT, p)
            raise ReplayError, "#{label}: checkout path #{p} does not exist" unless File.exist?(src)
            FileUtils.mkdir_p(File.dirname(File.join(ws, p)))
            FileUtils.cp_r(src, File.join(ws, p))
          end
        when %r{\Aactions/download-artifact@}
          write_artifacts(File.join(ws, expand(with["path"] || ".", ctx)), artifacts, corrupt: corrupt, stray: stray)
        when %r{\Asoftprops/action-gh-release@}
          raise ReplayError, "#{label}: not a draft" unless with["draft"] == true
          attached = []
          expand(with["files"], ctx).split("\n").map(&:strip).reject(&:empty?).each do |pat|
            m = Dir.glob(pat, base: ws).select { |f| File.file?(File.join(ws, f)) }
            raise ReplayError, "#{label}: pattern #{pat} matched no file" if m.empty?
            attached.concat(m)
          end
          raise ReplayError, "#{label}: no fail_on_unmatched_files" unless with["fail_on_unmatched_files"] == true
          raise ReplayError, "#{label}: no tag_name" if expand(with["tag_name"], ctx).to_s.empty?
          body = expand(with["body_path"], ctx)
          raise ReplayError, "#{label}: body_path #{body} missing" unless body && File.file?(File.join(ws, body))
          return attached.uniq.sort
        else
          raise ReplayError, "#{label}: no emulation for #{uses}"
        end
      elsif step["run"]
        senv = env.merge((step["env"] || {}).transform_values { |v| expand(v.to_s, ctx) })
        out, status = Open3.capture2e(senv, "bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", expand(step["run"], ctx), chdir: ws)
        raise ReplayError, "#{label}: exit #{status.exitstatus}: #{out.strip.lines.last(3).join}" unless status.success?
      end
    end
    raise ReplayError, "the job never creates the release" if release
    return :completed
  end
end

def expect_ok(name, job, **kw)
  got = replay(job, **kw)
  check(name, got == EXPECTED, "attached #{got.inspect}")
rescue ReplayError => e
  check(name, false, e.message)
end

def expect_fail(name, job, like, **kw)
  got = replay(job, **kw)
  check(name, false, "the run succeeded, attaching #{got.inspect}")
rescue ReplayError => e
  check(name, e.message.match?(like), e.message)
end

wf = YAML.load_file(WORKFLOW)
jobs = wf.fetch("jobs")
draft = jobs.fetch("draft")

# 1. The draft job, as written.
expect_ok("draft job attaches exactly the supported assets", draft)

# 2. Inputs that must stop the release.
expect_fail("missing target artifact is refused", draft, /missing .*linux-arm64/, artifacts: BUILT - ["linux-arm64"])
expect_fail("corrupt checksum is refused", draft, /checksum mismatch .*linux-amd64/, corrupt: "linux-amd64")
expect_fail("stray tarball of another version is refused", draft, /unexpected release file .*0\.0\.1/, stray: true)
expect_ok("experimental darwin-amd64 artifact is never attached (also when absent)", draft, artifacts: SUPPORTED)

# 3. Mutations of the workflow the replay must catch.
def mutate(job)
  j = Marshal.load(Marshal.dump(job))
  yield j["steps"]
  j
end
checkout_i = draft["steps"].index { |s| s["uses"].to_s.start_with?("actions/checkout@") }
download_i = draft["steps"].index { |s| s["uses"].to_s.start_with?("actions/download-artifact@") }
check("draft job checks out and downloads", checkout_i && download_i)
if checkout_i && download_i
  check("checkout runs before download-artifact", checkout_i < download_i)
  late_checkout = mutate(draft) { |st| st.insert(st.size - 1, st.delete_at(checkout_i)) }
  expect_fail("mutation: checkout after download (the v0.1.3 bug) is caught", late_checkout, /matched no file|missing|No such file|Can.t open/)
end
no_collect = mutate(draft) { |st| st.reject! { |s| s["run"].to_s.include?("collect.sh") } }
expect_fail("mutation: no collect step is caught", no_collect, /matched no file|attached/)

# 3b. The workflow_dispatch dry run of the draft job.
dry = jobs["draft-dry-run"]
check("draft-dry-run job exists", dry)
if dry
  check("draft-dry-run never creates a release", dry["steps"].none? { |st| st["uses"].to_s.include?("action-gh-release") })
  check("draft-dry-run runs only on workflow_dispatch", dry["if"].to_s == "github.event_name == 'workflow_dispatch'", dry["if"].inspect)
  begin
    check("draft-dry-run completes on a full artifact set", replay(dry, release: false) == :completed)
  rescue ReplayError => e
    check("draft-dry-run completes on a full artifact set", false, e.message)
  end
  begin
    replay(dry, release: false, artifacts: BUILT - ["darwin-arm64"])
    check("draft-dry-run fails on a missing target", false, "completed")
  rescue ReplayError => e
    check("draft-dry-run fails on a missing target", e.message.include?("missing"), e.message)
  end
end
mainstep = jobs["guard"]["steps"].find { |st| st["name"].to_s.include?("ancestor of origin/main") }
check("the on-main check applies to tag pushes", mainstep && mainstep["if"] == "github.event_name == 'push'", mainstep&.dig("if").inspect)

# 4. Triggers and conditions.
check("draft job runs only on a tag push",
      draft["if"].to_s.include?("github.event_name == 'push'") && draft["if"].to_s.include?("refs/tags/v"),
      draft["if"].inspect)
check("draft job needs guard and build", Array(draft["needs"]).sort == %w[build guard], draft["needs"].inspect)

# 5. The build job produces what the draft job collects.
build = jobs.fetch("build")
targets = build.dig("strategy", "matrix", "include").map { |m| m["target"] }
check("build matrix covers every supported target", (SUPPORTED - targets).empty?, targets.inspect)
upload = build["steps"].find { |s| s["uses"].to_s.start_with?("actions/upload-artifact@") }
check("artifact name is pgboxdb-<target>", upload && upload.dig("with", "name") == "pgboxdb-${{ matrix.target }}",
      upload&.dig("with", "name").inspect)
vstep = build["steps"].find { |s| s["name"] == "Determine version" }
check("build takes the version from the guard job",
      vstep && vstep["run"].include?("needs.guard.outputs.version") && !vstep["run"].include?("GITHUB_REF_NAME"),
      vstep&.fetch("run").inspect)
check("build needs guard", Array(build["needs"]).include?("guard"))

# 6. dist/release/version.sh.
Dir.mktmpdir("pgboxdb-version") do |root|
  FileUtils.mkdir_p(File.join(root, "crates/bin/pgboxdb"))
  FileUtils.mkdir_p(File.join(root, "dist/npm"))
  File.write(File.join(root, "crates/bin/pgboxdb/Cargo.toml"), "[package]\nname = \"pgboxdb\"\nversion = \"0.2.0\"\n")
  File.write(File.join(root, "dist/npm/package.json"), "{\n  \"name\": \"pgboxdb\",\n  \"version\": \"0.2.0\",\n}\n")
  script = File.join(ROOT, "dist/release/version.sh")
  run = ->(*args) { Open3.capture3({ "ROOT" => root }, "sh", script, *args) }
  [
    [%w[push v0.2.0], true, "0.2.0"],
    [["workflow_dispatch", "main", "0.3.0-rc.1"], true, "0.3.0-rc.1"],
    [["workflow_dispatch", "main", "v0.3.0"], true, "0.3.0"],
    [%w[push v0.2.1], false, /Cargo.toml/],
    [%w[push main], false, /needs a tag/],
    [%w[push v0.2], false, /not a version/],
    [%w[workflow_dispatch main], false, /needs the version input/],
    [["workflow_dispatch", "main", "0.2.0; rm -rf /"], false, /not a version/],
    [%w[pull_request v0.2.0], false, /unsupported event/],
  ].each do |args, ok, want|
    out, err, st = run.(*args)
    name = "version.sh #{args.join(' ')}"
    if ok
      check(name, st.success? && out.strip == want, "exit #{st.exitstatus} out=#{out.strip} err=#{err.strip}")
    else
      check(name, !st.success? && err.match?(want), "exit #{st.exitstatus} err=#{err.strip}")
    end
  end
  File.write(File.join(root, "dist/npm/package.json"), "{\n  \"version\": \"0.1.9\"\n}\n")
  _, err, st = run.("push", "v0.2.0")
  check("version.sh refuses an npm version mismatch", !st.success? && err.include?("package.json"), err.strip)
end

puts "#{$checks - $failures}/#{$checks} checks passed"
exit($failures.zero? ? 0 : 1)
