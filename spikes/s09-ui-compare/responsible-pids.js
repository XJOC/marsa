// Used by measure.sh (JavaScript for Automation: `osascript -l JavaScript`).
//
// Prints, one per line, the PIDs from argv[1..] that belong to the app with
// PID argv[0]. WebKit's WebContent, GPU, and Networking processes are XPC
// services: their parent is launchd, not the app. macOS still records which
// app is "responsible" for them, and Activity Monitor groups them with that
// libSystem call. A candidate belongs to the app when its responsible PID is
// the app itself (the app was started from Finder or the tray) or the app's
// own responsible process (the app was started from a terminal, which then
// owns the app and its XPC services). measure.sh also drops WebKit processes
// that existed before the app started.
ObjC.import("stdlib");

function run(argv) {
  ObjC.bindFunction("responsibility_get_pid_responsible_for_pid", ["int", ["int"]]);
  const responsible = (pid) => $.responsibility_get_pid_responsible_for_pid(pid);
  const app = parseInt(argv[0], 10);
  const owner = responsible(app);
  const matches = [];
  for (const arg of argv.slice(1)) {
    const pid = parseInt(arg, 10);
    const candidateOwner = responsible(pid);
    if (candidateOwner === app || candidateOwner === owner) matches.push(pid);
  }
  return matches.join("\n");
}
