import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import ar from "./i18n/ar.json";
import en from "./i18n/en.json";

type Locale = "ar" | "en";
type Dict = typeof ar;
const dicts: Record<Locale, Dict> = { ar, en };

// Mirrors `Diagnostics` and `SidecarOutput` in src-tauri/src/lib.rs.
interface Diagnostics {
  os: string;
  arch: string;
  webview_version: string;
  current_exe: string;
  app_data_dir: string;
}
interface SidecarOutput {
  exit_code: number;
  stdout: string;
}
interface FontCounts {
  arabic: number;
  latin: number;
}
interface CspViolations {
  count: number;
  first: string;
}

// Renders `backtick` segments as <code>, the way the task file writes them.
// `isolate` puts each one in dir="ltr": a leading "/" has no direction of its
// own, so inside Arabic text it otherwise moves to the end of the path.
function Mixed({ text, isolate = false }: { text: string; isolate?: boolean }) {
  return (
    <>
      {text.split("`").map((part, i) => {
        if (i % 2 === 0) return <span key={i}>{part}</span>;
        const code = <code key={i}>{part}</code>;
        return isolate ? <span key={i} dir="ltr">{code}</span> : code;
      })}
    </>
  );
}

// The inner span isolates the value and leaves the <dd> aligned with the page.
function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="row">
      <dt>{label}</dt>
      <dd>
        <span dir="ltr">{value}</span>
      </dd>
    </div>
  );
}

export default function App() {
  const [locale, setLocale] = useState<Locale>("ar");
  const [diag, setDiag] = useState<Diagnostics | null>(null);
  const [diagError, setDiagError] = useState<string | null>(null);
  const [sidecar, setSidecar] = useState<SidecarOutput | null>(null);
  const [sidecarError, setSidecarError] = useState<string | null>(null);
  const [fonts, setFonts] = useState<FontCounts | null>(null);
  const [csp, setCsp] = useState<CspViolations>({ count: 0, first: "" });

  const t = dicts[locale];

  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.dir = locale === "ar" ? "rtl" : "ltr";
  }, [locale]);

  // Declared before the `invoke` effect, so the listener is attached before
  // the first IPC call: a block during startup is the one we must not miss.
  // Tauri adds nothing to `connect-src` itself; a violation here means the
  // CSP in `tauri.conf.json` and the actual IPC scheme disagree.
  useEffect(() => {
    function onViolation(event: SecurityPolicyViolationEvent) {
      setCsp((prev) => ({
        count: prev.count + 1,
        first: prev.first === "" ? event.blockedURI : prev.first,
      }));
    }
    document.addEventListener("securitypolicyviolation", onViolation);
    return () => document.removeEventListener("securitypolicyviolation", onViolation);
  }, []);

  useEffect(() => {
    let alive = true;
    invoke<Diagnostics>("diagnostics")
      .then((d) => alive && setDiag(d))
      .catch((e) => alive && setDiagError(String(e)));
    Promise.all([
      document.fonts.load("400 16px 'IBM Plex Sans Arabic'", "مرسى 8.4"),
      document.fonts.load("700 16px 'IBM Plex Sans Arabic'", "مرسى 8.4"),
      document.fonts.load("400 16px 'IBM Plex Sans'", "Marsa 8.4"),
      document.fonts.load("700 16px 'IBM Plex Sans'", "Marsa 8.4"),
    ])
      .then(([ar400, ar700, la400, la700]) => {
        if (alive) setFonts({ arabic: ar400.length + ar700.length, latin: la400.length + la700.length });
      })
      .catch(() => alive && setFonts({ arabic: 0, latin: 0 }));
    return () => {
      alive = false;
    };
  }, []);

  async function runSidecar() {
    setSidecar(null);
    setSidecarError(null);
    try {
      setSidecar(await invoke<SidecarOutput>("run_sidecar"));
    } catch (e) {
      setSidecarError(String(e));
    }
  }

  const rows: Array<[string, string]> = diag
    ? [
        [t.os, `${diag.os} (${diag.arch})`],
        [t.webview, diag.webview_version],
        [t.currentExe, diag.current_exe],
        [t.appDataDir, diag.app_data_dir],
        [t.origin, window.location.origin],
        [t.secureContext, String(window.isSecureContext)],
      ]
    : [];

  return (
    <main className="app">
      <header className="header">
        <h1>{t.title}</h1>
        <button type="button" onClick={() => setLocale(locale === "ar" ? "en" : "ar")}>
          {t.switchLocale}
        </button>
      </header>

      <p>{t.intro}</p>
      <p>
        <Mixed text={t.mixed} />
      </p>
      <p>
        {t.mixedIsolated}: <Mixed text={t.mixed} isolate />
      </p>
      <p className="system-font">{t.systemFontNote}</p>

      <section className="card">
        <h2>{t.fontsTitle}</h2>
        <p>
          {t.fontsLoaded}{" "}
          {fonts ? (
            <>
              {t.fontsArabic}: {fonts.arabic} · {t.fontsLatin}: {fonts.latin} · {t.fontsTotal}:{" "}
              {fonts.arabic + fonts.latin}
            </>
          ) : (
            t.loading
          )}
        </p>
        <p className="note">{t.fontsZero}</p>
      </section>

      <section className="card">
        <h2>{t.diagTitle}</h2>
        {diagError && <p className="error">{`${t.error}: ${diagError}`}</p>}
        {!diag && !diagError && <p>{t.loading}</p>}
        <dl>
          {rows.map(([label, value]) => (
            <Row key={label} label={label} value={value} />
          ))}
          {/* Stays visible when `diagnostics` fails: a blocked IPC call is the
              likeliest reason it did. */}
          <Row
            label={t.cspViolations}
            value={csp.first === "" ? String(csp.count) : `${csp.count} · ${csp.first}`}
          />
        </dl>
      </section>

      <section className="card">
        <h2>{t.sidecarTitle}</h2>
        <button type="button" onClick={() => void runSidecar()}>
          {t.runSidecar}
        </button>
        {sidecarError && <p className="error">{`${t.error}: ${sidecarError}`}</p>}
        {sidecar && (
          <p>
            {t.exitCode}: {sidecar.exit_code}
            <br />
            <code>{sidecar.stdout}</code>
          </p>
        )}
      </section>
    </main>
  );
}
