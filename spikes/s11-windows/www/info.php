<?php
// What the pool looks like from inside PHP.
header('Content-Type: text/plain');
echo 'php=', PHP_VERSION, ' sapi=', PHP_SAPI, ' pid=', getmypid(), "\n";
$opcache = function_exists('opcache_get_status') ? opcache_get_status(false) : null;
echo 'opcache=', json_encode($opcache['opcache_enabled'] ?? 'not loaded'), "\n";
echo 'extensions=', implode(',', get_loaded_extensions()), "\n";
