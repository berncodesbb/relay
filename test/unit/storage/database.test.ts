import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { openDatabase, type RelayDatabase } from '../../../src/storage/database.js';

let dir: string;
let open: RelayDatabase | undefined;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'relay-storage-db-'));
});

afterEach(() => {
  open?.close();
  open = undefined;
  rmSync(dir, { recursive: true, force: true });
});

describe('openDatabase', () => {
  it('creates missing parent directories', () => {
    const dbPath = join(dir, 'nested', 'deep', 'state.db');
    open = openDatabase(dbPath);
    expect(existsSync(dbPath)).toBe(true);
  });

  it('enables WAL journaling and creates WAL files next to the database', () => {
    const dbPath = join(dir, 'state.db');
    open = openDatabase(dbPath);
    open.raw.exec('CREATE TABLE probe (v TEXT NOT NULL);');
    open.raw.prepare('INSERT INTO probe (v) VALUES (?);').run('x');

    const mode = open.raw.prepare('PRAGMA journal_mode;').get() as { journal_mode: string };
    expect(mode.journal_mode).toBe('wal');
    expect(existsSync(`${dbPath}-wal`)).toBe(true);
    expect(existsSync(`${dbPath}-shm`)).toBe(true);
  });

  it('preserves written data across close and reopen', () => {
    const dbPath = join(dir, 'state.db');
    const first = openDatabase(dbPath);
    first.raw.exec('CREATE TABLE probe (v TEXT NOT NULL);');
    first.raw.prepare('INSERT INTO probe (v) VALUES (?);').run('durable');
    first.close();

    const reopened = openDatabase(dbPath);
    open = reopened;
    const row = reopened.raw.prepare('SELECT v FROM probe;').get() as { v: string };
    expect(row.v).toBe('durable');
  });
});
