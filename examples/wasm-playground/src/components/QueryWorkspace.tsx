import { Flex } from '@neo4j-ndl/react';

import type { ScriptRun } from '../lib/engine';
import { useIsMobile } from '../hooks/useIsMobile';
import { QueryPanel } from './QueryPanel';
import { ResultsPanel, type ViewTab } from './ResultsPanel';

interface Props {
  query: string;
  onQueryChange: (q: string) => void;
  onRun: (text: string) => void;
  engineReady: boolean;
  running: boolean;
  tab: ViewTab;
  onTabChange: (t: ViewTab) => void;
  run: ScriptRun | null;
  error: string | null;
  elapsed: number | null;
  lastRun: string | null;
  history: string[];
}

export function QueryWorkspace({
  query,
  onQueryChange,
  onRun,
  engineReady,
  running,
  tab,
  onTabChange,
  run,
  error,
  elapsed,
  lastRun,
  history,
}: Props) {
  const mobile = useIsMobile();
  return (
    <Flex
      flexDirection="column"
      gap="16"
      className="n-flex-1 n-min-w-0 n-min-h-0 n-w-full"
      style={{
        padding: mobile ? 10 : '10px 5px 10px 4px',
        boxSizing: 'border-box',
        overflow: mobile ? 'auto' : 'hidden',
        minHeight: 0,
        height: mobile ? '100%' : undefined,
      }}
    >
      <QueryPanel
        query={query}
        onQueryChange={onQueryChange}
        onRun={onRun}
        engineReady={engineReady}
        running={running}
        lastRun={lastRun}
        history={history}
      />
      <ResultsPanel
        tab={tab}
        onTabChange={onTabChange}
        run={run}
        error={error}
        elapsed={elapsed}
      />
    </Flex>
  );
}
