//! A token-frugal JIRA Cloud client, its compact renderers, and the MCP server built on top.
//!
//! The binary (`src/main.rs`) is a thin wrapper: [`config::Config::load`] -> [`client::JiraClient`]
//! -> [`server::JiraMcp`] over stdio.
//!
//! # Embedding
//!
//! Other MCP servers can reuse the parts without inheriting this server's tool surface — which
//! matters when the host mediates what an agent may reach (read-only, no create/transition, …).
//! Take the client and the renderers, keep your own tools:
//!
//! ```no_run
//! use ujira::{Access, Config, JiraClient, render};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // `from_env` is strict (all three vars or nothing), for a host that injects credentials
//! // server-side; `Config::load` adds the on-disk fallbacks a human install wants.
//! let Some(mut cfg) = Config::from_env() else {
//!     return Ok(()); // JIRA not configured: report `disabled` rather than failing
//! };
//! cfg.access = Access::ReadComment; // this host's agent may read and comment, nothing more
//! let jira = JiraClient::new(cfg);
//! let issue = jira.get_issue("PROJ-1", false).await?;
//! println!("{}", render::issue(&issue, jira.config(), 6000));
//! # Ok(())
//! # }
//! ```
//!
//! To expose this crate's full sixteen-tool surface instead, serve [`server::JiraMcp`] directly.
//!
//! # Instrumentation
//!
//! Every client call, op, and MCP tool opens a `debug` [`tracing`] span carrying its identifiers
//! (issue key, JQL, limits) and never the token or prose bodies. The HTTP boundary emits a `debug`
//! event per request and response, a `warn` on a non-2xx or an access-level refusal, and config
//! resolution reports which token source won. The library installs no subscriber; the host decides
//! where, and whether, any of it goes.

pub mod adf;
pub mod client;
pub mod config;
pub mod fields;
pub mod keychain;
pub mod ops;
pub mod render;
pub mod server;

pub use client::JiraClient;
pub use config::{Access, Config};
pub use server::JiraMcp;
