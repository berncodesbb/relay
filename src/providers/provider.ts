/**
 * Provider-neutral model interface.
 *
 * This module is the only contract between the runtime and model providers.
 * It contains no SDK types. Adapters (`openai.ts`, `anthropic.ts`, `fake.ts`)
 * translate SDK responses into these Relay-owned types and must not let SDK
 * objects cross their module boundary.
 */

import type { TokenUsage } from '../core/events.js';

export type { TokenUsage };

export type JsonSchemaObject = {
  readonly [key: string]: unknown;
};

export type ToolSpec = {
  readonly name: string;
  readonly description: string;
  /** JSON Schema for the tool arguments. */
  readonly parameters: JsonSchemaObject;
};

export type ToolCall = {
  readonly id: string;
  readonly name: string;
  readonly arguments: unknown;
};

export type AssistantContent =
  | { readonly type: 'text'; readonly text: string }
  | { readonly type: 'tool-call'; readonly call: ToolCall };

export type ModelMessage =
  | { readonly role: 'user'; readonly content: string }
  | { readonly role: 'assistant'; readonly content: readonly AssistantContent[] }
  | { readonly role: 'tool-result'; readonly toolCallId: string; readonly content: string };

export type FinishReason = 'stop' | 'tool-calls' | 'length' | 'canceled' | 'error';

export type StructuredOutputRequest = {
  /** Logical name of the structured output, e.g. "intent-proposal". */
  readonly name: string;
  /** JSON Schema describing the expected object. */
  readonly schema: JsonSchemaObject;
};

export type ModelRequest = {
  readonly model: string;
  readonly system?: string | undefined;
  readonly messages: readonly ModelMessage[];
  readonly tools?: readonly ToolSpec[] | undefined;
  readonly structuredOutput?: StructuredOutputRequest | undefined;
  readonly maxOutputTokens?: number | undefined;
  readonly temperature?: number | undefined;
  /** Logical purpose (e.g. "mode:discovery", "intent") used for routing logs and fixtures. */
  readonly purpose?: string | undefined;
  readonly signal?: AbortSignal | undefined;
};

export type ProviderStreamEvent =
  | { readonly type: 'text-delta'; readonly text: string }
  | { readonly type: 'tool-call'; readonly call: ToolCall }
  | { readonly type: 'usage'; readonly usage: TokenUsage }
  | { readonly type: 'structured-output'; readonly name: string; readonly value: unknown }
  | { readonly type: 'completed'; readonly finishReason: FinishReason; readonly usage: TokenUsage }
  | { readonly type: 'error'; readonly error: ProviderFailure };

export type ProviderErrorKind =
  | 'rate-limit'
  | 'server'
  | 'network'
  | 'timeout'
  | 'canceled'
  | 'auth'
  | 'invalid-response'
  | 'unknown';

export class ProviderFailure extends Error {
  constructor(
    readonly kind: ProviderErrorKind,
    message: string,
    readonly retryable: boolean = false,
    readonly status?: number | undefined,
  ) {
    super(message);
    this.name = 'ProviderFailure';
  }
}

export type ProviderName = 'openai' | 'anthropic' | 'fake';

/**
 * A model provider returns an async stream of Relay-owned events. Relay's
 * event log and context builder remain the authoritative conversation record;
 * provider-side conversation ids are optional optimizations adapters may
 * expose via `conversationHint`.
 */
export interface ModelProvider {
  readonly name: ProviderName;
  /** Model ids this provider reports for configuration validation. */
  readonly models: readonly string[];
  stream(request: ModelRequest): AsyncIterable<ProviderStreamEvent>;
}

/** Aggregated result of one model turn, produced from a stream. */
export type ModelTurnResult = {
  readonly text: string;
  readonly toolCalls: readonly ToolCall[];
  readonly structuredOutput: { readonly name: string; readonly value: unknown } | null;
  readonly finishReason: FinishReason;
  readonly usage: TokenUsage;
};
