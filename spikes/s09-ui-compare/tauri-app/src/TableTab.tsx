import { api, type AppConfig, type Row } from "./api";
import { strings } from "./strings";
import { useBatchedList } from "./useBatchedList";

/** Same row height as the egui table. */
const ROW_HEIGHT = 20;

export function TableTab({ config }: { config: AppConfig }) {
  const { scrollRef, virtualizer, items, cache } = useBatchedList<Row>({
    name: "table",
    count: config.table_rows,
    rowHeight: ROW_HEIGHT,
    batchSize: config.batch_size,
    fetchBatch: api.tableRows,
    readySignal: "table-ready",
  });
  return (
    <>
      <div className="table-row table-header">
        {config.table_columns.map((title) => (
          <div key={title} className="cell">
            {title}
          </div>
        ))}
      </div>
      <div ref={scrollRef} className="scroller">
        <div className="rows" style={{ height: virtualizer.getTotalSize() }}>
          {items.map((item) => {
            const row = cache.get(item.index);
            return (
              <div
                key={item.key}
                className={item.index % 2 === 1 ? "table-row odd" : "table-row"}
                style={{ transform: `translateY(${item.start}px)` }}
              >
                {row === undefined ? (
                  <div className="cell loading">{strings.loadingCell}</div>
                ) : (
                  row.map((cell, column) => (
                    <div key={column} className="cell">
                      {cell}
                    </div>
                  ))
                )}
              </div>
            );
          })}
        </div>
      </div>
    </>
  );
}
