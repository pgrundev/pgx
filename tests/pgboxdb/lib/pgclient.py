"""A PostgreSQL wire-protocol client in the standard library: Unix socket or
TCP, trust / cleartext / md5 / SCRAM-SHA-256 authentication, simple and
extended queries, COPY in/out. Used by the black-box suites so that they run
anywhere Python 3 runs, with no driver to install.
"""
import base64
import hashlib
import hmac
import os
import secrets
import socket
import struct


class DbError(Exception):
    def __init__(self, fields):
        self.fields = fields
        self.severity = fields.get("S", "")
        self.sqlstate = fields.get("C", "")
        self.message = fields.get("M", "")
        self.detail = fields.get("D")
        self.hint = fields.get("H")
        super().__init__("%s: %s" % (self.sqlstate, self.message))


class ConnectionClosed(Exception):
    pass


class Result:
    def __init__(self):
        self.columns = []
        self.rows = []
        self.tags = []
        self.notices = []

    def scalar(self):
        return self.rows[0][0] if self.rows and self.rows[0] else None

    def __repr__(self):
        return "Result(columns=%r, rows=%r, tags=%r)" % (self.columns, self.rows, self.tags)


def _parse_fields(body):
    out = {}
    for field in body.split(b"\0"):
        if field:
            out[chr(field[0])] = field[1:].decode(errors="replace")
    return out


class Conn:
    def __init__(self, sock, user, database, password=None, timeout=None):
        self.sock = sock
        self.sock.settimeout(timeout)
        self.parameters = {}
        self.pid = None
        self.user = user
        self.password = password
        self.notices = []
        try:
            params = b""
            for k, v in (("user", user), ("database", database), ("client_encoding", "UTF8"), ("application_name", "pgboxdb-tests")):
                params += k.encode() + b"\0" + v.encode() + b"\0"
            params += b"\0"
            self.sock.sendall(struct.pack("!ii", 8 + len(params), 196608) + params)
            self._startup()
        except BaseException:
            self.sock.close()
            raise

    # ------------------------------------------------------------ transport
    def _recv_exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.sock.recv(n - len(buf))
            if not chunk:
                raise ConnectionClosed("connection closed by server")
            buf += chunk
        return buf

    def _read_msg(self):
        head = self._recv_exact(5)
        (length,) = struct.unpack("!i", head[1:])
        return head[:1], self._recv_exact(length - 4)

    def _send(self, kind, payload=b""):
        try:
            self.sock.sendall(kind + struct.pack("!i", 4 + len(payload)) + payload)
        except OSError as e:
            # The server closed the connection (e.g. it terminated this backend
            # after a FATAL). Surface it as ConnectionClosed, not a raw OSError.
            raise ConnectionClosed("send failed: %s" % e)

    # ------------------------------------------------------------ startup / auth
    def _startup(self):
        while True:
            kind, body = self._read_msg()
            if kind == b"R":
                (code,) = struct.unpack("!i", body[:4])
                if code == 0:
                    continue
                if code == 3:
                    self._send(b"p", (self.password or "").encode() + b"\0")
                elif code == 5:
                    salt = body[4:8]
                    inner = hashlib.md5((self.password or "").encode() + self.user.encode()).hexdigest()
                    self._send(b"p", b"md5" + hashlib.md5(inner.encode() + salt).hexdigest().encode() + b"\0")
                elif code == 10:
                    mechs = [m.decode() for m in body[4:].split(b"\0") if m]
                    if "SCRAM-SHA-256" not in mechs:
                        raise RuntimeError("server offers %r, SCRAM-SHA-256 needed" % mechs)
                    self._scram()
                else:
                    raise RuntimeError("unsupported authentication request %d" % code)
            elif kind == b"S":
                k, v = body.split(b"\0")[:2]
                self.parameters[k.decode()] = v.decode()
            elif kind == b"K":
                self.pid = struct.unpack("!i", body[:4])[0]
            elif kind == b"N":
                self.notices.append(_parse_fields(body))
            elif kind == b"E":
                raise DbError(_parse_fields(body))
            elif kind == b"Z":
                return
            else:
                raise RuntimeError("unexpected message %r during startup" % kind)

    def _scram(self):
        nonce = base64.b64encode(secrets.token_bytes(18)).decode()
        client_first_bare = "n=,r=" + nonce
        first = b"SCRAM-SHA-256\0" + struct.pack("!i", len(client_first_bare) + 3) + b"n,," + client_first_bare.encode()
        self._send(b"p", first)
        kind, body = self._read_msg()
        if kind == b"E":
            raise DbError(_parse_fields(body))
        assert kind == b"R" and struct.unpack("!i", body[:4])[0] == 11, "expected SASLContinue"
        server_first = body[4:].decode()
        attrs = dict(a.split("=", 1) for a in server_first.split(","))
        assert attrs["r"].startswith(nonce), "server nonce does not extend client nonce"
        salt = base64.b64decode(attrs["s"])
        iterations = int(attrs["i"])
        salted = hashlib.pbkdf2_hmac("sha256", (self.password or "").encode(), salt, iterations)
        client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
        stored_key = hashlib.sha256(client_key).digest()
        without_proof = "c=biws,r=" + attrs["r"]
        auth_message = (client_first_bare + "," + server_first + "," + without_proof).encode()
        client_sig = hmac.new(stored_key, auth_message, hashlib.sha256).digest()
        proof = bytes(a ^ b for a, b in zip(client_key, client_sig))
        final = without_proof + ",p=" + base64.b64encode(proof).decode()
        self._send(b"p", final.encode())
        kind, body = self._read_msg()
        if kind == b"E":
            raise DbError(_parse_fields(body))
        assert kind == b"R" and struct.unpack("!i", body[:4])[0] == 12, "expected SASLFinal"
        server_key = hmac.new(salted, b"Server Key", hashlib.sha256).digest()
        expect = base64.b64encode(hmac.new(server_key, auth_message, hashlib.sha256).digest()).decode()
        got = body[4:].decode()
        assert got == "v=" + expect, "server signature mismatch"

    # ------------------------------------------------------------ queries
    def _collect(self, result, copy_data=None):
        """Reads until ReadyForQuery; raises the first ErrorResponse after draining."""
        error = None
        while True:
            try:
                kind, body = self._read_msg()
            except ConnectionClosed:
                if error:
                    raise DbError(error)
                raise
            if kind == b"T":
                (n,) = struct.unpack("!h", body[:2])
                off, cols = 2, []
                for _ in range(n):
                    end = body.index(b"\0", off)
                    cols.append(body[off:end].decode())
                    off = end + 1 + 18
                result.columns = cols
                result.rows = []
            elif kind == b"D":
                (n,) = struct.unpack("!h", body[:2])
                off, row = 2, []
                for _ in range(n):
                    (clen,) = struct.unpack("!i", body[off:off + 4])
                    off += 4
                    if clen < 0:
                        row.append(None)
                    else:
                        row.append(body[off:off + clen].decode())
                        off += clen
                result.rows.append(row)
            elif kind == b"C":
                result.tags.append(body.rstrip(b"\0").decode())
            elif kind == b"E":
                error = error or _parse_fields(body)
            elif kind == b"N":
                result.notices.append(_parse_fields(body))
                self.notices.append(result.notices[-1])
            elif kind == b"G":  # CopyInResponse
                if copy_data is None:
                    self._send(b"f", b"COPY without data\0")
                else:
                    for i in range(0, len(copy_data), 65536):
                        self._send(b"d", copy_data[i:i + 65536])
                    self._send(b"c")
            elif kind == b"H":  # CopyOutResponse
                result.copy_out = b""
            elif kind == b"d":
                result.copy_out += body
            elif kind in (b"c", b"I", b"S", b"A", b"1", b"2", b"3", b"n", b"s", b"t"):
                pass
            elif kind == b"Z":
                result.txn_status = body.decode()
                if error:
                    raise DbError(error)
                return result
            else:
                raise RuntimeError("unexpected message %r" % kind)

    def query(self, sql, copy_data=None):
        payload = sql.encode() + b"\0"
        self._send(b"Q", payload)
        return self._collect(Result(), copy_data)

    def execute(self, sql, params=()):
        """Extended protocol: Parse / Bind (text params) / Describe / Execute / Sync."""
        self._send(b"P", b"\0" + sql.encode() + b"\0" + struct.pack("!h", 0))
        bind = b"\0\0" + struct.pack("!h", 0) + struct.pack("!h", len(params))
        for p in params:
            if p is None:
                bind += struct.pack("!i", -1)
            else:
                b = str(p).encode()
                bind += struct.pack("!i", len(b)) + b
        bind += struct.pack("!h", 0)
        self._send(b"B", bind)
        self._send(b"D", b"P\0")
        self._send(b"E", b"\0" + struct.pack("!i", 0))
        self._send(b"S")
        return self._collect(Result())

    def prepare(self, name, sql):
        self._send(b"P", name.encode() + b"\0" + sql.encode() + b"\0" + struct.pack("!h", 0))
        self._send(b"S")
        return self._collect(Result())

    def execute_prepared(self, name, params=()):
        bind = b"\0" + name.encode() + b"\0" + struct.pack("!h", 0) + struct.pack("!h", len(params))
        for p in params:
            if p is None:
                bind += struct.pack("!i", -1)
            else:
                b = str(p).encode()
                bind += struct.pack("!i", len(b)) + b
        bind += struct.pack("!h", 0)
        self._send(b"B", bind)
        self._send(b"D", b"P\0")
        self._send(b"E", b"\0" + struct.pack("!i", 0))
        self._send(b"S")
        return self._collect(Result())

    def scalar(self, sql):
        return self.query(sql).scalar()

    def close(self):
        try:
            self._send(b"X")
        except (OSError, ConnectionClosed):
            pass
        try:
            self.sock.close()
        except OSError:
            pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


def connect_unix(sock_dir, port, database="postgres", user="postgres", timeout=60):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(os.path.join(sock_dir, ".s.PGSQL.%d" % port))
    return Conn(s, user, database, timeout=timeout)


def connect_tcp(host, port, database="postgres", user="postgres", password=None, timeout=60):
    s = socket.create_connection((host, port), timeout=timeout)
    return Conn(s, user, database, password=password, timeout=timeout)


def connect_url(url, timeout=60):
    """postgres://user:password@host:port/db or postgres://user@/db?host=<dir>&port=<n>"""
    from urllib.parse import urlparse, parse_qs, unquote
    u = urlparse(url)
    db = unquote(u.path.lstrip("/"))
    q = parse_qs(u.query)
    if "host" in q and q["host"][0].startswith("/"):
        return connect_unix(unquote(q["host"][0]), int(q.get("port", [5432])[0]), db, unquote(u.username or "postgres"), timeout)
    return connect_tcp(u.hostname, u.port or 5432, db, unquote(u.username or "postgres"), unquote(u.password) if u.password else None, timeout)
