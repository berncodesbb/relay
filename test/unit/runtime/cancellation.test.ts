import { describe, expect, it } from 'vitest';
import { CancellationToken, OperationCanceledError } from '../../../src/runtime/cancellation.js';

describe('CancellationToken', () => {
  it('starts un-canceled with a null reason', () => {
    const { token } = CancellationToken.create();
    expect(token.canceled).toBe(false);
    expect(token.reason).toBeNull();
    expect(() => token.throwIfCanceled()).not.toThrow();
    expect(token.signal.aborted).toBe(false);
  });

  it('records the cancellation reason and throws on demand', () => {
    const { token, cancel } = CancellationToken.create();
    cancel('user pressed ctrl-c');
    expect(token.canceled).toBe(true);
    expect(token.reason).toBe('user pressed ctrl-c');
    expect(token.signal.aborted).toBe(true);
    expect(() => token.throwIfCanceled()).toThrowError(OperationCanceledError);
    expect(() => token.throwIfCanceled()).toThrowError(/user pressed ctrl-c/);
  });

  it('defaults the reason to "canceled"', () => {
    const { token, cancel } = CancellationToken.create();
    cancel();
    expect(token.reason).toBe('canceled');
  });

  it('keeps the first reason on repeated cancel calls', () => {
    const { token, cancel } = CancellationToken.create();
    cancel('first');
    cancel('second');
    expect(token.reason).toBe('first');
  });

  it('fires onCancel callbacks once and supports disposal', () => {
    const { token, cancel } = CancellationToken.create();
    let calls = 0;
    const dispose = token.onCancel(() => {
      calls += 1;
    });
    dispose();
    cancel('now');
    expect(calls).toBe(0);

    let lateCalls = 0;
    token.onCancel(() => {
      lateCalls += 1;
    });
    expect(lateCalls).toBe(1); // already canceled: fires immediately
  });
});
