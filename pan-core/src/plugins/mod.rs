//! # Wave 1–5 plugins.
//!
//! Wave 1 plugins form the walking skeleton — exec.local, gov.allow, state.memory,
//! obs.logging. Wave 2+ add capabilities (cap.fs, cap.http, cap.mcp), governance
//! (gov.policy, gov.secrets), durable state (state.file), scheduling (sched.cron,
//! sched.eventbus), skill execution (skill.runner), memory (memory.vector),
//! sandboxed execution (exec.docker), context assembly (context.template,
//! context.history), and admission filtering (obs.admission).

pub mod cap_fs;
pub mod cap_http;
pub mod cap_mcp;
pub mod context_history;
pub mod context_template;
pub mod exec_docker;
pub mod exec_local;
pub mod gov_allow;
pub mod gov_policy;
pub mod gov_secrets;
pub mod memory_vector;
pub mod obs_admission;
pub mod obs_logging;
pub mod sched_cron;
pub mod sched_eventbus;
pub mod skill_runner;
pub mod state_file;
pub mod state_memory;
