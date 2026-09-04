//! Where supra credentials live in the OS keyring.
//!
//! The service name under which supra credentials are stored. A distinct service keeps supra's
//! entries from colliding with another application's, and gives the user a name they can
//! recognise in a keyring manager.
//!
//! This lives in `supra_config` rather than `supra_secrets` because it is the *config schema*
//! that names the service: `api_key_keyring` holds an account (the entry name), and the
//! service is the fixed other half of the `service/account` pair. The manager itself is
//! service-agnostic - it reads whatever pair it is asked for - so the constant belongs to the
//! crate that defines what the pair means.

/// The OS keyring service for supra provider credentials.
pub const KEYRING_SERVICE: &str = "supra-harness";
