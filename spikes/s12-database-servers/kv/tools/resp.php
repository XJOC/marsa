<?php
// A tiny RESP2 client with no dependencies, so every candidate can be probed
// the same way, even when it ships no CLI (Garnet, Memurai without redis-cli).
//
//   php resp.php [--host H] [--port P] [--wait SECONDS] [--raw] CMD [ARG...]
//
// Prints the reply as plain text (arrays one element per line). Exit codes:
// 0 reply, 1 error reply, 2 cannot connect (after --wait), 3 protocol error.
// `SHUTDOWN` may close the connection without a reply: that counts as 0.

$host = '127.0.0.1';
$port = 6379;
$wait = 0.0;
$args = array_slice($argv, 1);
while ($args && str_starts_with($args[0], '--')) {
    $opt = array_shift($args);
    if ($opt === '--host') {
        $host = array_shift($args);
    } elseif ($opt === '--port') {
        $port = (int) array_shift($args);
    } elseif ($opt === '--wait') {
        $wait = (float) array_shift($args);
    } else {
        fwrite(STDERR, "unknown option $opt\n");
        exit(3);
    }
}
if (!$args) {
    fwrite(STDERR, "no command\n");
    exit(3);
}

$deadline = microtime(true) + $wait;
$sock = false;
do {
    $sock = @stream_socket_client("tcp://$host:$port", $errno, $errstr, 2);
    if ($sock) {
        break;
    }
    usleep(200000);
} while (microtime(true) < $deadline);
if (!$sock) {
    fwrite(STDERR, "cannot connect to $host:$port: $errstr\n");
    exit(2);
}
stream_set_timeout($sock, 10);

$cmd = '*' . count($args) . "\r\n";
foreach ($args as $a) {
    $cmd .= '$' . strlen($a) . "\r\n" . $a . "\r\n";
}
fwrite($sock, $cmd);

function readReply($sock, int &$status)
{
    $line = fgets($sock);
    if ($line === false) {
        throw new RuntimeException('connection closed');
    }
    $type = $line[0];
    $rest = rtrim(substr($line, 1), "\r\n");
    switch ($type) {
        case '+':
            return $rest;
        case '-':
            $status = 1;
            return "ERR $rest";
        case ':':
            return $rest;
        case '$':
            $len = (int) $rest;
            if ($len < 0) {
                return '(nil)';
            }
            $data = '';
            while (strlen($data) < $len + 2) {
                $chunk = fread($sock, $len + 2 - strlen($data));
                if ($chunk === false || $chunk === '') {
                    throw new RuntimeException('short bulk');
                }
                $data .= $chunk;
            }
            return substr($data, 0, $len);
        case '*':
            $n = (int) $rest;
            if ($n < 0) {
                return '(nil)';
            }
            $items = [];
            for ($i = 0; $i < $n; $i++) {
                $items[] = readReply($sock, $status);
            }
            return implode("\n", array_map(fn($x) => is_array($x) ? implode(' ', $x) : $x, $items));
        default:
            throw new RuntimeException("bad reply type '$type'");
    }
}

$status = 0;
try {
    echo readReply($sock, $status), "\n";
} catch (RuntimeException $e) {
    if (strtoupper($args[0]) === 'SHUTDOWN') {
        echo "(connection closed)\n";
        exit(0);
    }
    fwrite(STDERR, $e->getMessage() . "\n");
    exit(3);
}
exit($status);
