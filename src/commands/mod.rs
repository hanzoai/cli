// The cloud product tree (generated) + the shared seams.
pub mod cloud;
pub mod host;
pub mod launch;
pub mod product;

// Resource-noun commands — the primary `hanzo <resource> <verb>` tree.
pub mod agent;
pub mod auth;
pub mod config;
pub mod fabric;
pub mod engine;
pub mod runner;
pub mod scan;
pub mod serve;

// The coding/agent orchestrator behind `agent run`.
pub mod code;

// Kept resources reachable in the additive model (money, network, provider
// connectors, local dev helpers, and the TS SDK proxies).
pub mod billing;
pub mod connector;
pub mod init;
pub mod network;
pub mod share;
pub mod wallet;
