//! The table of contents of a `pg_dump` custom-format archive, as printed by
//! `pg_restore --list`, and the compatibility decision taken from it before
//! anything is restored into PGBoxDB (docs/pgboxdb/compatibility-gate.md).
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    /// The object kind as pg_restore prints it: `TABLE`, `TABLE DATA`, `FK CONSTRAINT`, …
    pub desc: String,
    /// `None` when pg_restore prints `-`.
    pub schema: Option<String>,
    /// The object's tag (name or signature); may contain spaces.
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toc {
    pub dbname: Option<String>,
    /// "18.6" (the server pg_dump read from).
    pub source_version: Option<String>,
    pub source_major: Option<u32>,
    pub dump_version: Option<String>,
    pub entries: Vec<Entry>,
}

/// Object kinds pg_restore can print, longest first so a multi-word kind
/// matches before its prefix (`TABLE DATA` before `TABLE`).
const KNOWN_DESCS: &[&str] = &[
    "PUBLICATION TABLES IN SCHEMA",
    "MATERIALIZED VIEW DATA",
    "FOREIGN DATA WRAPPER",
    "PROCEDURAL LANGUAGE",
    "TEXT SEARCH CONFIGURATION",
    "TEXT SEARCH DICTIONARY",
    "TEXT SEARCH TEMPLATE",
    "TEXT SEARCH PARSER",
    "DATABASE PROPERTIES",
    "MATERIALIZED VIEW",
    "PUBLICATION TABLE",
    "CHECK CONSTRAINT",
    "SEQUENCE OWNED BY",
    "OPERATOR FAMILY",
    "OPERATOR CLASS",
    "SECURITY LABEL",
    "STATISTICS DATA",
    "ACCESS METHOD",
    "BLOB METADATA",
    "EVENT TRIGGER",
    "FOREIGN TABLE",
    "FK CONSTRAINT",
    "LARGE OBJECT",
    "SUBSCRIPTION",
    "INDEX ATTACH",
    "SEQUENCE SET",
    "TABLE ATTACH",
    "USER MAPPING",
    "ROW SECURITY",
    "PUBLICATION",
    "DEFAULT ACL",
    "STDSTRINGS",
    "SEARCHPATH",
    "CONSTRAINT",
    "CONVERSION",
    "STATISTICS",
    "SHELL TYPE",
    "TABLE DATA",
    "COLLATION",
    "AGGREGATE",
    "EXTENSION",
    "PROCEDURE",
    "TRANSFORM",
    "FUNCTION",
    "DATABASE",
    "ENCODING",
    "OPERATOR",
    "SEQUENCE",
    "TRIGGER",
    "COMMENT",
    "DEFAULT",
    "SERVER",
    "POLICY",
    "SCHEMA",
    "DOMAIN",
    "BLOBS",
    "INDEX",
    "TABLE",
    "VIEW",
    "TYPE",
    "RULE",
    "CAST",
    "BLOB",
    "ACL",
];

/// Parse `pg_restore -l` output. Header lines start with `;`; entries look
/// like `224; 1259 16545 TABLE public posts app` (owner last, empty for
/// objects that have none, in which case the line ends with a space).
pub fn parse(text: &str) -> Toc {
    let mut toc = Toc::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches(['\r']);
        if let Some(h) = line.strip_prefix(';') {
            let h = h.trim();
            if let Some(v) = h.strip_prefix("dbname:") {
                toc.dbname = Some(v.trim().to_string());
            } else if let Some(v) = h.strip_prefix("Dumped from database version:") {
                let v = v.trim();
                toc.source_version = Some(version_number(v));
                toc.source_major = v
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok();
            } else if let Some(v) = h.strip_prefix("Dumped by pg_dump version:") {
                toc.dump_version = Some(version_number(v.trim()));
            }
            continue;
        }
        if let Some(e) = parse_entry(line) {
            toc.entries.push(e);
        }
    }
    toc
}

fn first_token(s: &str) -> String {
    s.split_whitespace().next().unwrap_or(s).to_string()
}

/// "18.6 (Homebrew)" → "18.6"
fn version_number(v: &str) -> String {
    v.split_whitespace().next().unwrap_or(v).to_string()
}

fn parse_entry(line: &str) -> Option<Entry> {
    let (id, rest) = line.split_once(';')?;
    let id: u32 = id.trim().parse().ok()?;
    // rest: " <catalog oid> <object oid> DESC schema name owner"
    let mut parts = rest.trim_start().splitn(3, ' ');
    let _cat = parts.next()?;
    let _oid = parts.next()?;
    let tail = parts.next()?;
    let desc = KNOWN_DESCS
        .iter()
        .find(|d| tail == **d || tail.starts_with(&format!("{d} ")))?;
    let after = tail[desc.len()..].trim_start_matches(' ');
    // schema is the first token ("-" for none)
    let (schema_tok, after_schema) = after.split_once(' ').unwrap_or((after, ""));
    let schema = if schema_tok == "-" || schema_tok.is_empty() {
        None
    } else {
        Some(schema_tok.to_string())
    };
    // Owner is the last token unless the line ends with a space (no owner).
    let name = if after_schema.ends_with(' ') || !after_schema.contains(' ') {
        after_schema.trim_end().to_string()
    } else {
        match after_schema.rsplit_once(' ') {
            Some((n, _owner)) => n.to_string(),
            None => after_schema.to_string(),
        }
    };
    Some(Entry {
        id,
        desc: desc.to_string(),
        schema,
        name,
    })
}

/// Object counts expected after a full restore of this archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub tables: usize,
    pub sequences: usize,
    pub views: usize,
    pub matviews: usize,
    pub indexes: usize,
    pub functions: usize,
    pub data_entries: usize,
    pub total_entries: usize,
    pub extensions: Vec<String>,
    pub languages: Vec<String>,
    pub publications: Vec<String>,
    pub subscriptions: Vec<String>,
    pub large_objects: bool,
}

impl Toc {
    pub fn inventory(&self) -> Inventory {
        let mut inv = Inventory {
            total_entries: self.entries.len(),
            ..Default::default()
        };
        let mut ext = BTreeSet::new();
        let mut lang = BTreeSet::new();
        for e in &self.entries {
            match e.desc.as_str() {
                "TABLE" => inv.tables += 1,
                "SEQUENCE" => inv.sequences += 1,
                "VIEW" => inv.views += 1,
                "MATERIALIZED VIEW" => inv.matviews += 1,
                "INDEX" => inv.indexes += 1,
                "FUNCTION" | "PROCEDURE" | "AGGREGATE" => inv.functions += 1,
                "TABLE DATA" => inv.data_entries += 1,
                // Extension and language names never contain spaces; keep only
                // the first token so an owner name with a space cannot leak in.
                "EXTENSION" => {
                    ext.insert(first_token(&e.name));
                }
                "PROCEDURAL LANGUAGE" => {
                    lang.insert(first_token(&e.name));
                }
                "PUBLICATION" => inv.publications.push(e.name.clone()),
                "SUBSCRIPTION" => inv.subscriptions.push(e.name.clone()),
                "BLOB" | "BLOBS" | "BLOB METADATA" | "LARGE OBJECT" => inv.large_objects = true,
                _ => {}
            }
        }
        inv.extensions = ext.into_iter().collect();
        inv.languages = lang.into_iter().collect();
        inv
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub rule: &'static str,
    pub object: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gate {
    pub reasons: Vec<Reason>,
    pub warnings: Vec<String>,
}

impl Gate {
    pub fn compatible(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// Rules evaluated before the restore. The restore itself (`--exit-on-error`)
/// is the final arbiter for anything these cannot see.
///
/// `available_extensions` comes from the running engine's
/// `pg_available_extensions`; `pgboxdb_major` from its `server_version_num`.
pub fn evaluate(inv: &Inventory, available_extensions: &[String], pgboxdb_major: u32) -> Gate {
    let mut g = Gate::default();
    for e in &inv.extensions {
        if !available_extensions.iter().any(|a| a == e) {
            g.reasons.push(Reason {
                rule: "extension_unavailable",
                object: e.clone(),
                detail: "extension is not built into PGBoxDB".into(),
            });
        }
    }
    for l in &inv.languages {
        if l != "plpgsql" && l != "sql" && l != "internal" && l != "c" {
            g.reasons.push(Reason {
                rule: "language_unavailable",
                object: l.clone(),
                detail: "PGBoxDB runs PL/pgSQL and SQL functions only".into(),
            });
        }
    }
    for s in &inv.subscriptions {
        g.reasons.push(Reason {
            rule: "feature_unsupported",
            object: s.clone(),
            detail: "logical replication subscriptions are not available in PGBoxDB".into(),
        });
    }
    for p in &inv.publications {
        g.warnings.push(format!(
            "publication {p} not copied (logical replication is off in PGBoxDB)"
        ));
    }
    if inv.large_objects {
        g.warnings.push("large objects (pg_largeobject) are copied; they are not shared copy-on-write between clones as efficiently as tables".into());
    }
    let _ = pgboxdb_major;
    g
}

/// Version-related warnings, kept apart from `evaluate` because they come
/// from the TOC header rather than the inventory.
pub fn version_warnings(toc: &Toc, pgboxdb_major: u32) -> Vec<String> {
    let mut w = Vec::new();
    if let Some(m) = toc.source_major {
        if m > pgboxdb_major {
            w.push(format!(
                "source is PostgreSQL {} but PGBoxDB is PostgreSQL {pgboxdb_major}-compatible; objects using newer features may fail to restore",
                toc.source_version.as_deref().unwrap_or("?")
            ));
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#";
; Archive created at 2026-10-24 18:27:35 PDT
;     dbname: myapp
;     TOC Entries: 36
;     Compression: gzip
;     Dump Version: 1.16-0
;     Format: CUSTOM
;     Integer: 4 bytes
;     Offset: 8 bytes
;     Dumped from database version: 18.6 (Homebrew)
;     Dumped by pg_dump version: 18.6 (Homebrew)
;
;
; Selected TOC Entries:
;
3; 3079 16425 EXTENSION - citext
3911; 0 0 COMMENT - EXTENSION citext
2; 3079 16387 EXTENSION - pgcrypto
955; 1247 16571 TYPE public mood app
321; 1255 16567 FUNCTION public post_count(bigint) app
322; 1255 16568 FUNCTION public f(integer, text) app
226; 1259 16578 SEQUENCE public order_no app
224; 1259 16545 TABLE public posts app
227; 1259 16579 MATERIALIZED VIEW public post_stats app
3913; 0 0 SEQUENCE OWNED BY public posts_id_seq app
222; 1259 16531 TABLE public users app
3914; 0 0 COMMENT public TABLE users app
225; 1259 16563 VIEW public user_posts app
3738; 2604 16548 DEFAULT public posts id app
3902; 0 16545 TABLE DATA public posts app
3900; 0 16531 TABLE DATA public users app
3916; 0 0 SEQUENCE SET public order_no app
3746; 2606 16555 CONSTRAINT public posts posts_pkey app
3744; 1259 16562 INDEX public posts_meta_gin app
3749; 2620 16569 TRIGGER public users users_touch app
3748; 2606 16556 FK CONSTRAINT public posts posts_user_id_fkey app
3904; 0 16579 MATERIALIZED VIEW DATA public post_stats app
4000; 6104 16600 PUBLICATION - mypub app
4001; 0 0 PROCEDURAL LANGUAGE - plpython3u
4002; 0 0 PROCEDURAL LANGUAGE - plpgsql my role
4; 0 0 ENCODING - ENCODING
5; 0 0 STDSTRINGS - STDSTRINGS
"#;

    #[test]
    fn header_fields() {
        let t = parse(SAMPLE);
        assert_eq!(t.dbname.as_deref(), Some("myapp"));
        assert_eq!(t.source_version.as_deref(), Some("18.6"));
        assert_eq!(t.source_major, Some(18));
        assert_eq!(t.dump_version.as_deref(), Some("18.6"));
    }

    #[test]
    fn entries_split_kind_schema_name_owner() {
        let t = parse(SAMPLE);
        let find = |id: u32| t.entries.iter().find(|e| e.id == id).unwrap();
        assert_eq!(find(3).desc, "EXTENSION");
        assert_eq!(find(3).schema, None);
        assert_eq!(find(3).name, "citext");
        assert_eq!(find(224).desc, "TABLE");
        assert_eq!(find(224).schema.as_deref(), Some("public"));
        assert_eq!(find(224).name, "posts");
        assert_eq!(find(3902).desc, "TABLE DATA");
        assert_eq!(find(322).name, "f(integer, text)", "signature with a space");
        assert_eq!(find(3748).desc, "FK CONSTRAINT");
        assert_eq!(find(3748).name, "posts posts_user_id_fkey");
        assert_eq!(find(227).desc, "MATERIALIZED VIEW");
        assert_eq!(find(3904).desc, "MATERIALIZED VIEW DATA");
        assert_eq!(find(4).desc, "ENCODING");
        assert_eq!(find(4001).desc, "PROCEDURAL LANGUAGE");
        assert_eq!(find(4001).name, "plpython3u");
    }

    #[test]
    fn inventory_counts() {
        let inv = parse(SAMPLE).inventory();
        assert_eq!(inv.tables, 2);
        assert_eq!(inv.sequences, 1);
        assert_eq!(inv.views, 1);
        assert_eq!(inv.matviews, 1);
        assert_eq!(inv.indexes, 1);
        assert_eq!(inv.functions, 2);
        assert_eq!(inv.data_entries, 2);
        assert_eq!(inv.extensions, vec!["citext", "pgcrypto"]);
        assert_eq!(inv.languages, vec!["plpgsql", "plpython3u"]);
        assert_eq!(inv.publications, vec!["mypub"]);
        assert!(!inv.large_objects);
    }

    #[test]
    fn gate_refuses_unknown_extensions_and_languages() {
        let inv = parse(SAMPLE).inventory();
        let avail = vec!["citext".to_string(), "plpgsql".to_string()];
        let g = evaluate(&inv, &avail, 18);
        assert!(!g.compatible());
        let rules: Vec<(&str, &str)> = g
            .reasons
            .iter()
            .map(|r| (r.rule, r.object.as_str()))
            .collect();
        assert_eq!(
            rules,
            vec![
                ("extension_unavailable", "pgcrypto"),
                ("language_unavailable", "plpython3u")
            ]
        );
        assert!(g.warnings.iter().any(|w| w.contains("publication mypub")));
    }

    #[test]
    fn gate_passes_when_everything_is_available() {
        let mut inv = parse(SAMPLE).inventory();
        inv.languages.clear();
        inv.publications.clear();
        let avail: Vec<String> = ["citext", "pgcrypto", "plpgsql"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let g = evaluate(&inv, &avail, 18);
        assert!(g.compatible(), "{:?}", g.reasons);
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn newer_source_is_a_warning_not_a_refusal() {
        let mut t = parse(SAMPLE);
        t.source_major = Some(19);
        t.source_version = Some("19.1".into());
        let w = version_warnings(&t, 18);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("19.1"));
        assert!(version_warnings(&parse(SAMPLE), 18).is_empty());
    }

    #[test]
    fn garbage_lines_are_ignored() {
        let t = parse("not a toc\n12; x y\n; Dumped from database version: 16.4\n");
        assert!(t.entries.is_empty());
        assert_eq!(t.source_major, Some(16));
    }
}
