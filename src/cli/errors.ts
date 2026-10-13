/**
 * Central mapping from thrown errors to CLI exit codes and user-facing text.
 *
 * Errors are detected by `name` rather than `instanceof` so classification
 * keeps working across dynamic-import boundaries. Every command funnels its
 * failures through {@link describeCliError} so exit codes stay consistent:
 * 0 success, 1 runtime failure, 2 usage or configuration error.
 */

export type CliErrorReport = {
  readonly message: string;
  readonly code: 0 | 1 | 2;
  readonly hint?: string | undefined;
};

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : '';
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  return String(error);
}

/**
 * Map any thrown value onto a printable message, an exit code, and an
 * optional actionable hint.
 */
export function describeCliError(error: unknown): CliErrorReport {
  const message = errorMessage(error);
  switch (errorName(error)) {
    case 'ConfigValidationError':
      return {
        message,
        code: 2,
        hint: 'Run `relay config path` to see which configuration files are loaded.',
      };
    case 'UsageError':
      return { message, code: 2, hint: 'Run relay --help for usage.' };
    case 'UnknownModelRoleError':
      return { message, code: 2, hint: 'Run `relay config show` to list configured model roles.' };
    case 'ProjectNotFoundError':
      return { message, code: 1, hint: 'Run relay init first.' };
    case 'NotInitializedError':
      return { message, code: 1, hint: 'Run relay init first (or relay run "<goal>").' };
    case 'ProjectAlreadyInitializedError':
      return { message, code: 1 };
    case 'ProviderFailure': {
      const kind = (error as { kind?: unknown }).kind;
      return {
        message,
        code: 1,
        hint:
          kind === 'auth'
            ? 'Check the provider API key environment variable (OPENAI_API_KEY / ANTHROPIC_API_KEY).'
            : 'Workflow state is resumable; fix the issue and rerun `relay run`.',
      };
    }
    case 'RouteUnavailableError':
      return {
        message,
        code: 1,
        hint: 'Set OPENAI_API_KEY / ANTHROPIC_API_KEY or pass --provider fake.',
      };
    case 'PolicyRejectionError':
      return { message, code: 1 };
    default:
      return { message: `relay: ${message}`, code: 1 };
  }
}
