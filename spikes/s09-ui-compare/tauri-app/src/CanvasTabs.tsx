// S-09b: the Table and Logs tabs drawn on a canvas with
// `@glideapps/glide-data-grid`. The grid redraws one canvas while scrolling,
// so WebKit does not create new layers or tiles for every frame, which was
// the problem with the DOM lists in S-09. The data still stays in Rust:
// `BatchCache` fetches batches of 500 rows with `invoke`.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DataEditor,
  GridCellKind,
  type DataEditorRef,
  type GridCell,
  type GridColumn,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { api, type AppConfig, type Row } from "./api";
import { BatchCache } from "./batchCache";
import { afterNextPaint, registerScroller, signal, type SignalName } from "./bench";

/**
 * Batches loaded on each side of the visible rows: 2 x 500 rows ahead is
 * about one second of a fast trackpad fling, and a batch arrives from Rust in
 * a few milliseconds.
 */
const PREFETCH_BATCHES = 2;
/** Batches kept: 24 x 500 = 12,000 rows, about 2 MB of strings. */
const CACHE_BATCHES = 24;
/** Same column widths as the egui table and the DOM table; the last one grows. */
const TABLE_WIDTHS = [70, 140, 230, 100, 80, 90, 150, 200];
const STRIPE: Partial<Theme> = { bgCell: "#F4F4F6" };

const TABLE_THEME: Partial<Theme> = {
  fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
  baseFontStyle: "13px",
  headerFontStyle: "600 13px",
};

const LOGS_THEME: Partial<Theme> = {
  fontFamily: "ui-monospace, Menlo, monospace",
  baseFontStyle: "12px",
  cellHorizontalPadding: 6,
};

interface CanvasListProps<T> {
  name: "table" | "logs";
  count: number;
  batchSize: number;
  rowHeight: number;
  headerHeight: number;
  columns: GridColumn[];
  fetchBatch: (start: number, count: number) => Promise<T[]>;
  cellText: (row: T, column: number) => string;
  readySignal: SignalName;
  theme: Partial<Theme>;
  striped: boolean;
}

const LOADING_CELL: GridCell = { kind: GridCellKind.Loading, allowOverlay: false };

function CanvasList<T>(props: CanvasListProps<T>) {
  const { name, count, batchSize, rowHeight, headerHeight, columns } = props;
  const { fetchBatch, cellText, readySignal, theme, striped } = props;
  const gridRef = useRef<DataEditorRef>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  /** The rows on screen, as the grid last reported them. */
  const regionRef = useRef<Rectangle | null>(null);
  const signaledRef = useRef(false);
  const unregisterRef = useRef<(() => void) | null>(null);
  const onLoadRef = useRef<(batch: number) => void>(() => undefined);

  const [cache] = useState(
    () =>
      new BatchCache<T>(
        fetchBatch,
        count,
        batchSize,
        CACHE_BATCHES,
        (batch) => onLoadRef.current(batch),
        PREFETCH_BATCHES,
      ),
  );

  /** True when a row on screen still waits for its batch. */
  const hasMissingRows = useCallback(() => {
    const region = regionRef.current;
    if (region === null) return true;
    const end = Math.min(region.y + region.height, count);
    for (let row = region.y; row < end; row++) {
      if (cache.get(row) === undefined) return true;
    }
    return false;
  }, [cache, count]);

  /** Signals the bench once the first real rows have been painted. */
  const signalWhenReady = useCallback(() => {
    const region = regionRef.current;
    if (signaledRef.current || region === null || region.height === 0) return;
    if (hasMissingRows()) return;
    signaledRef.current = true;
    afterNextPaint(() => signal(readySignal));
  }, [hasMissingRows, readySignal]);

  // A batch arrived: redraw the cells of that batch that are on screen.
  onLoadRef.current = (batch: number) => {
    const region = regionRef.current;
    if (region === null) return;
    const first = Math.max(region.y, batch * batchSize);
    const last = Math.min(region.y + region.height, (batch + 1) * batchSize, count) - 1;
    if (first <= last) {
      const cells: { cell: Item }[] = [];
      for (let row = first; row <= last; row++) {
        for (let column = 0; column < columns.length; column++) cells.push({ cell: [column, row] });
      }
      gridRef.current?.updateCells(cells);
    }
    signalWhenReady();
  };

  const getCellContent = useCallback(
    ([column, row]: Item): GridCell => {
      const data = cache.get(row);
      if (data === undefined) return LOADING_CELL;
      const text = cellText(data, column);
      return {
        kind: GridCellKind.Text,
        data: text,
        displayData: text,
        allowOverlay: false,
        readonly: true,
      };
    },
    [cache, cellText],
  );

  // Called by the grid, synchronously, on every scroll that changes the rows.
  const onVisibleRegionChanged = useCallback(
    (range: Rectangle) => {
      regionRef.current = range;
      cache.ensure(range.y, range.y + Math.max(range.height, 1) - 1);
      if (unregisterRef.current === null) {
        // The grid's scroll element exists once the grid has a size.
        const element = hostRef.current?.querySelector<HTMLElement>(".dvn-scroller");
        if (element) unregisterRef.current = registerScroller(name, { element, hasMissingRows });
      }
      signalWhenReady();
    },
    [cache, name, hasMissingRows, signalWhenReady],
  );

  useEffect(
    () => () => {
      unregisterRef.current?.();
      unregisterRef.current = null;
    },
    [],
  );

  const getRowThemeOverride = useCallback(
    (row: number) => (striped && row % 2 === 1 ? STRIPE : undefined),
    [striped],
  );

  return (
    <div ref={hostRef} className="canvas-host">
      <DataEditor
        ref={gridRef}
        columns={columns}
        rows={count}
        getCellContent={getCellContent}
        rowHeight={rowHeight}
        headerHeight={headerHeight}
        width="100%"
        height="100%"
        smoothScrollX
        smoothScrollY
        rowMarkers="none"
        rangeSelect="none"
        columnSelect="none"
        rowSelect="none"
        verticalBorder={striped}
        theme={theme}
        getRowThemeOverride={getRowThemeOverride}
        onVisibleRegionChanged={onVisibleRegionChanged}
      />
    </div>
  );
}

const tableCell = (row: Row, column: number) => row[column] ?? "";
const logCell = (line: string) => line;

/** 100,000 rows x 8 columns, row height 20 (same as egui and the DOM table). */
export function CanvasTableTab({ config }: { config: AppConfig }) {
  const columns = useMemo<GridColumn[]>(
    () =>
      config.table_columns.map((title, index) => ({
        title,
        id: title,
        width: TABLE_WIDTHS[index] ?? 120,
        grow: index === config.table_columns.length - 1 ? 1 : undefined,
      })),
    [config.table_columns],
  );
  return (
    <CanvasList<Row>
      name="table"
      count={config.table_rows}
      batchSize={config.batch_size}
      rowHeight={20}
      headerHeight={22}
      columns={columns}
      fetchBatch={api.tableRows}
      cellText={tableCell}
      readySignal="table-ready"
      theme={TABLE_THEME}
      striped
    />
  );
}

/** 50,000 lines in one wide column, row height 18, no header. */
export function CanvasLogsTab({ config }: { config: AppConfig }) {
  const columns = useMemo<GridColumn[]>(
    () => [{ title: "", id: "line", width: 1000, grow: 1 }],
    [],
  );
  return (
    <CanvasList<string>
      name="logs"
      count={config.log_lines}
      batchSize={config.batch_size}
      rowHeight={18}
      headerHeight={0}
      columns={columns}
      fetchBatch={api.logLines}
      cellText={logCell}
      readySignal="logs-ready"
      theme={LOGS_THEME}
      striped={false}
    />
  );
}
