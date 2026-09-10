//! The seven layers, in order, and the verdict that composes them.
//!
//! # Why seven, and why in this order
//!
//! Each layer refuses a different way of being this harness. The order is cheapest-check
//! first, most-authoritative last - but every layer runs even after one refuses, because the
//! caller reports *all* refusals, not the first. A single "refused by L3" tells the operator
//! what happened; "refused by L3, L4, and L5" tells them what was attempted.
//!
//! | Layer | Refuses | Mechanism |
//! | ----- | ------- | --------- |
//! | L1 | no identity established | [`crate::identity::is_established`] |
//! | L2 | no marker key generated | [`crate::marker::has_key`] |
//! | L3 | the command names this binary | `argv[0]`, path, and resolved file name |
//! | L4 | the command *is* this binary | (device, inode) via `supra_ffi` |
//! | L5 | the marker does not authenticate | HMAC-SHA256 over `version:nonce` |
//! | L6 | the lineage would cycle or nest | [`supra_types::Lineage::child`] |
//! | L7 | a proposer voting its own claim | voter id against proposer id |
//!
//! L1/L2 are readiness, not judgement: they refuse because the guard cannot yet tell, not
//! because the spawn is wrong. L3/L4 are about *this binary*; L5 covers the copy L4 cannot
//! see; L6 is about *this agent*; L7 is about *this claim*. Four different questions, which
//! is why one layer cannot substitute for another.
//!
//! # No off switch
//!
//! There is no flag, mode, or configuration that skips a layer. `yolo` relaxes consent
//! (T16.7); it does not touch authority, and these layers are authority. Disabling the
//! sandbox is a separate `--sandbox off` flag with its own confirmation; disabling the
//! guard is not an option at all.

use supra_types::{AgentId, Lineage, LineageError};

use crate::error::Refusal;
use crate::identity::{self, ProcessIdentity};
use crate::marker::{self, MARKER_ENV};

/// A request to start something that might be this harness.
#[derive(Clone, Debug)]
pub struct SpawnRequest<'a> {
    /// argv of the command, first element the program.
    pub argv: &'a [&'a str],
    /// Marker presented by the environment, if any.
    pub marker: Option<&'a str>,
    /// Lineage the child would extend, if spawning a peer.
    pub lineage: Option<&'a Lineage>,
    /// The id the child would carry, if spawning a peer.
    pub child: Option<AgentId>,
    /// The claim being voted on, if this is a vote.
    pub claim_proposer: Option<AgentId>,
    /// Who is voting, if this is a vote.
    pub voter: Option<AgentId>,
}

/// The layers' combined answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Every refusal, in layer order. Empty means allowed.
    pub refusals: Vec<Refusal>,
}

impl Verdict {
    /// Whether every layer passed.
    ///
    /// Not `const`: `Vec::is_empty` is not const on the pinned MSRV.
    #[must_use]
    pub fn allowed(&self) -> bool {
        self.refusals.is_empty()
    }

    /// Which layers refused, in order.
    #[must_use]
    pub fn refused_layers(&self) -> Vec<u8> {
        self.refusals.iter().map(layer_of).collect()
    }
}

/// Which layer a refusal came from.
///
/// L1 and L2 share the `NoIdentity` variant - both are "the guard cannot yet tell" - so the
/// detail distinguishes them: L1's detail names identity, L2's names the key.
#[must_use]
pub fn layer_of(refusal: &Refusal) -> u8 {
    match refusal {
        Refusal::NoIdentity { detail } => {
            if detail.contains("marker key") {
                2
            } else {
                1
            }
        }
        Refusal::OwnBinary { .. } => 3,
        Refusal::SameFile => 4,
        Refusal::BadMarker { .. } => 5,
        Refusal::BadLineage { .. } => 6,
        Refusal::SelfVote { .. } => 7,
        // `NoEntropy` is not a layer refusal - it is a failure to key the marker at all.
        // It sorts with L2 (readiness, not judgement) so a caller counting layers sees the
        // guard unready rather than a gap in the numbering.
        Refusal::NoEntropy { .. } => 2,
    }
}

/// Judge a spawn request through all seven layers.
///
/// Every layer runs unconditionally: short-circuiting on the first refusal would hide what
/// the other layers saw, and the operator needs the full picture. The layers that need
/// identity or key state report `NoIdentity` when it is absent rather than skipping -
/// absence is fail-closed, not "not applicable".
#[must_use]
pub fn judge(request: &SpawnRequest<'_>) -> Verdict {
    let mut refusals = Vec::new();

    // -- L1: identity established -------------------------------------------
    let identity = identity::current();
    if identity.is_none() {
        refusals.push(Refusal::NoIdentity { detail: "no process identity established".to_owned() });
    }

    // -- L2: marker key generated --------------------------------------------
    if !marker::has_key() {
        refusals.push(Refusal::NoIdentity { detail: "no marker key generated".to_owned() });
    }

    // -- L3: the command names this binary ------------------------------------
    if let Some(identity) = &identity {
        if let Some(detail) = own_binary_detail(identity, request.argv) {
            refusals.push(Refusal::OwnBinary { detail });
        }
    }

    // -- L4: the command resolves to this binary -------------------------------
    if let Some(identity) = &identity {
        // Only the program path is resolved: arguments are data, and resolving them would
        // refuse a command for mentioning a file that happens to be this binary - `rm
        // ./supra` must reach the permission gate, not die in the guard.
        if let Some(program) = request.argv.first() {
            // A resolution failure (NUL bytes and the like) is not absence: it is a
            // malformed command, and fail-closed means refusing it. `is_own_file` already
            // maps absence to `Ok(false)`; only `Err` reaches here. Bound first so the
            // match arms stay one refusal each, which is the shape both tools agree on.
            let own_file = identity.is_own_file(program);
            match own_file {
                Ok(true) => refusals.push(Refusal::SameFile),
                Ok(false) => (),
                Err(_) => {
                    let detail = format!("{program:?} cannot be resolved");
                    refusals.push(Refusal::OwnBinary { detail });
                }
            }
        }
    }

    // -- L5: the marker authenticates -------------------------------------------
    if marker::verify(request.marker).is_err() {
        // Re-derive the detail rather than carrying the verify error through: the layers
        // report in their own vocabulary, and `verify`'s message already says it.
        let detail = match request.marker {
            None => format!("no {MARKER_ENV} in the environment"),
            Some(_) => format!("{MARKER_ENV} does not authenticate under this process key"),
        };
        refusals.push(Refusal::BadMarker { detail });
    }

    // -- L6: the lineage allows the extension ------------------------------------
    if let (Some(lineage), Some(child)) = (request.lineage, request.child) {
        if let Err(error) = lineage.child(child) {
            let detail = match &error {
                LineageError::SelfSpawn { .. } => format!("a model may not spawn itself: {error}"),
                LineageError::MaxDepthExceeded { .. } => format!("peers may not nest: {error}"),
                LineageError::Empty => format!("the lineage has no root: {error}"),
            };
            refusals.push(Refusal::BadLineage { detail });
        }
    }

    // -- L7: no self-voting --------------------------------------------------------
    if let (Some(proposer), Some(voter)) = (request.claim_proposer, request.voter) {
        if proposer == voter {
            refusals.push(Refusal::SelfVote { voter, proposer });
        }
    }

    Verdict { refusals }
}

/// L3's comparison: argv[0] as given, its final component, and - where the path resolves -
/// the resolved file name. Three spellings because a spawn can name this binary three ways:
/// bare name through `PATH`, relative path, or absolute path.
fn own_binary_detail(identity: &ProcessIdentity, argv: &[&str]) -> Option<String> {
    let program = argv.first()?;
    // Bare name or final component.
    let base = program.rsplit('/').next().unwrap_or(program);
    if identity.is_own_name(base) {
        return Some(format!("argv[0] {program:?} names this binary"));
    }
    // Resolved file name: catches `./supra` from another directory and symlinks to it.
    if let Ok(Some(resolved)) = std::fs::canonicalize(program)
        .map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
    {
        if identity.is_own_name(&resolved) {
            return Some(format!("argv[0] {program:?} resolves to this binary"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::establish;
    use crate::marker::generate_key;

    fn setup() {
        establish();
        generate_key().expect("the OS provides entropy in tests");
    }

    fn benign_argv<'a>() -> Vec<&'a str> {
        vec!["/bin/sh", "-c", "cargo test"]
    }

    /// A request with a fresh marker, a fresh lineage extension, and no vote - the shape an
    /// honest host-side spawn takes. Asserts the helper itself is honest before using it.
    fn honest<'a>(
        argv: &'a [&'a str],
        marker: &'a str,
        lineage: &'a Lineage,
        child: AgentId,
    ) -> SpawnRequest<'a> {
        SpawnRequest {
            argv,
            marker: Some(marker),
            lineage: Some(lineage),
            child: Some(child),
            claim_proposer: None,
            voter: None,
        }
    }

    #[test]
    fn an_honest_host_spawn_passes_every_layer() {
        setup();
        let host = AgentId::generate();
        let lineage = Lineage::root(host);
        let marker = marker::issue().expect("keyed").expect("a marker");
        let argv = benign_argv();
        let request = honest(&argv, &marker, &lineage, AgentId::generate());
        let verdict = judge(&request);
        assert!(verdict.allowed(), "refused: {:?}", verdict.refusals);
    }

    #[test]
    fn a_valid_early_layer_does_not_skip_the_later_ones() {
        // The short-circuit mutation: returning early when one layer passes, instead of
        // only when one refuses. An honest spawn is valid at L6 and must still face L7 -
        // so this judges a valid-lineage spawn that self-votes, and demands both the
        // allowance of the lineage and the refusal of the vote. A suite with only
        // fully-honest and fully-dishonest fixtures cannot see an early-accept; this one
        // is honest everywhere except the last layer.
        setup();
        let host = AgentId::generate();
        let lineage = Lineage::root(host);
        let marker = marker::issue().expect("keyed").expect("a marker");
        let argv = benign_argv();
        // Valid at L6 (fresh child of the host) but self-voting at L7.
        let request = SpawnRequest {
            argv: &argv,
            marker: Some(&marker),
            lineage: Some(&lineage),
            child: Some(AgentId::generate()),
            claim_proposer: Some(host),
            voter: Some(host),
        };
        let verdict = judge(&request);
        assert!(!verdict.allowed(), "an early-accept at L6 skipped L7: {verdict:?}");
        assert_eq!(verdict.refused_layers(), vec![7], "{verdict:?}");
    }

    #[test]
    fn every_layer_runs_even_after_a_refusal() {
        // No short-circuit: a spawn that is wrong in three ways reports all three. The
        // operator needs the full picture, and a test that only sees the first refusal
        // cannot tell a targeted attempt from a confused one.
        setup();
        let identity = identity::current().expect("established");
        let own: String = identity.path().to_string_lossy().into_owned();
        let leaked_own = Box::leak(own.into_boxed_str());
        let argv = [leaked_own as &str];
        let host = AgentId::generate();
        let lineage = Lineage::root(host);
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: Some(&lineage),
            child: Some(host), // self-spawn: already in the chain
            claim_proposer: Some(host),
            voter: Some(host), // self-vote
        };
        let verdict = judge(&request);
        assert!(!verdict.allowed());
        let layers = verdict.refused_layers();
        assert!(layers.contains(&3), "L3 missing from {layers:?}");
        assert!(layers.contains(&4), "L4 missing from {layers:?}");
        assert!(layers.contains(&5), "L5 missing from {layers:?}");
        assert!(layers.contains(&6), "L6 missing from {layers:?}");
        assert!(layers.contains(&7), "L7 missing from {layers:?}");
    }

    #[test]
    fn l3_refuses_the_binary_by_bare_name() {
        setup();
        let identity = identity::current().expect("established");
        let name: String = identity.path().file_name().expect("a name").to_string_lossy().into_owned();
        let leaked = Box::leak(name.into_boxed_str());
        let argv = [leaked as &str];
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: None,
            child: None,
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        assert!(verdict.refused_layers().contains(&3), "{verdict:?}");
    }

    #[test]
    fn l4_refuses_a_symlink_to_this_binary() {
        // L3 compares names; a symlink with an innocent name passes it. L4 resolves the
        // file and compares (device, inode).
        setup();
        let dir = std::env::temp_dir().join("supra-guard-l4");
        let _ = std::fs::create_dir_all(&dir);
        let link = dir.join("innocent-name");
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::env::current_exe().expect("exe"), &link).expect("symlink");
        #[cfg(not(unix))]
        std::fs::copy(std::env::current_exe().expect("exe"), &link).expect("copy");

        let text = link.to_string_lossy().into_owned();
        let leaked = Box::leak(text.into_boxed_str());
        let argv = [leaked as &str];
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: None,
            child: None,
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        // Unix: L4 fires (L3 does not - the name is innocent). Non-Unix: the copy has a
        // different inode, so L4 cannot fire; L5 still refuses the missing marker.
        #[cfg(unix)]
        assert!(verdict.refused_layers().contains(&4), "{verdict:?}");
        assert!(verdict.refused_layers().contains(&5), "{verdict:?}");
        let _ = std::fs::remove_file(&link);
    }

    #[test]
    fn l5_refuses_a_cleared_environment() {
        // The documented residual: `env -i` strips the marker. Fail-closed means Absent
        // refuses - and the test proves the refusal, not the stripping.
        setup();
        let argv = benign_argv();
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: None,
            child: None,
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        assert_eq!(verdict.refused_layers(), vec![5], "{verdict:?}");
    }

    #[test]
    fn l5_refuses_a_forged_marker() {
        setup();
        let argv = benign_argv();
        let request = SpawnRequest {
            argv: &argv,
            marker: Some("1:00000000000000000000000000000000:00"),
            lineage: None,
            child: None,
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        assert!(verdict.refused_layers().contains(&5), "{verdict:?}");
    }

    #[test]
    fn l6_refuses_a_cycle_and_a_nesting() {
        setup();
        let host = AgentId::generate();
        let lineage = Lineage::root(host);
        let argv = benign_argv();

        // Cycle: the host spawning itself.
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: Some(&lineage),
            child: Some(host),
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        assert!(verdict.refused_layers().contains(&6), "{verdict:?}");

        // Nesting: a peer spawning anything at all.
        let peer = AgentId::generate();
        let peer_lineage = lineage.child(peer).expect("depth 1");
        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: Some(&peer_lineage),
            child: Some(AgentId::generate()),
            claim_proposer: None,
            voter: None,
        };
        let verdict = judge(&request);
        assert!(verdict.refused_layers().contains(&6), "{verdict:?}");
    }

    #[test]
    fn l7_refuses_a_self_vote_and_allows_any_other() {
        setup();
        let proposer = AgentId::generate();
        let other = AgentId::generate();
        let argv = benign_argv();

        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: None,
            child: None,
            claim_proposer: Some(proposer),
            voter: Some(proposer),
        };
        let verdict = judge(&request);
        assert!(verdict.refused_layers().contains(&7), "{verdict:?}");

        let request = SpawnRequest {
            argv: &argv,
            marker: None,
            lineage: None,
            child: None,
            claim_proposer: Some(proposer),
            voter: Some(other),
        };
        let verdict = judge(&request);
        assert!(!verdict.refused_layers().contains(&7), "{verdict:?}");
    }
}
