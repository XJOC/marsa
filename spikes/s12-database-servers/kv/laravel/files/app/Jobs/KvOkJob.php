<?php

namespace App\Jobs;

use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Foundation\Queue\Queueable;

class KvOkJob implements ShouldQueue
{
    use Dispatchable, Queueable;

    public function __construct(public string $tag) {}

    public function handle(): void
    {
        file_put_contents(storage_path('kv-evidence.log'), $this->tag . "\n", FILE_APPEND | LOCK_EX);
    }
}
