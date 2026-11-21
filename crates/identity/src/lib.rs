//! The identity every Hanzo program on a machine shares: who a token is, the
//! credential vault it is filed in, the lock that serializes writers, and the
//! index naming the active identity. `hanzo` (CLI) and `hanzod` read the same
//! files through this crate; tokens themselves stay IAM's.

pub mod identity;
pub mod index;
pub mod lock;
pub mod token;
