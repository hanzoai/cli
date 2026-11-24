// The cloud product tree (generated) + the shared seams.
pub mod host;
pub mod launch;
pub mod link;
pub mod product;
pub mod term;

// Resource-noun commands — the primary `hanzo <resource> <verb>` tree.
pub mod auth;
pub mod bot;
pub mod build;
pub mod chain;
pub mod channels;
pub mod config;
pub mod doctor;
pub mod engine;
pub mod graph;
pub mod man;
pub mod monitor;
pub mod runner;
pub mod savings;
pub mod scan;
pub mod serve;
pub mod status;
pub mod up;
pub mod upgrade;
pub mod version;

// The one coding/agent orchestrator, behind `hanzo dev` and `hanzo desktop`.
pub mod cloud_sessions;
pub mod daemon;
pub mod dev;
pub mod dev_server;
pub mod prompt;
pub mod sources;

// Kept resources reachable in the additive model (money, network, provider
// connectors, local dev helpers, and the TS SDK proxies).
pub mod init;
pub mod k8s;
pub mod network;
pub mod share;
pub mod shell;
pub mod vm;
pub mod wallet;
