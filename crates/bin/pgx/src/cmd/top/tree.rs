//! Hierarchy, filtering and sorting for `pgx top`.
//!
//! ```text
//! project (project_id)
//!   Git branch + worktree (git_scope), when PGX Git knows it
//!     host · session (host + session_id)
//!       the session's own database, then its subagents' databases
//! Git · repository        PGX Git databases without an agent owner
//! Standalone / unowned    everything else
//! ```
//!
//! Every node has a stable string id built from identities, never from a
//! row position: `p:<project>`, `p:<project>/g:<scope>`,
//! `…/s:<host>:<session>`, `git:<repo>`, `standalone`, and `db:<oid>` for a
//! database. Two projects with the same session id, two worktrees on the
//! same Git branch and two hosts with the same session id therefore stay
//! apart, and selection/collapse state survives refreshes and re-sorting.
use super::model::{Database, Role};
use super::text::sanitize;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Deadline,
    Age,
    Connections,
}

impl SortKey {
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Name => SortKey::Deadline,
            SortKey::Deadline => SortKey::Age,
            SortKey::Age => SortKey::Connections,
            SortKey::Connections => SortKey::Name,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Deadline => "ttl",
            SortKey::Age => "age",
            SortKey::Connections => "conn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewOpts {
    /// Tree (true) or a flat list of databases.
    pub tree: bool,
    /// Only agent-owned databases.
    pub agents_only: bool,
    /// Case-insensitive search over safe fields.
    pub query: String,
    /// `--project`: an initial project filter.
    pub project: Option<String>,
    pub sort: SortKey,
}

impl Default for ViewOpts {
    fn default() -> Self {
        ViewOpts {
            tree: true,
            agents_only: false,
            query: String::new(),
            project: None,
            sort: SortKey::Name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKind {
    Project,
    GitScope,
    Session,
    GitRepo,
    Standalone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub kind: GroupKind,
    pub label: String,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Group(Group),
    /// Index into the snapshot's database list.
    Db(usize),
}

/// One visible line of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub depth: usize,
    /// Last among its siblings (for `└─`).
    pub last: bool,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Group {
        kind: GroupKind,
        label: String,
        dbs: usize,
        conns: Option<u64>,
        collapsed: bool,
        /// Session groups: whether the session's own database is present
        /// (a subagent can outlive it; the group is then synthetic).
        has_session_db: bool,
    },
    Db(usize),
}

pub fn host_label(host: &str) -> String {
    match host {
        "claude" => "Claude Code".into(),
        "cursor" => "Cursor".into(),
        "codex" => "Codex".into(),
        "exec" => "pgx agent exec".into(),
        other => sanitize(other),
    }
}

pub fn short(id: &str, n: usize) -> String {
    sanitize(&id.chars().take(n).collect::<String>())
}

/// The label of a database row in the tree.
pub fn db_label(d: &Database) -> String {
    let task = d
        .phase
        .as_ref()
        .and_then(|p| p.label.as_deref())
        .map(|l| format!(" · {}", sanitize(l)))
        .unwrap_or_default();
    match (&d.agent, d.role) {
        (Some(a), Role::Subagent) => {
            format!(
                "{}{task}",
                sanitize(a.agent_id.as_deref().unwrap_or("agent"))
            )
        }
        (Some(_), _) => format!("session db {}{task}", sanitize(&d.name)),
        (None, Role::GitMain | Role::GitBranch | Role::GitSnapshot) => match &d.git {
            Some(g) => format!("{} · {}", sanitize(&g.branch), sanitize(&d.name)),
            None => sanitize(&d.name),
        },
        _ => sanitize(&d.name),
    }
}

fn haystack(d: &Database) -> String {
    let mut h = vec![d.name.clone(), db_label(d)];
    if let Some(a) = &d.agent {
        h.push(a.project_name.clone());
        h.push(a.project_root.clone());
        h.push(a.host.clone());
        h.push(host_label(&a.host));
        h.push(a.session_id.clone());
        h.extend(a.agent_id.clone());
        h.extend(a.git_branch.clone());
        h.extend(a.worktree.clone());
    }
    if let Some(g) = &d.git {
        h.push(g.repo_name.clone());
        h.push(g.branch.clone());
    }
    if let Some(p) = &d.phase {
        h.push(p.phase.label().to_string());
        h.extend(p.label.clone());
    }
    h.join("\u{1}").to_lowercase()
}

fn matches_project(d: &Database, filter: &str) -> bool {
    let f = filter.to_lowercase();
    if let Some(a) = &d.agent {
        return a.project_name.to_lowercase().contains(&f)
            || a.project_id.to_lowercase().starts_with(&f)
            || a.project_root.to_lowercase().contains(&f);
    }
    if let Some(g) = &d.git {
        return g.repo_name.to_lowercase().contains(&f);
    }
    false
}

/// The databases the view shows, by index.
pub fn filtered(dbs: &[Database], opts: &ViewOpts) -> Vec<usize> {
    let q = opts.query.trim().to_lowercase();
    (0..dbs.len())
        .filter(|&i| {
            let d = &dbs[i];
            (!opts.agents_only || d.agent.is_some())
                && opts
                    .project
                    .as_deref()
                    .is_none_or(|p| p.is_empty() || matches_project(d, p))
                && (q.is_empty() || haystack(d).contains(&q))
        })
        .collect()
}

/// The group path a database lives under.
fn path(d: &Database) -> Vec<(String, GroupKind, String)> {
    if let Some(a) = &d.agent {
        let pid = format!("p:{}", a.project_id);
        let mut p = vec![(pid.clone(), GroupKind::Project, a.project_name.clone())];
        let mut base = pid;
        if let Some(scope) = &a.git_scope {
            let gid = format!("{base}/g:{scope}");
            let branch = a
                .git_branch
                .as_deref()
                .map(sanitize)
                .unwrap_or_else(|| "(unknown branch)".into());
            let wt = a
                .worktree
                .as_deref()
                .and_then(|w| std::path::Path::new(w).file_name())
                .map(|f| format!("  [{}]", sanitize(&f.to_string_lossy())))
                .unwrap_or_default();
            p.push((gid.clone(), GroupKind::GitScope, format!("⎇ {branch}{wt}")));
            base = gid;
        }
        p.push((
            format!("{base}/s:{}:{}", a.host, a.session_id),
            GroupKind::Session,
            format!(
                "{} · session {}",
                host_label(&a.host),
                short(&a.session_id, 8)
            ),
        ));
        return p;
    }
    if let Some(g) = &d.git {
        return vec![(
            format!("git:{}", g.repo_id),
            GroupKind::GitRepo,
            format!("Git · {}", g.repo_name),
        )];
    }
    vec![(
        "standalone".into(),
        GroupKind::Standalone,
        "Standalone / unowned".into(),
    )]
}

struct Building {
    kind: GroupKind,
    label: String,
    groups: BTreeMap<String, Building>,
    dbs: Vec<usize>,
}

fn insert(map: &mut BTreeMap<String, Building>, path: &[(String, GroupKind, String)], idx: usize) {
    let (id, kind, label) = &path[0];
    let b = map.entry(id.clone()).or_insert_with(|| Building {
        kind: *kind,
        label: label.clone(),
        groups: BTreeMap::new(),
        dbs: Vec::new(),
    });
    if path.len() == 1 {
        b.dbs.push(idx);
    } else {
        insert(&mut b.groups, &path[1..], idx);
    }
}

/// Databases under a node (for aggregates and sorting).
pub fn members(n: &Node, out: &mut Vec<usize>) {
    match n {
        Node::Db(i) => out.push(*i),
        Node::Group(g) => g.children.iter().for_each(|c| members(c, out)),
    }
}

fn sort_value(n: &Node, dbs: &[Database], key: SortKey) -> (u64, String) {
    let mut m = Vec::new();
    members(n, &mut m);
    let name = match n {
        Node::Db(i) => db_label(&dbs[*i]).to_lowercase(),
        Node::Group(g) => g.label.to_lowercase(),
    };
    let v = match key {
        SortKey::Name => 0,
        SortKey::Deadline => m
            .iter()
            .filter_map(|i| dbs[*i].expires_at)
            .min()
            .unwrap_or(u64::MAX),
        SortKey::Age => {
            u64::MAX
                - m.iter()
                    .filter_map(|i| dbs[*i].created_at)
                    .max()
                    .unwrap_or(0)
        }
        SortKey::Connections => {
            u64::MAX - m.iter().filter_map(|i| dbs[*i].connections).sum::<u64>()
        }
    };
    (v, name)
}

fn node_id(n: &Node, dbs: &[Database]) -> String {
    match n {
        Node::Db(i) => dbs[*i].id(),
        Node::Group(g) => g.id.clone(),
    }
}

fn rank(n: &Node, dbs: &[Database]) -> u8 {
    match n {
        // Inside a session, its own database first.
        Node::Db(i) if dbs[*i].role == Role::Session => 0,
        Node::Db(_) => 1,
        Node::Group(g) => match g.kind {
            GroupKind::Standalone => 4,
            GroupKind::GitRepo => 3,
            _ => 2,
        },
    }
}

fn sort_nodes(nodes: &mut [Node], dbs: &[Database], key: SortKey) {
    nodes.sort_by(|a, b| {
        rank(a, dbs)
            .cmp(&rank(b, dbs))
            .then_with(|| sort_value(a, dbs, key).cmp(&sort_value(b, dbs, key)))
            .then_with(|| node_id(a, dbs).cmp(&node_id(b, dbs)))
    });
}

fn finish(map: BTreeMap<String, Building>, dbs: &[Database], key: SortKey) -> Vec<Node> {
    let mut out: Vec<Node> = map
        .into_iter()
        .map(|(id, b)| {
            let mut children: Vec<Node> = b.dbs.into_iter().map(Node::Db).collect();
            children.extend(finish(b.groups, dbs, key));
            sort_nodes(&mut children, dbs, key);
            Node::Group(Group {
                id,
                kind: b.kind,
                label: b.label,
                children,
            })
        })
        .collect();
    sort_nodes(&mut out, dbs, key);
    out
}

/// The view's roots: groups (tree) or databases (flat).
pub fn build(dbs: &[Database], opts: &ViewOpts) -> Vec<Node> {
    let idx = filtered(dbs, opts);
    if !opts.tree {
        let mut nodes: Vec<Node> = idx.into_iter().map(Node::Db).collect();
        nodes.sort_by(|a, b| {
            sort_value(a, dbs, opts.sort)
                .cmp(&sort_value(b, dbs, opts.sort))
                .then_with(|| node_id(a, dbs).cmp(&node_id(b, dbs)))
        });
        return nodes;
    }
    let mut map = BTreeMap::new();
    for i in idx {
        insert(&mut map, &path(&dbs[i]), i);
    }
    finish(map, dbs, opts.sort)
}

/// The visible rows: depth-first, children of collapsed groups hidden.
pub fn flatten(roots: &[Node], dbs: &[Database], collapsed: &HashSet<String>) -> Vec<Row> {
    fn walk(
        nodes: &[Node],
        depth: usize,
        dbs: &[Database],
        collapsed: &HashSet<String>,
        out: &mut Vec<Row>,
    ) {
        for (k, n) in nodes.iter().enumerate() {
            let last = k + 1 == nodes.len();
            match n {
                Node::Db(i) => out.push(Row {
                    id: dbs[*i].id(),
                    depth,
                    last,
                    kind: RowKind::Db(*i),
                }),
                Node::Group(g) => {
                    let mut m = Vec::new();
                    members(n, &mut m);
                    let conns = m
                        .iter()
                        .map(|i| dbs[*i].connections)
                        .try_fold(0u64, |acc, c| c.map(|c| acc + c));
                    let is_collapsed = collapsed.contains(&g.id);
                    out.push(Row {
                        id: g.id.clone(),
                        depth,
                        last,
                        kind: RowKind::Group {
                            kind: g.kind,
                            label: g.label.clone(),
                            dbs: m.len(),
                            conns,
                            collapsed: is_collapsed,
                            has_session_db: g
                                .children
                                .iter()
                                .any(|c| matches!(c, Node::Db(i) if dbs[*i].role == Role::Session)),
                        },
                    });
                    if !is_collapsed {
                        walk(&g.children, depth + 1, dbs, collapsed, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(roots, 0, dbs, collapsed, &mut out);
    out
}

/// The id of the group that contains `id`, if any.
pub fn parent_of(roots: &[Node], dbs: &[Database], id: &str) -> Option<String> {
    fn find(
        nodes: &[Node],
        dbs: &[Database],
        id: &str,
        parent: Option<&str>,
    ) -> Option<Option<String>> {
        for n in nodes {
            if node_id(n, dbs) == id {
                return Some(parent.map(String::from));
            }
            if let Node::Group(g) = n {
                if let Some(r) = find(&g.children, dbs, id, Some(&g.id)) {
                    return Some(r);
                }
            }
        }
        None
    }
    find(roots, dbs, id, None).flatten()
}

#[cfg(test)]
mod tests {
    use super::super::model::fixtures::*;
    use super::super::model::*;
    use super::*;

    fn ids(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.id.clone()).collect()
    }

    fn five_agents() -> Vec<Database> {
        let mut v = vec![agent_db(
            100,
            "agent-claude-s1",
            "remotely",
            "claude",
            "s1",
            None,
        )];
        for (i, a) in ["agent-a", "agent-b", "agent-c", "agent-d", "agent-e"]
            .iter()
            .enumerate()
        {
            v.push(agent_db(
                101 + i as u64,
                &format!("agent-claude-s1-{a}"),
                "remotely",
                "claude",
                "s1",
                Some(a),
            ));
        }
        v.push(standalone(5, "postgres"));
        v.push(standalone(6, "app"));
        v
    }

    #[test]
    fn project_session_subagent_hierarchy_is_exact() {
        let dbs = five_agents();
        let roots = build(&dbs, &ViewOpts::default());
        let rows = flatten(&roots, &dbs, &HashSet::new());
        let groups: Vec<&Row> = rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Group { .. }))
            .collect();
        assert_eq!(groups.len(), 3, "project, session, standalone");
        match &rows[0].kind {
            RowKind::Group {
                kind,
                label,
                dbs: n,
                ..
            } => {
                assert_eq!(*kind, GroupKind::Project);
                assert_eq!(label, "remotely");
                assert_eq!(*n, 6);
            }
            other => panic!("{other:?}"),
        }
        match &rows[1].kind {
            RowKind::Group {
                kind,
                dbs: n,
                has_session_db,
                label,
                ..
            } => {
                assert_eq!(*kind, GroupKind::Session);
                assert_eq!(*n, 6);
                assert!(*has_session_db);
                assert_eq!(label, "Claude Code · session s1");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(rows[2].id, "db:100", "session db first");
        assert_eq!(rows[2].depth, 2);
        let subs: Vec<&Row> = rows[3..8].iter().collect();
        assert!(subs
            .iter()
            .all(|r| r.depth == 2 && matches!(r.kind, RowKind::Db(_))));
        assert!(rows[7].last);
        assert_eq!(rows[8].id, "standalone");
        assert_eq!(rows.len(), 11);
    }

    #[test]
    fn identities_that_collide_by_name_stay_apart() {
        let mut dbs = vec![
            agent_db(1, "a", "p1", "claude", "same", None),
            agent_db(2, "b", "p2", "claude", "same", None), // other project, same session id
            agent_db(3, "c", "p1", "codex", "same", None),  // same project+session, other host
            agent_db(4, "d", "p1", "claude", "same", Some("agent-x")),
            agent_db(5, "e", "p1", "claude", "same", Some("agent-y")),
        ];
        // Two worktrees on the same Git branch: different scopes.
        for (oid, scope, wt) in [
            (6u64, "scopeA", "/w/main-wt"),
            (7, "scopeB", "/w/second-wt"),
        ] {
            let mut d = agent_db(oid, &format!("g{oid}"), "p3", "claude", "s9", None);
            let a = d.agent.as_mut().unwrap();
            a.git_scope = Some(scope.into());
            a.git_branch = Some("feature-auth".into());
            a.worktree = Some(wt.into());
            dbs.push(d);
        }
        let roots = build(&dbs, &ViewOpts::default());
        let rows = flatten(&roots, &dbs, &HashSet::new());
        let gids: HashSet<String> = rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Group { .. }))
            .map(|r| r.id.clone())
            .collect();
        for want in [
            "p:p1-id",
            "p:p2-id",
            "p:p3-id",
            "p:p1-id/s:claude:same",
            "p:p2-id/s:claude:same",
            "p:p1-id/s:codex:same",
            "p:p3-id/g:scopeA",
            "p:p3-id/g:scopeB",
            "p:p3-id/g:scopeA/s:claude:s9",
            "p:p3-id/g:scopeB/s:claude:s9",
        ] {
            assert!(gids.contains(want), "missing group {want}: {gids:?}");
        }
        assert_eq!(gids.len(), 10);
        // Both subagents sit under the one parent session, each its own row.
        let parent = rows
            .iter()
            .position(|r| r.id == "p:p1-id/s:claude:same")
            .unwrap();
        let under: Vec<&str> = rows[parent + 1..parent + 4]
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(under, ["db:1", "db:4", "db:5"]);
        let labels: Vec<String> = rows
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Group {
                    kind: GroupKind::GitScope,
                    label,
                    ..
                } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("main-wt"))
                && labels.iter().any(|l| l.contains("second-wt"))
        );
    }

    #[test]
    fn parent_missing_children_present_is_a_synthetic_session_group() {
        let dbs = vec![
            agent_db(4, "d", "p1", "claude", "s1", Some("agent-x")),
            agent_db(5, "e", "p1", "claude", "s1", Some("agent-y")),
        ];
        let rows = flatten(&build(&dbs, &ViewOpts::default()), &dbs, &HashSet::new());
        match &rows[1].kind {
            RowKind::Group {
                kind: GroupKind::Session,
                has_session_db,
                dbs: n,
                ..
            } => {
                assert!(!has_session_db);
                assert_eq!(*n, 2);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            rows.iter().all(|r| r.id != "db:0"),
            "no invented parent database"
        );
    }

    #[test]
    fn agents_only_removes_exactly_the_unowned_nodes() {
        let mut dbs = five_agents();
        let mut g = standalone(60, "remotely_main");
        g.role = Role::GitMain;
        g.git = Some(GitInfo {
            repo_id: "r1".into(),
            repo_name: "remotely".into(),
            branch: "main".into(),
            worktree_path: "/w".into(),
        });
        dbs.push(g);
        let all = flatten(&build(&dbs, &ViewOpts::default()), &dbs, &HashSet::new());
        let n_db = |rows: &[Row]| {
            rows.iter()
                .filter(|r| matches!(r.kind, RowKind::Db(_)))
                .count()
        };
        assert_eq!(n_db(&all), dbs.len(), "nothing disappears in all mode");
        assert!(all.iter().any(|r| r.id == "git:r1"));
        assert!(all.iter().any(|r| r.id == "standalone"));
        let opts = ViewOpts {
            agents_only: true,
            ..Default::default()
        };
        let agents = flatten(&build(&dbs, &opts), &dbs, &HashSet::new());
        assert_eq!(n_db(&agents), 6);
        assert!(agents
            .iter()
            .all(|r| r.id != "git:r1" && r.id != "standalone"));
    }

    #[test]
    fn search_is_case_insensitive_and_keeps_ancestors() {
        let mut dbs = five_agents();
        dbs[2].phase = Some(TaskPhase {
            phase: Phase::Testing,
            label: Some("OAuth".into()),
            reported_at: NOW,
            source: "pgx agent activity".into(),
        });
        let opts = ViewOpts {
            query: "oauth".into(),
            ..Default::default()
        };
        let rows = flatten(&build(&dbs, &opts), &dbs, &HashSet::new());
        assert_eq!(
            ids(&rows),
            ["p:remotely-id", "p:remotely-id/s:claude:s1", "db:102"]
        );
        let opts = ViewOpts {
            query: "TESTING".into(),
            ..Default::default()
        };
        assert_eq!(flatten(&build(&dbs, &opts), &dbs, &HashSet::new()).len(), 3);
        let opts = ViewOpts {
            query: "REMOTELY".into(),
            ..Default::default()
        };
        assert_eq!(
            flatten(&build(&dbs, &opts), &dbs, &HashSet::new())
                .iter()
                .filter(|r| matches!(r.kind, RowKind::Db(_)))
                .count(),
            6
        );
        let opts = ViewOpts {
            query: "no-such-thing".into(),
            ..Default::default()
        };
        assert!(build(&dbs, &opts).is_empty());
        let opts = ViewOpts {
            project: Some("REMOTE".into()),
            ..Default::default()
        };
        assert_eq!(filtered(&dbs, &opts).len(), 6);
    }

    #[test]
    fn sorting_keeps_parents_and_orders_siblings() {
        let mut dbs = five_agents();
        // agent-c: soonest deadline, most connections, newest.
        dbs[3].expires_at = Some(NOW + 10);
        dbs[3].connections = Some(9);
        dbs[3].created_at = Some(NOW - 5);
        for key in [SortKey::Deadline, SortKey::Connections, SortKey::Age] {
            let opts = ViewOpts {
                sort: key,
                ..Default::default()
            };
            let rows = flatten(&build(&dbs, &opts), &dbs, &HashSet::new());
            assert_eq!(rows[0].id, "p:remotely-id", "{key:?}");
            assert_eq!(rows[1].id, "p:remotely-id/s:claude:s1");
            assert_eq!(rows[2].id, "db:100", "session db stays first under {key:?}");
            assert_eq!(rows[3].id, "db:103", "{key:?}");
            assert_eq!(
                rows.last().unwrap().depth,
                1,
                "standalone children stay nested"
            );
        }
        let flat = ViewOpts {
            tree: false,
            sort: SortKey::Connections,
            ..Default::default()
        };
        let rows = flatten(&build(&dbs, &flat), &dbs, &HashSet::new());
        assert_eq!(rows[0].id, "db:103");
        assert!(rows
            .iter()
            .all(|r| r.depth == 0 && matches!(r.kind, RowKind::Db(_))));
        assert_eq!(rows.len(), dbs.len());
        let mut k = SortKey::Name;
        for _ in 0..4 {
            k = k.next();
        }
        assert_eq!(k, SortKey::Name);
    }

    #[test]
    fn collapsing_hides_children_and_parent_lookup_works() {
        let dbs = five_agents();
        let roots = build(&dbs, &ViewOpts::default());
        let mut c = HashSet::new();
        c.insert("p:remotely-id/s:claude:s1".to_string());
        let rows = flatten(&roots, &dbs, &c);
        assert_eq!(
            ids(&rows),
            [
                "p:remotely-id",
                "p:remotely-id/s:claude:s1",
                "standalone",
                "db:6",
                "db:5"
            ]
        );
        assert_eq!(
            parent_of(&roots, &dbs, "db:103").as_deref(),
            Some("p:remotely-id/s:claude:s1")
        );
        assert_eq!(
            parent_of(&roots, &dbs, "p:remotely-id/s:claude:s1").as_deref(),
            Some("p:remotely-id")
        );
        assert_eq!(parent_of(&roots, &dbs, "p:remotely-id"), None);
    }

    #[test]
    fn thousand_databases_hundred_agents_tree_is_stable() {
        let mut dbs = Vec::new();
        for i in 0..1000u64 {
            if i < 100 {
                dbs.push(agent_db(
                    10_000 + i,
                    &format!("a{i:04}"),
                    &format!("p{}", i % 7),
                    "claude",
                    &format!("s{}", i % 20),
                    Some(&format!("agent-{i}")),
                ));
            } else {
                dbs.push(standalone(10_000 + i, &format!("db{i:04}")));
            }
        }
        let t0 = std::time::Instant::now();
        let a = flatten(&build(&dbs, &ViewOpts::default()), &dbs, &HashSet::new());
        let b = flatten(&build(&dbs, &ViewOpts::default()), &dbs, &HashSet::new());
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            t0.elapsed()
        );
        assert_eq!(a, b, "deterministic");
        assert_eq!(
            a.iter()
                .filter(|r| matches!(r.kind, RowKind::Db(_)))
                .count(),
            1000
        );
        let mut sorted = a.clone();
        sorted.sort_by(|x: &Row, y: &Row| -> std::cmp::Ordering { x.id.cmp(&y.id) });
        sorted.dedup_by(|x, y| x.id == y.id);
        assert_eq!(sorted.len(), a.len(), "ids unique");
    }
}
