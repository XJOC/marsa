import { createRoot } from "react-dom/client";
import "./app.css";
import { api, reportError } from "./api";
import { App } from "./App";
import { strings } from "./strings";

// A blocked style or script would change what is measured: report it.
document.addEventListener("securitypolicyviolation", (event) =>
  reportError("CSP", `${event.violatedDirective} blocked ${event.blockedURI}`),
);

const container = document.getElementById("root");
if (container === null) throw new Error("index.html has no #root element");
const root = createRoot(container);

// The row counts and bench timings come from Rust, so the UI waits for them.
api.appConfig().then(
  (config) => root.render(<App config={config} />),
  (error: unknown) =>
    root.render(
      <p className="error">
        {strings.configError} {String(error)}
      </p>,
    ),
);
