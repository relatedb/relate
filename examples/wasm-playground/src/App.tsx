import { useCallback, useEffect, useMemo, useState } from 'react';
import { Flex } from '@neo4j-ndl/react';

import { engine, EnginePoisonedError, type ScriptRun } from './lib/engine';
import { resultHasGraph } from './lib/extract-graph';
import { PREVIEW_QUERY, SEED_DISPLAY_SCRIPT, SEED_PARAMS, SEED_SCRIPT } from './lib/seed';
import { canRunQuery } from './lib/query-eligibility';
import type { QueryStats } from './lib/wire';
import { CliSidebar, type CliEntry } from './components/CliSidebar';
import { FullScreenBanner } from './components/FullScreenBanner';
import { Header } from './components/Header';
import { QueryWorkspace } from './components/QueryWorkspace';
import { type ViewTab } from './components/ResultsPanel';
import { SchemaSidebar } from './components/SchemaSidebar';
import { WelcomeDialog } from './components/WelcomeDialog';
import { useIsMobile } from './hooks/useIsMobile';

const SCHEMA_KEYS: (keyof QueryStats)[] = [
  'nodes_created',
  'nodes_deleted',
  'relationships_created',
  'relationships_deleted',
  'labels_added',
  'labels_removed',
  'properties_set',
];

function queryChangedSchema(stats: QueryStats): boolean {
  return SCHEMA_KEYS.some((k) => (stats[k] ?? 0) > 0);
}

export function App() {
  const [query, setQuery] = useState('');
  const [run, setRun] = useState<ScriptRun | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const [running, setRunning] = useState(false);
  const [tab, setTab] = useState<ViewTab>('graph');
  const [engineReady, setEngineReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [poisoned, setPoisoned] = useState<EnginePoisonedError | null>(null);
  const [schemaRefreshKey, setSchemaRefreshKey] = useState(0);
  const [resetting, setResetting] = useState(false);
  const [cliHistory, setCliHistory] = useState<CliEntry[]>([]);

  // Everything that describes the database's contents goes stale when it is
  // reset: the Reset button calls this right after the engine resets,
  // whatever happens next.
  const afterDatabaseReset = useCallback(() => {
    setCliHistory([]);
    setRun(null);
    setElapsed(null);
    setError(null);
    setQuery('');
    setSchemaRefreshKey((k) => k + 1);
  }, []);

  const [welcomeOpen, setWelcomeOpen] = useState(true);
  const [seeding, setSeeding] = useState(false);
  const isMobile = useIsMobile();
  const [mobileTab, setMobileTab] = useState<'console' | 'schema' | 'query'>('console');

  useEffect(() => {
    let cancelled = false;
    engine
      .ready()
      .then(() => !cancelled && setEngineReady(true))
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof EnginePoisonedError) setPoisoned(e);
        else setLoadError((e as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const echoInput = useCallback((text: string) => {
    setCliHistory((h) => [...h, { kind: 'input', text: text.trimEnd() }]);
  }, []);

  /** Run `q` with `paramsJson` (a JSON object as text, or null). Call sites
   * are expected to have already called `echoInput` for the user-visible
   * input text; `runQuery` only handles execution and the output/error reply. */
  const runQuery = useCallback(async (q: string, paramsJson: string | null) => {
    if (!canRunQuery(engineReady, running, q)) return false;
    // Terminal & editor share `query` as one draft: clear both after running so
    // the next prompt starts fresh. Past inputs remain in the terminal history
    // and are clickable to restore.
    setQuery('');
    setRunning(true);
    setError(null);
    const t0 = performance.now();
    try {
      const r = await engine.runScript(q, paramsJson);
      const took = performance.now() - t0;
      setElapsed(took);
      // Statements before a failing one stay applied, so the schema can
      // change either way.
      if (r && queryChangedSchema(r.stats)) setSchemaRefreshKey((k) => k + 1);
      if (r === null) {
        setCliHistory((h) => [
          ...h,
          { kind: 'output', text: 'Nothing to run: the input holds only comments.' },
          { kind: 'output', text: '' },
        ]);
        return false;
      } else if (r.last.error !== null) {
        const message = r.statements > 1 ? `Statement ${r.statements}: ${r.last.error}` : r.last.error;
        setError(message);
        setRun(null);
        setCliHistory((h) => [...h, { kind: 'error', text: message }, { kind: 'output', text: '' }]);
        return false;
      } else {
        const rows = r.last.rows.length;
        setRun(r);
        setTab(resultHasGraph(r.last) ? 'graph' : 'table');
        setCliHistory((h) => [
          ...h,
          { kind: 'output', text: `${rows} record${rows === 1 ? '' : 's'} · ${took.toFixed(1)} ms` },
          { kind: 'output', text: '' },
        ]);
        return true;
      }
    } catch (e) {
      if (e instanceof EnginePoisonedError) setPoisoned(e);
      else {
        const msg = (e as Error).message;
        setError(msg);
        setCliHistory((h) => [...h, { kind: 'error', text: msg }, { kind: 'output', text: '' }]);
      }
      setRun(null);
      return false;
    } finally {
      setRunning(false);
    }
  }, [engineReady, running]);

  // The welcome dialog runs the seed. The LOAD CSV teaser is echoed into the
  // terminal, then the equivalent UNWIND seed actually runs; the browser can't
  // execute LOAD CSV so showing one and running the other is the whole point.
  const runSeed = useCallback(async () => {
    setSeeding(true);
    try {
      echoInput(SEED_DISPLAY_SCRIPT);
      if (!(await runQuery(SEED_SCRIPT, SEED_PARAMS))) return;
      echoInput(PREVIEW_QUERY);
      if (!(await runQuery(PREVIEW_QUERY, null))) return;
      setWelcomeOpen(false);
    } finally {
      setSeeding(false);
    }
  }, [runQuery, echoInput]);

  const resetDatabase = useCallback(async () => {
    setResetting(true);
    setError(null);
    try {
      await engine.reset();
      afterDatabaseReset();
    } catch (e) {
      if (e instanceof EnginePoisonedError) setPoisoned(e);
      else setError((e as Error).message);
    } finally {
      setResetting(false);
    }
  }, [afterDatabaseReset]);

  if (loadError) {
    return (
      <FullScreenBanner
        title="Failed to load Relate WASM"
        message={loadError}
        hint="Run make build-wasm (or npm run build:wasm), then reload."
      />
    );
  }

  if (poisoned) {
    return (
      <FullScreenBanner
        title="Relate wasm module crashed"
        message={poisoned.cause.message}
        hint="The engine trapped and cannot be reused safely. Reload the page. Please file the failing query as a bug."
        onReload={() => window.location.reload()}
      />
    );
  }

  const runFromEditor = (text: string) => {
    echoInput(text);
    void runQuery(text, null);
  };

  const runFromSchema = (q: string) => {
    runFromEditor(q);
    if (isMobile) setMobileTab('query');
  };

  // Ghost-text in the editor after a run: the most recent input in history.
  const lastRun = [...cliHistory].reverse().find((e) => e.kind === 'input')?.text ?? null;

  // CypherEditor's REPL history (up/down arrows) wants newest-first.
  const editorHistory = useMemo(
    () =>
      cliHistory
        .filter((e): e is { kind: 'input'; text: string } => e.kind === 'input')
        .map((e) => e.text)
        .reverse(),
    [cliHistory],
  );

  const schema = (
    <SchemaSidebar
      engineReady={engineReady}
      refreshKey={schemaRefreshKey}
      onRunQuery={runFromSchema}
    />
  );
  const workspace = (
    <QueryWorkspace
      query={query}
      onQueryChange={setQuery}
      history={editorHistory}
      onRun={runFromEditor}
      engineReady={engineReady}
      running={running}
      tab={tab}
      onTabChange={setTab}
      run={run}
      error={error}
      elapsed={elapsed}
      lastRun={lastRun}
    />
  );
  const cliSidebar = (
    <CliSidebar
      history={cliHistory}
      engineReady={engineReady}
      running={running}
      draft={query}
      onDraftChange={setQuery}
      onSubmit={runFromEditor}
      onSwitchToQuery={() => setMobileTab('query')}
    />
  );

  return (
    <Flex
      flexDirection="row"
      className="n-h-full n-w-full n-bg-neutral-bg-default n-text-neutral-text-default"
      style={{ overflow: 'hidden', gap: 0 }}
    >
      <Flex
        flexDirection="column"
        gap="2"
        className="n-flex-1 n-min-w-0 n-min-h-0"
        style={{ overflow: 'hidden' }}
      >
        <Header
          engineReady={engineReady}
          resetting={resetting}
          onReset={() => void resetDatabase()}
        />

        {isMobile ? (
          <Flex
            flexDirection="column"
            className="n-flex-1 n-min-h-0"
            style={{ overflow: 'hidden', minHeight: 0 }}
          >
            <MobileTabs value={mobileTab} onChange={setMobileTab} />
            <div style={{ flex: 1, minHeight: 0, overflow: 'hidden', position: 'relative' }}>
              <MobilePane visible={mobileTab === 'console'}>{cliSidebar}</MobilePane>
              <MobilePane visible={mobileTab === 'schema'}>{schema}</MobilePane>
              <MobilePane visible={mobileTab === 'query'}>{workspace}</MobilePane>
            </div>
          </Flex>
        ) : (
          <Flex
            flexDirection="row"
            className="n-flex-1 n-min-h-0"
            style={{ overflow: 'hidden', minHeight: 0, gap: 0 }}
          >
            {schema}
            {workspace}
          </Flex>
        )}
      </Flex>

      {!isMobile && cliSidebar}

      <WelcomeDialog
        isOpen={welcomeOpen}
        seedScript={SEED_DISPLAY_SCRIPT}
        onRunSeed={() => void runSeed()}
        isRunning={seeding}
        canRun={engineReady}
      />
    </Flex>
  );
}

function MobilePane({ visible, children }: { visible: boolean; children: React.ReactNode }) {
  return (
    <div
      style={{
        position: 'absolute',
        inset: 0,
        display: visible ? 'block' : 'none',
        overflow: 'hidden',
      }}
    >
      {children}
    </div>
  );
}

function MobileTabs({
  value,
  onChange,
}: {
  value: 'console' | 'schema' | 'query';
  onChange: (v: 'console' | 'schema' | 'query') => void;
}) {
  const tabs: { id: 'console' | 'schema' | 'query'; label: string }[] = [
    { id: 'console', label: 'Terminal' },
    { id: 'schema', label: 'Database' },
    { id: 'query', label: 'Query' },
  ];
  return (
    <div
      role="tablist"
      className="n-shrink-0 n-border-b n-border-neutral-border-weak n-bg-neutral-bg-weak"
      style={{ display: 'flex' }}
    >
      {tabs.map((t) => {
        const active = t.id === value;
        return (
          <button
            key={t.id}
            role="tab"
            aria-selected={active}
            onClick={() => onChange(t.id)}
            style={{
              flex: 1,
              minHeight: 44,
              padding: '10px 12px',
              background: 'transparent',
              border: 'none',
              borderBottom: active
                ? '2px solid var(--theme-color-primary-border-strong)'
                : '2px solid transparent',
              color: active
                ? 'var(--theme-color-neutral-text-default)'
                : 'var(--theme-color-neutral-text-weak)',
              font: 'inherit',
              fontWeight: active ? 600 : 400,
              cursor: 'pointer',
            }}
          >
            {t.label}
          </button>
        );
      })}
    </div>
  );
}
