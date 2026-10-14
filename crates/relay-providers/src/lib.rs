//! Provider-neutral model contracts and adapters for Relay.
//!
//! This crate is the only contract between the runtime and model providers,
//! ported from the retired TypeScript implementation in `src/providers/**`
//! (removed from the tree 2026-10-14).
//! It contains no provider SDK types: the neutral contract in [`contract`]
//! describes requests, stream events, usage, finish reasons, and normalized
//! failures, and the adapters ([`fake`], [`openai`], [`anthropic`]) translate
//! Relay-owned requests into provider wire calls and normalize responses back.
//! Provider wire types stay private to their adapter module and never appear
//! in a public signature.
//!
//! Errors are bounded and redacted: messages are trimmed to 300 characters and
//! credential-shaped substrings are replaced before any failure is returned
//! (see [`errors`]). API keys are read from the environment or passed in by
//! the caller and are never logged, echoed, or embedded in errors.
//!
//! All tests in this crate are offline and deterministic: HTTP conformance
//! tests replay recorded SSE fixtures through a loopback-only mock server.

pub mod anthropic;
pub mod collect;
pub mod contract;
pub mod errors;
pub mod fake;
pub mod openai;
mod sse;

pub use collect::collect_turn;
pub use contract::{
    AssistantContent, CancelToken, FinishReason, JsonSchemaObject, ModelMessage, ModelProvider,
    ModelRequest, ModelTurnResult, ProviderErrorKind, ProviderEventStream, ProviderFailure,
    ProviderName, ProviderStreamEvent, StructuredOutput, StructuredOutputRequest, ToolCall,
    ToolSpec,
};
