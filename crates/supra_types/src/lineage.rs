//! Lineage: who spawned whom, and why a peer cannot spawn anything.
//!
//! This is the logical half of T12.5's anti-self-spawn guarantee. The physical half,
//! proving that a binary about to be executed is not this binary, is T12.5's own
//! work, and T4 already recorded why it needs two mechanisms: `self_identity`
//! compares device and inode, but a *copied* binary has a different inode, so an
//! environment marker is mandatory alongside it.
//!
//! What this module contributes is the structural rule. A lineage is a chain of
//! [`AgentId`] from the host outward, and [`Lineage::child`] refuses two things:
//!
//! - an id that already appears in the chain, which is a cycle and therefore a
//!   model spawning itself;
//! - any depth beyond [`Lineage::MAX_DEPTH`], which is a nested sub-agent.
//!
//! # Why the maximum depth is 1
//!
//! The host is the root at depth 0 and peers are its children at depth 1. There is
//! no depth 2, because nested sub-agents are explicitly out of scope and because
//! the peer model has no hierarchy to nest: k peers are siblings that validate each
//! other, not a tree with a coordinator at the top.
//!
//! Stating that as a depth limit rather than as an absence is deliberate. A limit
//! gives the guard something concrete to refuse and something concrete to report,
//! and it makes "this system is flat" a checked property rather than a description.

use serde::{Deserialize, Serialize};

use crate::id::AgentId;

/// The chain of agents from the host to one peer, root first.
///
/// Deserialisation re-validates, so a tampered session file cannot restore a
/// lineage that [`Lineage::child`] would have refused to build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LineageRepr")]
pub struct Lineage {
    chain: Vec<AgentId>,
}

impl Lineage {
    /// Deepest permitted lineage: the host, then one generation of peers.
    pub const MAX_DEPTH: usize = 1;

    /// Start a lineage at the host.
    #[must_use]
    pub fn root(host: AgentId) -> Self {
        Self { chain: vec![host] }
    }

    /// Distance from the root. The root itself is 0.
    #[must_use]
    pub fn depth(&self) -> usize {
        // A chain always holds at least the root, so this cannot underflow.
        self.chain.len() - 1
    }

    /// The host at the base of the chain.
    #[must_use]
    pub fn root_id(&self) -> AgentId {
        // Construction guarantees a non-empty chain; the fallback keeps this
        // infallible without an unwrap on a path that a guard layer calls.
        self.chain.first().copied().unwrap_or_else(AgentId::nil)
    }

    /// The agent this lineage identifies.
    #[must_use]
    pub fn agent_id(&self) -> AgentId {
        self.chain.last().copied().unwrap_or_else(AgentId::nil)
    }

    /// Whoever spawned [`Self::agent_id`], if that is not the root itself.
    #[must_use]
    pub fn parent_id(&self) -> Option<AgentId> {
        if self.chain.len() < 2 {
            return None;
        }
        self.chain.get(self.chain.len() - 2).copied()
    }

    /// The chain, root first.
    #[must_use]
    pub fn chain(&self) -> &[AgentId] {
        &self.chain
    }

    /// Whether `id` already appears in this lineage.
    #[must_use]
    pub fn contains(&self, id: AgentId) -> bool {
        self.chain.contains(&id)
    }

    /// Whether extending by `id` would be a model spawning itself.
    ///
    /// Offered as a predicate so a guard layer can report the refusal it is about
    /// to make before making it, rather than turning an error into a log line.
    #[must_use]
    pub fn would_self_spawn(&self, id: AgentId) -> bool {
        self.contains(id)
    }

    /// Whether this lineage may be extended at all.
    #[must_use]
    pub fn can_extend(&self) -> bool {
        self.depth() < Self::MAX_DEPTH
    }

    /// Extend the lineage by one generation.
    ///
    /// # Errors
    ///
    /// [`LineageError::SelfSpawn`] when `id` is already in the chain: an agent
    /// appearing in its own ancestry is the cycle this guard exists to prevent.
    ///
    /// [`LineageError::MaxDepthExceeded`] when the result would be deeper than
    /// [`Self::MAX_DEPTH`], which is a nested sub-agent.
    pub fn child(&self, id: AgentId) -> Result<Self, LineageError> {
        if let Some(at) = self.chain.iter().position(|existing| *existing == id) {
            return Err(LineageError::SelfSpawn { id, at_depth: at });
        }
        let depth = self.depth() + 1;
        if depth > Self::MAX_DEPTH {
            return Err(LineageError::MaxDepthExceeded { attempted: depth });
        }

        let mut chain = self.chain.clone();
        chain.push(id);
        Ok(Self { chain })
    }
}

/// The stored shape, used only as a deserialisation staging area.
#[derive(Deserialize)]
struct LineageRepr {
    chain: Vec<AgentId>,
}

impl TryFrom<LineageRepr> for Lineage {
    type Error = LineageError;

    fn try_from(repr: LineageRepr) -> Result<Self, Self::Error> {
        let mut chain = repr.chain.into_iter();
        let Some(root) = chain.next() else {
            return Err(LineageError::Empty);
        };

        let mut lineage = Self::root(root);
        for id in chain {
            lineage = lineage.child(id)?;
        }
        Ok(lineage)
    }
}

/// Why a lineage operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LineageError {
    /// The id is already an ancestor.
    #[error("agent {id} is already at depth {at_depth} of this lineage: a model may not spawn itself")]
    SelfSpawn {
        /// The offending id.
        id: AgentId,
        /// Where it already appears.
        at_depth: usize,
    },
    /// The extension would nest.
    #[error(
        "lineage depth {attempted} exceeds the maximum of {max}: peers are siblings, not a tree",
        max = Lineage::MAX_DEPTH
    )]
    MaxDepthExceeded {
        /// The depth that was refused.
        attempted: usize,
    },
    /// A stored lineage had no root.
    #[error("a lineage must contain at least its root")]
    Empty,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_lineage_is_depth_zero() {
        let host = AgentId::generate();
        let lineage = Lineage::root(host);
        assert_eq!(lineage.depth(), 0);
        assert_eq!(lineage.root_id(), host);
        assert_eq!(lineage.agent_id(), host);
        assert_eq!(lineage.parent_id(), None);
        assert!(lineage.can_extend());
    }

    #[test]
    fn a_peer_is_a_child_of_the_host() {
        let host = AgentId::generate();
        let peer = AgentId::generate();
        let lineage = Lineage::root(host).child(peer).expect("a peer may be spawned");

        assert_eq!(lineage.depth(), 1);
        assert_eq!(lineage.root_id(), host);
        assert_eq!(lineage.agent_id(), peer);
        assert_eq!(lineage.parent_id(), Some(host));
    }

    #[test]
    fn an_agent_cannot_spawn_itself() {
        // The rule the user asked for in as many words: one model must not spawn
        // itself. Both the direct case and the ancestor case are refused.
        let host = AgentId::generate();
        let root = Lineage::root(host);

        assert_eq!(root.child(host), Err(LineageError::SelfSpawn { id: host, at_depth: 0 }));
        assert!(root.would_self_spawn(host));

        let peer = AgentId::generate();
        let peer_lineage = root.child(peer).expect("distinct id");
        assert!(peer_lineage.would_self_spawn(peer));
        assert!(peer_lineage.would_self_spawn(host), "an ancestor counts too");
    }

    #[test]
    fn a_peer_cannot_spawn_anything() {
        // No nested sub-agents: the depth limit is what makes the topology flat.
        let peer_lineage =
            Lineage::root(AgentId::generate()).child(AgentId::generate()).expect("depth 1 is allowed");

        assert!(!peer_lineage.can_extend());
        assert_eq!(
            peer_lineage.child(AgentId::generate()),
            Err(LineageError::MaxDepthExceeded { attempted: 2 })
        );
    }

    #[test]
    fn a_cohort_is_siblings_not_a_hierarchy() {
        // The structural statement of "not an orchestrator": every peer in a cohort
        // sits at the same depth, and no peer is any other peer's ancestor.
        let host = AgentId::generate();
        let root = Lineage::root(host);

        let cohort: Vec<Lineage> =
            (0..80).map(|_| root.child(AgentId::generate()).expect("a sibling")).collect();

        assert_eq!(cohort.len(), 80, "the configured maximum");
        for peer in &cohort {
            assert_eq!(peer.depth(), 1, "no peer is deeper than any other");
            assert_eq!(peer.parent_id(), Some(host), "every peer answers to the host only");
        }

        for (index, peer) in cohort.iter().enumerate() {
            for (other_index, other) in cohort.iter().enumerate() {
                if index == other_index {
                    continue;
                }
                assert!(!peer.contains(other.agent_id()), "no peer may appear in another peer's ancestry");
            }
        }
    }

    #[test]
    fn a_stored_lineage_is_revalidated_on_load() {
        let lineage = Lineage::root(AgentId::generate()).child(AgentId::generate()).expect("valid");
        let json = serde_json::to_string(&lineage).expect("serialise");
        assert_eq!(serde_json::from_str::<Lineage>(&json).expect("deserialise"), lineage);
    }

    #[test]
    fn a_tampered_lineage_is_refused_on_load() {
        // Without revalidation, a hand-edited session file could restore a depth
        // that `child` would never have produced, and the guard would then be
        // reasoning about a topology that cannot occur.
        let deep = format!(
            r#"{{"chain":["{}","{}","{}"]}}"#,
            AgentId::generate(),
            AgentId::generate(),
            AgentId::generate()
        );
        let error = serde_json::from_str::<Lineage>(&deep).expect_err("too deep");
        assert!(error.to_string().contains("exceeds the maximum"), "got: {error}");

        let host = AgentId::generate();
        let cyclic = format!(r#"{{"chain":["{host}","{host}"]}}"#);
        let error = serde_json::from_str::<Lineage>(&cyclic).expect_err("a cycle");
        assert!(error.to_string().contains("spawn itself"), "got: {error}");

        let empty = r#"{"chain":[]}"#;
        let error = serde_json::from_str::<Lineage>(empty).expect_err("no root");
        assert!(error.to_string().contains("at least its root"), "got: {error}");
    }
}
