<?php

namespace App\Jobs;

use Illuminate\Contracts\Queue\ShouldBeUnique;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Foundation\Queue\Queueable;

// ShouldBeUnique takes a cache lock when it is dispatched.
class KvUniqueJob implements ShouldQueue, ShouldBeUnique
{
    use Dispatchable, Queueable;

    public int $uniqueFor = 60;

    public function handle(): void
    {
        file_put_contents(storage_path('kv-evidence.log'), "unique\n", FILE_APPEND | LOCK_EX);
    }
}
