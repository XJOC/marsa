# S-12b: the Windows twin of run.sh. Builds a fresh Laravel app and runs the
# cache and queue probes against a server already listening on 127.0.0.1:$Port.
#
#   laravel/run.ps1 -Port 6379 -Work C:\w -Out C:\out -Label garnet
#
# Exit code 0 even when checks fail: the results are the answer.
param(
  [Parameter(Mandatory)][int]$Port,
  [Parameter(Mandatory)][string]$Work,
  [Parameter(Mandatory)][string]$Out,
  [Parameter(Mandatory)][string]$Label
)
$ErrorActionPreference = 'Continue'
$Kv = Split-Path -Parent $PSScriptRoot
New-Item -ItemType Directory -Force -Path $Work, $Out | Out-Null
$Log = Join-Path $Out "laravel-$Label.log"
Set-Content -Path $Log -Value ''
function Say($m) { "[laravel] $m" | Tee-Object -FilePath $Log -Append }
function Run-Logged([string[]]$Cmd) {
  Say ('$ ' + ($Cmd -join ' '))
  $exe = $Cmd[0]; $rest = $Cmd[1..($Cmd.Length - 1)]
  & $exe @rest 2>&1 | Tee-Object -FilePath $Log -Append
  return $LASTEXITCODE
}

$App = Join-Path $Work 'app'
if (-not (Test-Path $App)) {
  Say 'composer create-project laravel/laravel'
  Push-Location $Work
  composer create-project laravel/laravel app --no-interaction --prefer-dist --quiet 2>&1 | Add-Content $Log
  $rc = $LASTEXITCODE
  Pop-Location
  if ($rc -ne 0) { Say 'create-project FAILED'; exit 0 }
  Push-Location $App
  composer require predis/predis --no-interaction --quiet 2>&1 | Add-Content $Log
  if ($LASTEXITCODE -ne 0) { Say 'require predis FAILED'; Pop-Location; exit 0 }
  Copy-Item -Recurse -Force (Join-Path $Kv 'laravel/files/app/*') (Join-Path $App 'app')
  $q = Join-Path $App 'config/queue.php'
  (Get-Content $q -Raw).Replace("'block_for' => null", "'block_for' => env('REDIS_QUEUE_BLOCK_FOR')") | Set-Content $q -NoNewline
  php artisan migrate --force 2>&1 | Add-Content $Log
  Pop-Location
}
Set-Location $App
Say ("laravel=" + (php artisan --version))
Say ("php=" + (php -r 'echo PHP_VERSION;'))

$env:REDIS_CLIENT = 'predis'; $env:CACHE_STORE = 'redis'; $env:QUEUE_CONNECTION = 'redis'
$env:SESSION_DRIVER = 'file'; $env:REDIS_HOST = '127.0.0.1'; $env:REDIS_PORT = "$Port"
$env:REDIS_PASSWORD = 'null'; $env:DB_CONNECTION = 'sqlite'
$env:REDIS_PREFIX = 'kv_' + ($Label -replace '[^a-zA-Z0-9]', '_') + '_'
Remove-Item -Force -ErrorAction SilentlyContinue storage/kv-results.jsonl, storage/kv-evidence.log

function Work-Queue {
  Say '$ queue:work'
  # --max-time keeps a stuck worker from running for ever.
  php artisan queue:work redis --queue=high,default --stop-when-empty --sleep=1 --max-time=60 2>&1 | Tee-Object -FilePath $Log -Append
  Say "worker exit: $LASTEXITCODE"
}

Run-Logged @('php', 'artisan', 'kv:cache') | Out-Null
Run-Logged @('php', 'artisan', 'kv:dispatch', 'basic') | Out-Null
Work-Queue
Run-Logged @('php', 'artisan', 'kv:verify', '1') | Out-Null
Say 'waiting 5 s for the delayed and released jobs'
Start-Sleep -Seconds 5
Work-Queue
Run-Logged @('php', 'artisan', 'kv:verify', '2') | Out-Null
$env:REDIS_QUEUE_BLOCK_FOR = '1'
Run-Logged @('php', 'artisan', 'kv:dispatch', 'block') | Out-Null
Work-Queue
Run-Logged @('php', 'artisan', 'kv:verify', 'block') | Out-Null
Remove-Item Env:REDIS_QUEUE_BLOCK_FOR

$res = Join-Path $Out "laravel-$Label.jsonl"
if (Test-Path storage/kv-results.jsonl) { Copy-Item storage/kv-results.jsonl $res } else { Set-Content $res '' }
if (Test-Path storage/logs/laravel.log) { Copy-Item storage/logs/laravel.log (Join-Path $Out "laravel-$Label-app.log") }
$pass = @(Select-String -Path $res -Pattern '"ok":true').Count
$fail = @(Select-String -Path $res -Pattern '"ok":false').Count
Say "done: $pass passed, $fail failed"
exit 0
