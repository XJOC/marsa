<?php

namespace App\Jobs;

use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Foundation\Queue\Queueable;

// Throws on attempt 1, succeeds on attempt 2 (released with a 2 s backoff).
class KvRetryJob implements ShouldQueue
{
    use Dispatchable, Queueable;

    public int $tries = 3;
    public int $backoff = 2;

    public function handle(): void
    {
        $f = storage_path('kv-evidence.log');
        if ($this->attempts() === 1) {
            file_put_contents($f, "retry attempt=1 threw\n", FILE_APPEND | LOCK_EX);
            throw new \RuntimeException('first attempt fails on purpose');
        }
        file_put_contents($f, 'retry attempt=' . $this->attempts() . " ok\n", FILE_APPEND | LOCK_EX);
    }
}
