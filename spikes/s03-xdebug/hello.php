<?php
// S-03 debug target: ten lines, one function call. The listener sets a breakpoint on line 5.
function greet(string $name): string
{
    $msg = "Hello, " . $name;
    return $msg;
}
$result = greet("Marsa");
echo $result, "\n";
echo "done\n";
