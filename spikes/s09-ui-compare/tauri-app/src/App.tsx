import { useCallback, useEffect, useState } from "react";
import { reportError, type AppConfig } from "./api";
import { runBench, type Tab } from "./bench";
import { LogsTab } from "./LogsTab";
import { SqlTab } from "./SqlTab";
import { strings } from "./strings";
import { TableTab } from "./TableTab";

const TABS: Tab[] = ["table", "sql", "logs"];

/**
 * A tab mounts the first time it opens and then stays mounted (hidden), so
 * the SQL editor keeps its text, like the egui app keeps its `String`.
 */
export function App({ config }: { config: AppConfig }) {
  const [tab, setTab] = useState<Tab>("table");
  const [opened, setOpened] = useState<ReadonlySet<Tab>>(() => new Set<Tab>(["table"]));

  const open = useCallback((next: Tab) => {
    setTab(next);
    setOpened((previous) =>
      previous.has(next) ? previous : new Set<Tab>([...previous, next]),
    );
  }, []);

  useEffect(() => {
    if (config.bench) runBench(config, open).catch((error: unknown) => reportError("bench", error));
  }, [config, open]);

  const panel = (name: Tab) => (tab === name ? "panel" : "panel hidden");

  return (
    <div className="app">
      <nav className="tabs">
        {TABS.map((name) => (
          <button
            key={name}
            type="button"
            className={name === tab ? "tab active" : "tab"}
            onClick={() => open(name)}
          >
            {strings.tabs[name]}
          </button>
        ))}
        {config.bench && <span className="badge">{strings.benchBadge}</span>}
        {config.with_tray && <span className="badge">{strings.trayBadge}</span>}
      </nav>
      <main>
        <section className={panel("table")}>
          <TableTab config={config} />
        </section>
        {opened.has("sql") && (
          <section className={panel("sql")}>
            <SqlTab />
          </section>
        )}
        {opened.has("logs") && (
          <section className={panel("logs")}>
            <LogsTab config={config} />
          </section>
        )}
      </main>
    </div>
  );
}
