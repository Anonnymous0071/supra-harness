//! Why a spawn was refused, and which layer refused it.
//!
//! The variants name layers, not causes: L1 and L2 refuse identity problems, L3 refuses a
//! command that names this binary, L4 refuses a file that *is* this binary, L5 refuses a
//! marker that does not authenticate, L6 refuses a lineage that cycles or nests, and L7
//! refuses a proposer voting on its own claim. A caller that needs the cause reads the
//! message; a caller that needs the layer reads the variant.

use thiserror::Error;

use supra_types::AgentId;

/// A refused spawn attempt.
///
/// Each variant names its layer in the message, so a log line says which check fired
/// without the reader consulting a table.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum Refusal {
    /// Identity (L1) or the marker key (L2) is not established: the guard was asked to judge
    /// a spawn before it knew who it is or could mint a tag. Fail-closed, always. The detail
    /// names which half is missing; [`crate::layer_of`] maps it back to 1 or 2.
    #[error("L1/L2: the guard is not ready ({detail}); refusing spawn (establish it first)")]
    NoIdentity {
        /// Which half is missing, for the message.
        detail: String,
    },

    /// The command names this binary by path, argv[0], or resolved identity.
    #[error("L3: the command names this binary ({detail})")]
    OwnBinary {
        /// Which spelling matched, for the message.
        detail: String,
    },
    /// The file the command resolves to has this process's own (device, inode).
    #[error("L4: the command resolves to this binary (device and inode match)")]
    SameFile,

    /// `SUPRA_SPAWN` is absent or does not authenticate.
    #[error("L5: the spawn marker does not authenticate ({detail})")]
    BadMarker {
        /// What was wrong, without revealing the key.
        detail: String,
    },

    /// The lineage would cycle or nest.
    #[error("L6: the lineage refuses this spawn ({detail})")]
    BadLineage {
        /// The underlying lineage error, rendered.
        detail: String,
    },

    /// The proposer is voting on its own claim.
    #[error("L7: agent {voter} may not vote on its own claim (proposer {proposer})")]
    SelfVote {
        /// Who is voting.
        voter: AgentId,
        /// Who proposed the claim.
        proposer: AgentId,
    },

    /// The OS refused entropy, so the marker key or nonce cannot be minted safely.
    ///
    /// Returned, not panicked: startup decides whether an unkeyed guard may run degraded,
    /// and a library that panics on a kernel state takes that decision away from it.
    /// Distinct from `NoIdentity` because the remedy differs - establishing identity again
    /// will not help when the kernel reports no entropy. A zeroed key would authenticate
    /// nothing, and a predictable nonce would make markers replayable.
    #[error("the OS refused to provide random bytes ({at}); cannot key the marker safely")]
    NoEntropy {
        /// Which step needed randomness: key generation or marker issuance.
        at: &'static str,
    },
}
