<?php

namespace App\Console\Commands;

use App\Console\Commands\Concerns\KvChecks;
use Illuminate\Console\Command;
use Illuminate\Support\Facades\Cache;
use Illuminate\Support\Facades\RateLimiter;
use Illuminate\Support\Facades\Redis;

class KvCache extends Command
{
    use KvChecks;

    protected $signature = 'kv:cache';
    protected $description = 'S-12b: cache, locks, tags and raw Redis commands against the configured store';

    public function handle(): int
    {
        $s = 'cache';
        $this->kvCheck($s, 'config_is_redis_predis', fn() => config('cache.default') === 'redis'
            && config('database.redis.client') === 'predis' ?: 'cache.default=' . config('cache.default'));
        $this->kvCheck($s, 'ping', fn() => (string) Redis::connection('default')->ping() === 'PONG');
        $this->kvCheck($s, 'server_info', function () {
            $info = Redis::connection('default')->info();
            $flat = [];
            array_walk_recursive($info, function ($v, $k) use (&$flat) { $flat[$k] = $v; });
            $this->line('INFO ' . json_encode(array_intersect_key($flat, array_flip(
                ['redis_version', 'valkey_version', 'server_name', 'redis_mode', 'os', 'process_id', 'tcp_port']))));
            return true;
        });
        // Laravel's default cache connection is DB 1 (REDIS_CACHE_DB=1): SELECT must work.
        $this->kvCheck($s, 'select_db1', fn() => (string) Redis::connection('cache')->ping() === 'PONG');

        Cache::flush();
        $this->kvCheck($s, 'put_get', function () {
            Cache::put('a', 'b', 60);
            Cache::put('arr', ['x' => 1, 'y' => [2, 3]], 60);
            return Cache::get('a') === 'b' && Cache::get('arr') === ['x' => 1, 'y' => [2, 3]];
        });
        $this->kvCheck($s, 'has_forget_missing', function () {
            Cache::put('f', 1, 60);
            $had = Cache::has('f');
            Cache::forget('f');
            return $had && !Cache::has('f') && Cache::get('f', 'dflt') === 'dflt';
        });
        $this->kvCheck($s, 'increment_decrement', function () {
            Cache::forget('n');
            $a = Cache::increment('n');
            $b = Cache::increment('n', 5);
            $c = Cache::decrement('n', 2);
            return [$a, $b, $c, Cache::get('n')] === [1, 6, 4, 4] ?: json_encode([$a, $b, $c, Cache::get('n')]);
        });
        $this->kvCheck($s, 'add_nx', function () {
            Cache::forget('nx');
            return Cache::add('nx', 1, 60) === true && Cache::add('nx', 2, 60) === false && Cache::get('nx') === 1;
        });
        $this->kvCheck($s, 'remember_forever', function () {
            Cache::forget('rem');
            $v = Cache::remember('rem', 60, fn() => 'computed');
            Cache::forever('fe', 'always');
            return $v === 'computed' && Cache::get('rem') === 'computed' && Cache::get('fe') === 'always';
        });
        $this->kvCheck($s, 'many_putmany', function () {
            Cache::putMany(['m1' => 1, 'm2' => 2], 60);
            return Cache::many(['m1', 'm2', 'm3']) === ['m1' => 1, 'm2' => 2, 'm3' => null];
        });
        $this->kvCheck($s, 'tags', function () {
            Cache::tags(['people', 'artists'])->put('John', 'Doe', 60);
            Cache::tags(['people', 'authors'])->put('Anne', 'Smith', 60);
            Cache::put('untagged', 'stays', 60);
            $get = Cache::tags(['people', 'artists'])->get('John') === 'Doe';
            Cache::tags('artists')->flush();
            $after = Cache::tags(['people', 'artists'])->get('John') === null
                && Cache::tags(['people', 'authors'])->get('Anne') === 'Smith'
                && Cache::get('untagged') === 'stays';
            return $get && $after ?: json_encode([$get, $after]);
        });
        $this->kvCheck($s, 'lock_basic', function () {
            Cache::lock('lk', 10)->forceRelease();
            $a = Cache::lock('lk', 10);
            $b = Cache::lock('lk', 10);
            $got = $a->get();
            $second = $b->get();
            $a->release();
            $third = $b->get();
            $b->release();
            return [$got, $second, $third] === [true, false, true] ?: json_encode([$got, $second, $third]);
        });
        $this->kvCheck($s, 'lock_closure_owner_restore', function () {
            Cache::lock('lk2', 10)->forceRelease();
            $r = Cache::lock('lk2', 10)->get(fn() => 'ran');
            $a = Cache::lock('lk2', 10);
            $a->get();
            $owner = $a->owner();
            $restored = Cache::restoreLock('lk2', $owner);
            $ok = $restored->release();
            return $r === 'ran' && $ok === true;
        });
        $this->kvCheck($s, 'lock_block_waits', function () {
            Cache::lock('lk3', 10)->forceRelease();
            $holder = Cache::lock('lk3', 1);
            $holder->get();
            $t = microtime(true);
            $waiter = Cache::lock('lk3', 5);
            $got = $waiter->block(4);  // the holder expires after 1 s
            $took = microtime(true) - $t;
            $waiter->release();
            return ($got && $took > 0.5 && $took < 4) ?: "got=$got took=$took";
        });
        $this->kvCheck($s, 'rate_limiter', function () {
            RateLimiter::clear('rl');
            $n = 0;
            for ($i = 0; $i < 5; $i++) {
                if (RateLimiter::attempt('rl', 3, fn() => true, 60)) { $n++; }
            }
            return $n === 3 && RateLimiter::tooManyAttempts('rl', 3) ?: "allowed=$n";
        });
        $this->kvCheck($s, 'raw_hash_list_set_zset', function () {
            $r = Redis::connection('default');
            $r->del('h', 'l', 'st', 'z');
            $r->hset('h', 'f', 'v');
            $r->rpush('l', 'a', 'b', 'c');
            $r->sadd('st', 'x', 'y');
            $r->zadd('z', 1, 'one', 2, 'two');
            return $r->hgetall('h') === ['f' => 'v'] && $r->lrange('l', 0, -1) === ['a', 'b', 'c']
                && $r->scard('st') == 2 && $r->zrangebyscore('z', 0, 1) === ['one'] ?: 'mismatch';
        });
        $this->kvCheck($s, 'raw_pipeline', function () {
            $res = Redis::connection('default')->pipeline(function ($p) {
                $p->set('p1', 1); $p->set('p2', 2); $p->get('p1'); $p->incr('p2');
            });
            return ((string) $res[2] === '1' && (int) $res[3] === 3) ?: json_encode($res);
        });
        $this->kvCheck($s, 'raw_transaction_multi_exec', function () {
            $res = Redis::connection('default')->transaction(function ($t) {
                $t->set('t1', 'a'); $t->get('t1');
            });
            return ((string) $res[1] === 'a') ?: json_encode($res);
        });
        $this->kvCheck($s, 'raw_lua_eval', function () {
            $r = Redis::connection('default')->eval("return redis.call('incr', KEYS[1]) + tonumber(ARGV[1])", 1, 'lua:c', 10);
            return $r == 11 ?: json_encode($r);
        });
        $this->kvCheck($s, 'raw_lua_evalsha', function () {
            $c = Redis::connection('default');
            $sha = $c->script('load', "return ARGV[1]");
            return $c->evalsha($sha, 0, 'hi') === 'hi';
        });
        $this->kvCheck($s, 'raw_scan', function () {
            $c = Redis::connection('default');
            for ($i = 0; $i < 20; $i++) { $c->set("scan:$i", $i); }
            $found = 0; $cursor = '0';
            do {
                [$cursor, $keys] = $c->scan($cursor, ['match' => 'scan:*', 'count' => 100]);
                $found += count($keys);
            } while ((string) $cursor !== '0');
            return $found === 20 ?: "found=$found";
        });
        // Needs a second of waiting; keep it last.
        $this->kvCheck($s, 'ttl_expiry', function () {
            Cache::put('short', 'x', 1);
            $before = Cache::get('short');
            sleep(2);
            return $before === 'x' && Cache::get('short') === null;
        });
        return self::SUCCESS;
    }
}
