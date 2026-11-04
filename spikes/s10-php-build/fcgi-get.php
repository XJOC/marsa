<?php
// S-10: a tiny FastCGI client. Sends one GET request to php-fpm and prints the body.
//   php fcgi-get.php <host> <port> <absolute path of the script to run>
// Exit code 0 if a response arrived, 1 on any error. Used by smoke.sh to prove that
// the php-fpm we built answers a request. This is not the M4 test through nginx/Caddy.

if ($argc !== 4) {
    fwrite(STDERR, "usage: php fcgi-get.php <host> <port> <script>\n");
    exit(2);
}
[, $host, $port, $script] = $argv;

function rec(int $type, string $body): string
{
    return pack('CCnnCC', 1, $type, 1, strlen($body), 0, 0) . $body;
}

function nv(string $name, string $value): string
{
    $len = fn (string $s): string => strlen($s) < 128 ? chr(strlen($s)) : pack('N', strlen($s) | 0x80000000);
    return $len($name) . $len($value) . $name . $value;
}

$params = nv('GATEWAY_INTERFACE', 'FastCGI/1.0')
    . nv('REQUEST_METHOD', 'GET')
    . nv('SCRIPT_FILENAME', $script)
    . nv('SCRIPT_NAME', '/' . basename($script))
    . nv('REQUEST_URI', '/' . basename($script))
    . nv('SERVER_PROTOCOL', 'HTTP/1.1')
    . nv('SERVER_NAME', 'localhost')
    . nv('SERVER_PORT', '80')
    . nv('REMOTE_ADDR', '127.0.0.1');

// BEGIN_REQUEST (role RESPONDER = 1, no keep-alive), PARAMS, empty PARAMS, empty STDIN.
$request = rec(1, pack('nCx5', 1, 0)) . rec(4, $params) . rec(4, '') . rec(5, '');

$s = false;
for ($i = 0; $i < 20 && $s === false; $i++) {
    $s = @stream_socket_client("tcp://$host:$port", $errno, $err, 2);
    if ($s === false) {
        usleep(500000);
    }
}
if ($s === false) {
    fwrite(STDERR, "cannot connect to $host:$port: $err\n");
    exit(1);
}
stream_set_timeout($s, 10);
fwrite($s, $request);

function read_exact($s, int $n): string|false
{
    $buf = '';
    while (strlen($buf) < $n) {
        $chunk = fread($s, $n - strlen($buf));
        if ($chunk === false || $chunk === '') {
            return false;
        }
        $buf .= $chunk;
    }
    return $buf;
}

$stdout = '';
$stderr = '';
while (($h = read_exact($s, 8)) !== false) {
    $f = unpack('Cver/Ctype/nid/nlen/Cpad/Cres', $h);
    $body = $f['len'] > 0 ? read_exact($s, $f['len']) : '';
    if ($f['pad'] > 0) {
        read_exact($s, $f['pad']);
    }
    if ($f['type'] === 6) {
        $stdout .= $body;
    } elseif ($f['type'] === 7) {
        $stderr .= $body;
    } elseif ($f['type'] === 3) {
        break;
    }
}
fclose($s);

if ($stderr !== '') {
    fwrite(STDERR, $stderr);
}
if ($stdout === '') {
    fwrite(STDERR, "empty response\n");
    exit(1);
}
$parts = explode("\r\n\r\n", $stdout, 2);
echo $parts[1] ?? $stdout;
