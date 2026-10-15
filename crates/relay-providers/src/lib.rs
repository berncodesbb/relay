//! Provider-neutral model contracts and adapters for Relay.
//!
//! This crate is the only contract between the runtime and model providers,
//! ported from the retired TypeScript implementation in `src/providers/**`
//! (removed from the tree 2026-10-14).
//! It contains no provider SDK types: the neutral contract in [`contract`]
//! describes ordered content-block transcripts, requests, stream events,
//! usage, finish reasons, and normalized failures, and the adapters
//! ([`fake`], [`anthropic`]) translate
//! Relay-owned requests into provider wire calls and normalize responses back.
//! Provider wire types stay private to their adapter module and never appear
//! in a public signature.
//!
//! Anthropic Messages is the sole live external protocol: [`anthropic`] is
//! the only adapter that speaks HTTP, and it is deliberately
//! protocol-compatible rather than vendor-specific, so any
//! Anthropic-Messages-compatible endpoint works as a configured connection.
//! [`fake`] is the deterministic in-process provider used for offline tests
//! and as the default route. The historical `openai-chat` adapter was
//! intentionally removed; configuration carrying that spelling fails at the
//! configuration boundary with guidance to configure an Anthropic
//! Messages-compatible connection instead.
//!
//! Adapter identity is which *protocol* a connection speaks
//! ([`AdapterKind`]: `anthropic`, `fake`), never which vendor it reaches;
//! the user-facing *connection* identity is a configurable name
//! (`providers.<name>`) owned by Relay's configuration, not this crate. Any
//! number of connections can share one adapter — several
//! Anthropic-Messages-compatible endpoints coexist as separate
//! [`anthropic::AnthropicProvider`] instances with their own base URL,
//! credential variable name, and effective capability profile.
//!
//! Each adapter enforces its connection's [`AdapterCapabilities`] profile
//! (adapter defaults, then an optional preset, then explicit overrides,
//! resolved with [`resolve_capabilities`] before construction) as a preflight
//! before any HTTP request: a requested Relay feature is either translated
//! by a documented rule or rejected with an actionable error, never silently
//! downgraded.
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
mod sse;

pub use collect::collect_turn;
pub use contract::{
    ADAPTER_KINDS, AdapterCapabilities, AdapterKind, AssistantContent, CancelToken,
    CapabilityError, CapabilityOverrides, FeatureSupport, FinishReason, InstructionStyle,
    JsonSchemaObject, MAX_CONTINUATION_DATA_BYTES, MAX_CONTINUATION_DISCRIMINATOR_CHARS,
    MAX_TEXT_BLOCK_BYTES, MAX_TOOL_RESULT_CONTENT_BYTES, ModelMessage, ModelProvider, ModelRequest,
    ModelTurnResult, OutputTokenParam, PRESET_NAMES, ProviderErrorKind, ProviderEventStream,
    ProviderFailure, ProviderStreamEvent, StructuredOutput, StructuredOutputMode,
    StructuredOutputRequest, ToolCall, ToolSpec, UserContent, preset_adapter, preset_capabilities,
    resolve_capabilities,
};
