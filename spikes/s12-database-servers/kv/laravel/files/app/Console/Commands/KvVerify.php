<?php

namespace App\Console\Commands;

use App\Console\Commands\Concerns\KvChecks;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Queue;

class KvVerify extends Command
{
    use KvChecks;

    protected $signature = 'kv:verify {stage : 1|2|block}';
    protected $description = 'S-12b: check what the queue worker did';

    public function handle(): int
    {
        $ev = $this->kvEvidence();
        $count = fn(string $p) => count(array_filter($ev, fn($l) => str_starts_with($l, $p)));
        $stage = $this->argument('stage');
        $s = "queue-$stage";
        $this->line('EVIDENCE ' . json_encode($ev));
        if ($stage === 'block') {
            $this->kvCheck($s, 'blpop_jobs_done', fn() => $count('block-') === 3 ?: 'done=' . $count('block-'));
            return self::SUCCESS;
        }
        if ($stage === '1') {
            $this->kvCheck($s, 'immediate_jobs_done', fn() => $count('ok-') === 5 ?: 'done=' . $count('ok-'));
            $this->kvCheck($s, 'named_queue_done', fn() => $count('high') === 1);
            $this->kvCheck($s, 'delayed_not_yet', fn() => $count('delayed') === 0 ?: 'ran too early');
            $this->kvCheck($s, 'unique_ran_once', fn() => $count('unique') === 1 ?: 'ran=' . $count('unique'));
            $this->kvCheck($s, 'retry_first_attempt_failed', fn() => $count('retry attempt=1 threw') === 1);
            $this->kvCheck($s, 'fail_first_attempt_failed', fn() => $count('fail attempt=1 threw') === 1);
            $this->kvCheck($s, 'released_jobs_wait_in_delayed_set', fn() => Queue::size('default') === 3 ?: 'size=' . Queue::size('default'));
            return self::SUCCESS;
        }
        $this->kvCheck($s, 'delayed_ran_after_wait', fn() => $count('delayed') === 1 ?: 'ran=' . $count('delayed'));
        $this->kvCheck($s, 'retry_succeeded_on_attempt_2', fn() => $count('retry attempt=2 ok') === 1);
        $this->kvCheck($s, 'fail_gave_up_after_tries', fn() => $count('fail attempt=2 threw') === 1
            && $count('fail failed-callback') === 1 ?: json_encode($ev));
        $this->kvCheck($s, 'queue_empty', fn() => Queue::size('default') === 0 ?: 'size=' . Queue::size('default'));
        $this->kvCheck($s, 'failed_jobs_row', fn() => DB::table('failed_jobs')->count() === 1 ?: 'rows=' . DB::table('failed_jobs')->count());
        $this->kvCheck($s, 'unique_lock_released', fn() => $count('unique') === 1);
        return self::SUCCESS;
    }
}
