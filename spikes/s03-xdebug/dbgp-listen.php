<?php
// S-03 tiny DBGp listener (the "IDE" side). Run it with the spike's own PHP, without ini:
//   work/php-cli/php -n dbgp-listen.php [port] [breakpoint-line]
// It listens on 127.0.0.1:<port> (default 9003), accepts ONE connection (30 s timeout), then:
//   1. reads the <init> packet and checks that the engine is Xdebug,
//   2. status, breakpoint_set (line of hello.php), run -> expects status="break",
//   3. stack_get (prints the stack), run -> expects status="stopping", stop.
// Packets are "<length>\0<xml>\0" from Xdebug and "command args\0" from us.
// Exit code 0 only if init names Xdebug, the breakpoint hit, the stack shows greet(), and the last run ended in "stopping".
declare(strict_types=1);

$port = (int)($argv[1] ?? 9003);
$line = (int)($argv[2] ?? 5);

$server = stream_socket_server("tcp://127.0.0.1:$port", $errno, $errstr);
if ($server === false) {
    fwrite(STDERR, "listen failed on 127.0.0.1:$port: $errstr\n");
    exit(2);
}
fwrite(STDERR, "listening on 127.0.0.1:$port\n");   // 40-check.sh waits for this line

$conn = stream_socket_accept($server, 30);
if ($conn === false) {
    echo "no connection within 30 s\n";
    exit(3);
}
stream_set_timeout($conn, 30);
echo "connection from ", stream_socket_get_name($conn, true), "\n";

function read_packet($conn): ?string
{
    $len = '';
    while (true) {
        $c = fread($conn, 1);
        if ($c === false || $c === '') {
            return null;
        }
        if ($c === "\0") {
            break;
        }
        $len .= $c;
    }
    $want = (int)$len;
    $xml = '';
    while (strlen($xml) < $want) {
        $chunk = fread($conn, $want - strlen($xml));
        if ($chunk === false || $chunk === '') {
            return null;
        }
        $xml .= $chunk;
    }
    fread($conn, 1);   // trailing NUL
    return $xml;
}

function command($conn, string $cmd): ?string
{
    fwrite($conn, $cmd . "\0");
    echo ">> $cmd\n";
    $reply = read_packet($conn);
    echo "<< ", $reply ?? '(connection closed)', "\n";
    return $reply;
}

$init = read_packet($conn);
echo "<< INIT\n", $init ?? '(nothing)', "\n";
$initOk = $init !== null && str_contains($init, '<init') && str_contains($init, 'Xdebug');
preg_match('/fileuri="([^"]+)"/', $init ?? '', $m);
$fileuri = $m[1] ?? '';

command($conn, 'status -i 1');
command($conn, "breakpoint_set -i 2 -t line -f $fileuri -n $line");
$r = command($conn, 'run -i 3');
$hitBreak = $r !== null && str_contains($r, 'status="break"');
$stack = command($conn, 'stack_get -i 4');
$sawGreet = $stack !== null && str_contains($stack, 'greet');
$r = command($conn, 'run -i 5');
$ended = $r !== null && str_contains($r, 'status="stopping"');
command($conn, 'stop -i 6');
fclose($conn);

printf("RESULT init_names_xdebug=%s break_hit=%s stack_has_greet=%s run_ended_stopping=%s\n",
    var_export($initOk, true), var_export($hitBreak, true), var_export($sawGreet, true), var_export($ended, true));
exit($initOk && $hitBreak && $sawGreet && $ended ? 0 : 1);
