# Third-party licenses in the PGBoxDB distribution

PGBoxDB itself (the `pgboxdb` CLI and the pgrust-based engine) is licensed under the
GNU Affero General Public License v3.0 (`LICENSE`). The distribution also
contains the following third-party material.

## PostgreSQL (share/timezonesets, share/tsearch_data, share/extension/plpgsql*, share/pgboxdb/base-cluster)

The timezone abbreviation sets, text-search data files, the plpgsql extension
files and the pristine base cluster are produced by or copied from PostgreSQL
18 and are distributed under the PostgreSQL License:

```
PostgreSQL Database Management System
(also known as Postgres, formerly known as Postgres95)

Portions Copyright (c) 1996-2025, PostgreSQL Global Development Group

Portions Copyright (c) 1994, The Regents of the University of California

Permission to use, copy, modify, and distribute this software and its
documentation for any purpose, without fee, and without a written agreement
is hereby granted, provided that the above copyright notice and this
paragraph and the following two paragraphs appear in all copies.

IN NO EVENT SHALL THE UNIVERSITY OF CALIFORNIA BE LIABLE TO ANY PARTY FOR
DIRECT, INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, INCLUDING
LOST PROFITS, ARISING OUT OF THE USE OF THIS SOFTWARE AND ITS
DOCUMENTATION, EVEN IF THE UNIVERSITY OF CALIFORNIA HAS BEEN ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.

THE UNIVERSITY OF CALIFORNIA SPECIFICALLY DISCLAIMS ANY WARRANTIES,
INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS FOR A PARTICULAR PURPOSE. THE SOFTWARE PROVIDED HEREUNDER IS ON
AN "AS IS" BASIS, AND THE UNIVERSITY OF CALIFORNIA HAS NO OBLIGATIONS TO
PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS.
```

## IANA Time Zone Database (share/timezone)

The compiled zoneinfo files are built by PostgreSQL from the IANA Time Zone
Database (tzdata), which is in the public domain.

## Snowball stemmer stop-word lists (share/tsearch_data/*.stop)

Distributed with PostgreSQL under the BSD license of the Snowball project
(Copyright (c) 2001, Dr Martin Porter; Copyright (c) 2002, Richard Boulton).

## Rust crates (bin/postgres, bin/pgboxdb)

The binaries statically link Rust crates from crates.io under their own
permissive licenses (MIT, Apache-2.0, BSD-3-Clause, Unicode-3.0, Zlib).
`cargo metadata` on the source tree at the recorded `engine_commit` lists
each crate and its license; the source tree is https://github.com/pgrundev/pgboxdb.

## Runtime libraries not included

libxml2 (MIT) and ICU (Unicode License) are loaded from the host at run time
if present and are not part of the distribution.
