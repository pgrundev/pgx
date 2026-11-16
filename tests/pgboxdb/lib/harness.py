"""Runs the `pgboxdb` CLI from an assembled distribution against a temporary
PGBOXDB_HOME, and connects to the runtime. One harness per test; everything is
stopped and deleted on exit, even on failure.

    PGBOXDB_DIST=<prefix> is required (dist/assemble.sh --out ...).
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time

from . import pgclient

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))


def dist_prefix():
    p = os.environ.get("PGBOXDB_DIST")
    if not p:
        sys.exit("PGBOXDB_DIST is not set: point it at an assembled distribution prefix (dist/assemble.sh --out ...)")
    for rel in ("bin/pgboxdb", "bin/postgres", "share/timezone", "share/pgboxdb/base-cluster/PG_VERSION"):
        if not os.path.exists(os.path.join(p, rel)):
            sys.exit("PGBOXDB_DIST=%s lacks %s" % (p, rel))
    return p


def _is_zombie(pid):
    # A process that has exited but not been reaped (common when PID 1 does not
    # reap, e.g. a bare container) still appears in pgrep; it is not running.
    try:
        with open("/proc/%d/stat" % pid) as f:
            stat = f.read()
        return stat[stat.rfind(")") + 1:].strip().startswith("Z")
    except OSError:
        return False


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def scratch_root():
    root = os.environ.get("PGBOXDB_TEST_SCRATCH", "/tmp/pgbt")  # short: socket paths
    os.makedirs(root, exist_ok=True)
    return root


class CliResult:
    def __init__(self, proc):
        self.code = proc.returncode
        self.stdout = proc.stdout
        self.stderr = proc.stderr

    def json(self):
        return json.loads(self.stdout.strip())

    def __repr__(self):
        return "exit %d\nstdout: %s\nstderr: %s" % (self.code, self.stdout, self.stderr)


class PGBoxDB:
    def __init__(self, name="t", durability=None, keep=False):
        self.prefix = dist_prefix()
        self.home = os.path.join(scratch_root(), "%s-%d-%s" % (name, os.getpid(), free_port()))
        os.makedirs(self.home)
        self.port = free_port()
        self.durability = durability
        self.keep = keep or bool(os.environ.get("PGBOXDB_TEST_KEEP"))
        self.env = dict(os.environ, PGBOXDB_HOME=self.home)
        self.env.pop("PGHOST", None)
        self.env.pop("PGPORT", None)

    @property
    def bin(self):
        return os.path.join(self.prefix, "bin", "pgboxdb")

    @property
    def data(self):
        return os.path.join(self.home, "data")

    def cli(self, *args, json_out=False, check=False, timeout=600, env=None):
        argv = [self.bin] + (["--json"] if json_out else []) + list(args)
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, env=dict(self.env, **(env or {})))
        r = CliResult(proc)
        if r.code == 4 or (check and r.code != 0):
            r.diagnostics = self.diagnostics()
        if check and r.code != 0:
            raise AssertionError("pgboxdb %s failed: %r\n--- diagnostics\n%s\n--- log tail\n%s" % (" ".join(args), r, r.diagnostics, self.log_tail()))
        return r

    def diagnostics(self):
        """Who holds the port, what the pid file says, which engines exist: for post-mortems."""
        out = ["time %s" % time.strftime("%H:%M:%S")]
        out.append("lsof -i:%d:\n%s" % (self.port, subprocess.run(["lsof", "-nP", "-i:%d" % self.port], capture_output=True, text=True).stdout))
        out.append("netstat entries for port:\n%s" % subprocess.run("netstat -anv 2>/dev/null | grep '[.:]%d ' || true" % self.port, shell=True, capture_output=True, text=True).stdout.strip())
        try:
            with open(os.path.join(self.data, "postmaster.pid")) as f:
                out.append("postmaster.pid: %r" % f.read())
        except OSError as e:
            out.append("postmaster.pid: %s" % e)
        out.append("engine pids: %s" % self.engine_pids())
        out.append("ps (postgres): %s" % subprocess.run("ps -axo pid,ppid,stat,lstart,command | grep '[p]ostgres -D' || true", shell=True, capture_output=True, text=True).stdout.strip())
        return "\n".join(out)

    def init(self, *extra):
        args = ["init", "--port", str(self.port)] + list(extra)
        if self.durability:
            args += ["--durability", self.durability]
        return self.cli(*args, check=True)

    def start(self):
        return self.cli("start", check=True)

    def stop(self, force=False):
        return self.cli("stop", *(["--force"] if force else []), check=True)

    def up(self):
        self.init()
        self.start()
        return self

    def config(self):
        with open(os.path.join(self.home, "config.json")) as f:
            return json.load(f)

    @property
    def sock_dir(self):
        return self.config().get("run_dir") or os.path.join(self.home, "run")

    def connect(self, database="postgres", timeout=60):
        return pgclient.connect_unix(self.sock_dir, self.port, database, timeout=timeout)

    def connect_tcp(self, database="postgres", password=None, timeout=60):
        return pgclient.connect_tcp("127.0.0.1", self.port, database, "postgres", password or self.config()["password"], timeout=timeout)

    def sql(self, database, statement):
        with self.connect(database) as c:
            return c.query(statement)

    def engine_pid(self):
        try:
            with open(os.path.join(self.data, "postmaster.pid")) as f:
                return int(f.readline().strip())
        except (OSError, ValueError):
            return None

    def engine_pids(self):
        out = subprocess.run(["pgrep", "-f", "postgres .*-D %s" % self.data], capture_output=True, text=True).stdout
        pids = [int(x) for x in out.split()]
        return [p for p in pids if not _is_zombie(p)]

    def kill(self, sig=signal.SIGKILL):
        pid = self.engine_pid()
        if pid:
            os.kill(pid, sig)
        return pid

    def wait_engine_gone(self, timeout=30):
        t0 = time.time()
        while time.time() - t0 < timeout:
            if not self.engine_pids():
                return True
            time.sleep(0.05)
        return False

    def log_tail(self, n=30):
        try:
            with open(os.path.join(self.home, "log", "pgboxdb.log")) as f:
                return "".join(f.readlines()[-n:])
        except OSError:
            return ""

    def base_dirs(self):
        return sorted(os.listdir(os.path.join(self.data, "base")))

    def cleanup(self):
        subprocess.run([self.bin, "stop", "--force"], capture_output=True, env=self.env)
        for pid in self.engine_pids():
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        if not self.keep:
            shutil.rmtree(self.home, ignore_errors=True)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.cleanup()
