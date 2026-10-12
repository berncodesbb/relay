/**
 * Cooperative cancellation token.
 *
 * A thin wrapper over `AbortController` that carries a human-readable reason
 * and lets long-running code poll (`throwIfCanceled`), await a signal
 * (`signal`), or register one-shot callbacks. Library code never listens for
 * process signals directly; the CLI decides when a run is canceled.
 */

export class OperationCanceledError extends Error {
  constructor(readonly reason: string) {
    super(`Operation canceled: ${reason}`);
    this.name = 'OperationCanceledError';
  }
}

export class CancellationToken {
  private readonly controller: AbortController;
  private stateReason: string | null = null;

  private constructor(controller: AbortController) {
    this.controller = controller;
  }

  /** Create a fresh token together with its cancel function. */
  static create(): { token: CancellationToken; cancel: (reason?: string) => void } {
    const controller = new AbortController();
    const token = new CancellationToken(controller);
    const cancel = (reason?: string): void => {
      token.cancel(reason);
    };
    return { token, cancel };
  }

  /** True once cancel() has been called. */
  get canceled(): boolean {
    return this.controller.signal.aborted;
  }

  /** The cancellation reason; defaults to "canceled" after cancel(). */
  get reason(): string | null {
    if (!this.canceled) return null;
    return this.stateReason ?? 'canceled';
  }

  /** Underlying AbortSignal for APIs that accept one. */
  get signal(): AbortSignal {
    return this.controller.signal;
  }

  /** Throw `OperationCanceledError` if the token has been canceled. */
  throwIfCanceled(): void {
    if (this.canceled) {
      throw new OperationCanceledError(this.reason ?? 'canceled');
    }
  }

  /**
   * Register a callback fired when cancellation happens. Returns a dispose
   * function; calling the callback immediately (once) if already canceled.
   */
  onCancel(callback: () => void): () => void {
    if (this.canceled) {
      callback();
      return () => undefined;
    }
    this.controller.signal.addEventListener('abort', callback, { once: true });
    return () => {
      this.controller.signal.removeEventListener('abort', callback);
    };
  }

  private cancel(reason?: string): void {
    if (this.canceled) return; // first reason wins
    this.stateReason = reason ?? 'canceled';
    this.controller.abort(this.stateReason);
  }
}
