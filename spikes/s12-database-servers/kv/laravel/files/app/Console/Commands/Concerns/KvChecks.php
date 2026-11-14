<?php

namespace App\Console\Commands\Concerns;

// Shared by the kv:* commands. Each check prints one line and appends one JSON
// line to storage/kv-results.jsonl, so the shell can build the summary.
trait KvChecks
{
    protected function kvCheck(string $stage, string $name, callable $fn): bool
    {
        try {
            $r = $fn();
            $ok = $r === true;
            $detail = $ok ? '' : (is_string($r) ? $r : var_export($r, true));
        } catch (\Throwable $e) {
            $ok = false;
            $detail = get_class($e) . ': ' . substr(str_replace(["\r", "\n"], ' ', $e->getMessage()), 0, 300);
        }
        $line = ($ok ? 'PASS' : 'FAIL') . " $stage/$name" . ($detail !== '' ? " -- $detail" : '');
        $this->line($line);
        file_put_contents(
            storage_path('kv-results.jsonl'),
            json_encode(['stage' => $stage, 'name' => $name, 'ok' => $ok, 'detail' => $detail]) . "\n",
            FILE_APPEND | LOCK_EX
        );
        return $ok;
    }

    protected function kvEvidence(): array
    {
        $f = storage_path('kv-evidence.log');
        return is_file($f) ? array_values(array_filter(array_map('trim', file($f)))) : [];
    }
}
