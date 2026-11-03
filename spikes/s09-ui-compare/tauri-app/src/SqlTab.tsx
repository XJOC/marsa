import { useEffect, useRef } from "react";
import { basicSetup, EditorView } from "codemirror";
import { sql } from "@codemirror/lang-sql";
import { api, reportError } from "./api";
import { afterNextPaint, registerEditor, signal } from "./bench";

/**
 * CodeMirror 6 with SQL highlighting. The 5,000 lines come from Rust when the
 * tab opens for the first time; CodeMirror renders only the lines in view.
 */
export function SqlTab() {
  const hostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const parent = hostRef.current;
    if (parent === null) return;
    let view: EditorView | undefined;
    let cancelled = false;
    api.sqlText().then(
      (doc) => {
        if (cancelled) return;
        view = new EditorView({ doc, extensions: [basicSetup, sql()], parent });
        // CodeMirror injects its styles at run time. Its base theme makes the
        // editor a flex box; anything else means the CSP blocked the styles.
        if (getComputedStyle(view.dom).display !== "flex") {
          reportError("CodeMirror", "editor styles were not applied");
        }
        registerEditor(view);
        afterNextPaint(() => signal("sql-ready"));
      },
      (error: unknown) => reportError("sql_text", error),
    );
    return () => {
      cancelled = true;
      if (view !== undefined) registerEditor(null);
      view?.destroy();
    };
  }, []);
  return <div ref={hostRef} className="sql-host" />;
}
