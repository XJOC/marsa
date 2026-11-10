<?php
// A request that waits 100 ms, like a slow database query.
usleep(100000);
echo 'ok ', getmypid();
