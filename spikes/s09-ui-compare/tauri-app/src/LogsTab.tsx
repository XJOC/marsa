import { api, type AppConfig } from "./api";
import { strings } from "./strings";
import { useBatchedList } from "./useBatchedList";

const ROW_HEIGHT = 18;

export function LogsTab({ config }: { config: AppConfig }) {
  const { scrollRef, virtualizer, items, cache } = useBatchedList<string>({
    name: "logs",
    count: config.log_lines,
    rowHeight: ROW_HEIGHT,
    batchSize: config.batch_size,
    fetchBatch: api.logLines,
    readySignal: "logs-ready",
  });
  return (
    <div ref={scrollRef} className="scroller">
      <div className="rows" style={{ height: virtualizer.getTotalSize() }}>
        {items.map((item) => (
          <div
            key={item.key}
            className="log-line"
            style={{ transform: `translateY(${item.start}px)` }}
          >
            {cache.get(item.index) ?? strings.loadingCell}
          </div>
        ))}
      </div>
    </div>
  );
}
