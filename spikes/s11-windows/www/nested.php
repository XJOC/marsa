<?php
// A request that calls the same site again and waits for the answer, like a
// Laravel page that calls its own API. With one php-cgi process this can
// never finish: the only worker is busy with the outer request.
$context = stream_context_create([
    'http' => ['timeout' => 10, 'header' => "Host: spike.test\r\n"],
]);
$inner = @file_get_contents('http://127.0.0.1:18080/fast.php', false, $context);
if ($inner === false) {
    http_response_code(504);
    echo 'inner request failed';
    return;
}
echo 'outer ', getmypid(), ' inner ', $inner;
