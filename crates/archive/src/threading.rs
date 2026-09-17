//! Thread order for the archive: replies under their parents, siblings by
//! date, as `HyperKitty` lays a thread out.
//!
//! A message's parent is the post its `In-Reply-To` (else the last
//! `References` entry) names, when the archive holds that post; a reply
//! whose parent never arrived stands at the top of its thread like a root.
//! Roots and siblings sort by date, then hash, so the order is stable.
use std::collections::BTreeMap;

/// One archived post as the ordering sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    /// Message-ID-Hash.
    pub hash: String,
    /// The hash the post replies to, when known.
    pub parent: Option<String>,
    /// The post's date in milliseconds; ties break on the hash.
    pub date_ms: i64,
}

/// A post placed in the thread: its hash and depth (roots are 0).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    pub hash: String,
    pub depth: usize,
}

/// Depth-first thread order over `nodes`. Cycles (possible after manual
/// reattachment) cannot trap the walk: every node is placed once, and a
/// node whose ancestors never reach a root is treated as a root.
#[must_use]
pub fn order(nodes: &[Node]) -> Vec<Placed> {
    let by_hash: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.hash.as_str(), n)).collect();
    let mut children: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    let mut roots: Vec<&Node> = Vec::new();
    for node in nodes {
        match node
            .parent
            .as_deref()
            .filter(|p| by_hash.contains_key(p) && *p != node.hash)
        {
            Some(parent) => children.entry(parent).or_default().push(node),
            None => roots.push(node),
        }
    }
    let by_date =
        |a: &&Node, b: &&Node| a.date_ms.cmp(&b.date_ms).then_with(|| a.hash.cmp(&b.hash));
    roots.sort_by(by_date);
    for list in children.values_mut() {
        list.sort_by(by_date);
    }
    let mut placed = Vec::with_capacity(nodes.len());
    let mut seen = std::collections::BTreeSet::new();
    let mut stack: Vec<(&Node, usize)> = roots.iter().rev().map(|n| (*n, 0)).collect();
    while let Some((node, depth)) = stack.pop() {
        if !seen.insert(node.hash.as_str()) {
            continue;
        }
        placed.push(Placed {
            hash: node.hash.clone(),
            depth,
        });
        if let Some(list) = children.get(node.hash.as_str()) {
            for child in list.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    // Nodes only reachable through a cycle stand as roots, by date.
    let mut orphans: Vec<&Node> = nodes
        .iter()
        .filter(|n| !seen.contains(n.hash.as_str()))
        .collect();
    orphans.sort_by(by_date);
    for node in orphans {
        let mut stack = vec![(node, 0)];
        while let Some((node, depth)) = stack.pop() {
            if !seen.insert(node.hash.as_str()) {
                continue;
            }
            placed.push(Placed {
                hash: node.hash.clone(),
                depth,
            });
            if let Some(list) = children.get(node.hash.as_str()) {
                for child in list.iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    placed
}
