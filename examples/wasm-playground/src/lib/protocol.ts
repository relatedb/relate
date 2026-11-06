import type { QueryStats, WireResult } from './wire';

/**
 * A script's run: its last statement's result, the mutation counters summed
 * over every statement (by `renderResult`), and how many statements ran.
 */
export interface ScriptRun {
  last: WireResult;
  stats: QueryStats;
  statements: number;
}

export type Request =
  | { type: 'init' }
  | { type: 'memory' }
  | { type: 'reset' }
  | { type: 'execute'; query: string; params: string | null }
  | { type: 'run'; code: string; params: string | null };

/** A page request plus the id the page uses to match the reply. */
export type Inbound = Request & { id: number };

export type Ok =
  | { id: number; ok: true; type: 'init' }
  | { id: number; ok: true; type: 'reset' }
  | { id: number; ok: true; type: 'memory'; bytes: number }
  | { id: number; ok: true; type: 'execute'; raw: string }
  | { id: number; ok: true; type: 'run'; result: ScriptRun | null };

export type Err = { id: number; ok: false; trap: boolean; message: string };

export type Response = Ok | Err;

export type OkFor<T extends Request['type']> = Extract<Ok, { type: T }>;
