# Sourced by the e2e scripts: calls to marsad's API, as the window makes them.
#
# The token goes from `run/daemon.json` into a file of headers that only this
# user can read. It is never printed and never on a command line. Needs
# DATA (the data folder) and RUNNER_TEMP. Uses jq and curl, which the runners
# have.

# api METHOD PATH [JSON]: the answer's body, then a last line with its HTTP
# status.
api() {
  local method=$1 path=$2 body=${3:-}
  local headers=$RUNNER_TEMP/api-headers
  (umask 077 && jq -r '"Authorization: Bearer " + .token' "$DATA/run/daemon.json" >"$headers")
  local port
  port=$(jq -r .port "$DATA/run/daemon.json")
  local args=(-sS -X "$method" -H "@$headers" -w '\n%{http_code}' "http://127.0.0.1:$port$path")
  if [ -n "$body" ]; then
    args+=(-H 'content-type: application/json' --data "$body")
  fi
  local rc=0
  curl "${args[@]}" || rc=$?
  rm -f "$headers"
  return "$rc"
}

# api_ok METHOD PATH [JSON]: the body of a 2xx answer, or a failure with the
# status and the body.
api_ok() {
  local answer status
  answer=$(api "$@")
  status=${answer##*$'\n'}
  case "$status" in
    2??) printf '%s\n' "${answer%$'\n'*}" ;;
    *) fail "$1 $2 answered $status: ${answer%$'\n'*}" ;;
  esac
}

# api_status METHOD PATH [JSON]: the HTTP status and the error code, as
# `404 db_not_found`.
api_status() {
  local answer
  answer=$(api "$@")
  printf '%s %s\n' "${answer##*$'\n'}" "$(printf '%s' "${answer%$'\n'*}" | jq -r '.error.code // empty')"
}

# api_job_end ID: the last event of job ID, waiting up to 30 s for its end.
api_job_end() {
  local event
  for _ in $(seq 1 300); do
    event=$(api_ok GET "/v1/jobs/$1")
    if [ "$(jq -r .status <<<"$event")" != running ]; then
      printf '%s\n' "$event"
      return 0
    fi
    sleep 0.1
  done
  fail "job $1 did not end in 30 s"
}

# api_query SOURCE_JSON SQL: starts the statement as a job, prints its id.
api_query() {
  api_ok POST /v1/jobs/db/query "$(jq -cn --argjson source "$1" --arg sql "$2" '{source: $source, sql: $sql}')" | jq -r .id
}
