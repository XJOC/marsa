<?php

namespace App\Console\Commands;

use App\Console\Commands\Concerns\KvChecks;
use App\Jobs\KvFailJob;
use App\Jobs\KvOkJob;
use App\Jobs\KvRetryJob;
use App\Jobs\KvUniqueJob;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\Queue;

class KvDispatch extends Command
{
    use KvChecks;

    protected $signature = 'kv:dispatch {set : basic|block}';
    protected $description = 'S-12b: dispatch the probe jobs';

    public function handle(): int
    {
        $set = $this->argument('set');
        @unlink(storage_path('kv-evidence.log'));
        $s = "queue-$set-dispatch";
        if ($set === 'block') {
            for ($i = 0; $i < 3; $i++) {
                KvOkJob::dispatch("block-$i");
            }
            return self::SUCCESS;
        }
        $this->kvCheck($s, 'clean_start', fn() => Queue::size('default') === 0 ?: 'size=' . Queue::size('default'));
        for ($i = 0; $i < 5; $i++) {
            KvOkJob::dispatch("ok-$i");
        }
        KvOkJob::dispatch('delayed')->delay(now()->addSeconds(3));
        KvOkJob::dispatch('high')->onQueue('high');
        KvRetryJob::dispatch();
        KvFailJob::dispatch();
        KvUniqueJob::dispatch();
        KvUniqueJob::dispatch();  // must be dropped: the unique lock is held
        // 5 ok + delayed + retry + fail + 1 unique = 9 on default; 1 on high.
        $this->kvCheck($s, 'size_default', fn() => Queue::size('default') === 9 ?: 'size=' . Queue::size('default'));
        $this->kvCheck($s, 'size_high', fn() => Queue::size('high') === 1 ?: 'size=' . Queue::size('high'));
        return self::SUCCESS;
    }
}
