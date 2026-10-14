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
//! Adapter identity is which *protocol* a connection speaks
//! ([`AdapterKind`]: `openai-chat`, `anthropic`, `fake`), never which vendor
//! it reaches; the user-facing *connection* identity is a configurable name
//! (`providers.<name>`) owned by Relay's configuration, not this crate. Any
//! number of connections can share one adapter — several `openai-chat`
//! endpoints (OpenAI, GLM, local gateways) coexist as separate
//! [`openai::OpenAIProvider`] instances with their own base URL, credential
//! variable name, and effective capability profile.
//!
//! Each adapter enforces its connection's [`AdapterCapabilities`] profile
//! (adapter defaults, then an optional preset such as `glm`, then explicit
//! overrides, resolved with [`resolve_capabilities`] before construction) as
//! a preflight before any HTTP request: a requested Relay feature is either
//! translated by a documented rule or rejected with an actionable error,
//! never silently downgraded.
//!
//! Errors are bounded and redacted: messages are trimmed to 300 characters and
//! credential-shaped substrings are replaced before any failure is returned
//! (see [`errors`]). API keys are read from the environment (through the
//! connection's configured variable name) or passed in by the caller and are
//! never logged, echoed, or embedded in errors.
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
    ADAPTER_KINDS, AdapterCapabilities, AdapterKind, AssistantContent, CancelToken,
    CapabilityError, CapabilityOverrides, FeatureSupport, FinishReason, InstructionStyle,
    JsonSchemaObject, ModelMessage, ModelProvider, ModelRequest, ModelTurnResult, OutputTokenParam,
    PRESET_NAMES, ProviderErrorKind, ProviderEventStream, ProviderFailure, ProviderStreamEvent,
    StructuredOutput, StructuredOutputMode, StructuredOutputRequest, ToolCall, ToolSpec,
    preset_adapter, preset_capabilities, resolve_capabilities,
};
