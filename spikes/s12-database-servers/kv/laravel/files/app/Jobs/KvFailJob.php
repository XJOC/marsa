<?php

namespace App\Jobs;

use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Foundation\Queue\Queueable;

// Always throws; gives up after 2 tries and lands in failed_jobs.
class KvFailJob implements ShouldQueue
{
    use Dispatchable, Queueable;

    public int $tries = 2;
    public int $backoff = 1;

    public function handle(): void
    {
        file_put_contents(storage_path('kv-evidence.log'), 'fail attempt=' . $this->attempts() . " threw\n", FILE_APPEND | LOCK_EX);
        throw new \RuntimeException('always fails');
    }

    public function failed(\Throwable $e): void
    {
        file_put_contents(storage_path('kv-evidence.log'), "fail failed-callback\n", FILE_APPEND | LOCK_EX);
    }
}
