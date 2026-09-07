//! What a shell session refused, and who must act.
//!
//! The split mirrors T16's: a refusal at the sandbox boundary is the
//! *caller's* problem (it asked for something the platform or the policy
//! will not do), while a refusal inside the pty itself is the *session's*
//! (the master is gone, or the descriptor broke). `NotRunning` is neither:
//! it is a misuse the caller can fix in one line, so it names itself.

use thiserror::Error;

/// A shell-session failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShellError {
    /// The sandbox refused the spawn. The message carries the refusal shape
    /// (authority, consent, leaky descriptor, unsupported platform, or the
    /// C side's own detail); the caller shows it and does not retry unless
    /// the user changed something.
    #[error("the sandbox refused the shell: {0}")]
    Spawn(#[from] supra_sandbox::SandboxError),

    /// The pty itself failed: the master could not be read or written, or
    /// the resize ioctl was refused. The child may still be running; the
    /// session kills it on drop, which is the fail-closed end of the story.
    #[error("the pty failed: {0}")]
    Pty(#[from] std::io::Error),

    /// The child's sandbox handle failed - wait or kill on a broken process.
    /// Distinct from [`ShellError::Spawn`]: the child *ran*, and the failure
    /// is in the accounting around it.
    #[error("the child's process handle failed: {0}")]
    Child(#[from] supra_ffi::sandbox::Error),

    /// The caller asked a session with no running child to wait or kill.
    ///
    /// `wait` on an exited session returns its exit code and leaves the
    /// child `None`; asking again is the misuse this names.
    #[error("the session has no running child")]
    NotRunning,
}
