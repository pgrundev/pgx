# Storage: copy-on-write vs portable

`pgboxdb clone` issues `CREATE DATABASE … STRATEGY = FILE_COPY`, so the engine's
`file_copy_method` decides how blocks are copied.

| Backend | Mechanism | Clone cost | Notes |
|---|---|---|---|
| APFS (macOS) | `clonefile` | ~0 bytes, shared | copy-on-write; verified |
| ZFS (Linux) | block cloning / `copy_file_range` | ~0 bytes, shared | copy-on-write; the production target (see `zfs-cow.md`) |
| btrfs / XFS reflink (Linux) | `copy_file_range` reflink | ~0 bytes, shared | copy-on-write |
| ext4 and other filesystems | full file copy | ≈ source size | correct portable fallback |

`pgboxdb doctor` reports the filesystem and whether it clones blocks. `pgboxdb init`
sets `file_copy_method = clone` when the data directory's filesystem supports
cloning, else `copy`.

**Durability** is an explicit `pgboxdb init --durability` choice:

| Setting | `fsync` / `synchronous_commit` / `full_page_writes` | Survives |
|---|---|---|
| `ephemeral` (default) | off / off / off | a server crash, not an OS crash or power loss |
| `full` | on / on / on | every acknowledged commit (PostgreSQL defaults) |

PGBoxDB is one crash domain: a PANIC (for example a full disk failing a WAL write)
stops the whole runtime; it recovers on the next `pgboxdb start` with committed data
intact. Keep PostgreSQL for data you cannot lose.
