// The cloud product tree (generated) + the shared seams.
pub mod host;
pub mod launch;
pub mod link;
pub mod term;
pub mod product;

// Resource-noun commands — the primary `hanzo <resource> <verb>` tree.
pub mod man;
pub mod auth;
pub mod config;
pub mod chain;
pub mod engine;
pub mod runner;
pub mod scan;
pub mod status;
pub mod version;
pub mod up;

// The one coding/agent orchestrator, behind `hanzo code` and `hanzo desktop`.
pub mod code;

// Kept resources reachable in the additive model (money, network, provider
// connectors, local dev helpers, and the TS SDK proxies).
pub mod init;
pub mod network;
pub mod share;
pub mod wallet;
