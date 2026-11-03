<?php
// S-03 CPU benchmark: loops, function calls, arrays, strings. Fixed values, no randomness.
// Prints one JSON line: wall ms and CPU ms (user+sys) of the workload, ms per part, PHP peak memory.
//   work/php-cli/php -n [-d zend_extension=...] bench.php
declare(strict_types=1);

function fib(int $n): int
{
    return $n < 2 ? $n : fib($n - 1) + fib($n - 2);
}

function cpu_ms(): float
{
    $u = getrusage();
    return ($u['ru_utime.tv_sec'] + $u['ru_stime.tv_sec']) * 1000 + ($u['ru_utime.tv_usec'] + $u['ru_stime.tv_usec']) / 1000;
}

function part(array &$parts, string $name, callable $f): int
{
    $t = hrtime(true);
    $r = $f();
    $parts[$name] = round((hrtime(true) - $t) / 1e6, 2);
    return $r;
}

$parts = [];
$c0 = cpu_ms();
$t0 = hrtime(true);
$acc = 0;
$acc += part($parts, 'loop', function (): int {            // plain loop, no calls
    $acc = 0;
    for ($i = 0; $i < 12_000_000; $i++) {
        $acc += ($i * 7) % 13;
    }
    return $acc;
});
$acc += part($parts, 'calls', fn(): int => fib(30));         // 1.6 million small function calls
$acc += part($parts, 'arrays', function (): int {            // arrays + array_map (builtin calling a closure)
    $a = [];
    for ($i = 0; $i < 1_000_000; $i++) {
        $a[] = $i * 3;
    }
    return array_sum(array_map(fn(int $x): int => $x % 11, $a));
});
$acc += part($parts, 'strings', function (): int {           // string building and builtins
    $s = '';
    for ($i = 0; $i < 1_000_000; $i++) {
        $s .= chr(97 + $i % 26);
    }
    return strlen(str_replace('abc', 'x', $s)) + crc32($s);
});
$ms = (hrtime(true) - $t0) / 1e6;
$cpu = cpu_ms() - $c0;
echo json_encode([
    'ms' => round($ms, 2),
    'cpu_ms' => round($cpu, 2),
    'parts' => $parts,
    'peak_bytes' => memory_get_peak_usage(),
    'result' => $acc,
]), "\n";
