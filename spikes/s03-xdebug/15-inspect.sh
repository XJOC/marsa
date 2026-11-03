#!/usr/bin/env bash
# S-03 step 3: inspect the prebuilt static-php-cli PHP (cli): version, ABI fields,
# linked libraries, signature, and whether it exports Zend symbols (needed by a dlopen'd .so).
# Run:  ./15-inspect.sh      Output: results/prebuilt-inspect.txt
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
P="$WORK/php-cli/php"
F="$WORK/php-fpm/php-fpm"
{
  echo "== php -v";                 "$P" -v
  echo; echo "== php -n -m (one line)"; "$P" -n -m | grep -v '^$' | tr '\n' ' '; echo
  echo; echo "== php -n -i (ABI fields)"
  "$P" -n -i | grep -E '^(PHP API|PHP Extension|Zend Extension|Zend Extension Build|PHP Extension Build|Debug Build|Thread Safety|Zend Signal Handling|extension_dir|Loaded Configuration File|Scan this dir|Built by|Server API)'
  echo; echo "== configure command (first 400 chars)"
  "$P" -n -i | grep -E '^Configure Command' | cut -c1-400
  echo; echo "== size and sha256"
  stat -f '%z bytes  %N' "$P" "$F"; shasum -a 256 "$P" "$F"
  echo; echo "== otool -L php";     otool -L "$P"
  echo; echo "== file / codesign";  file "$P"; codesign -dv "$P" 2>&1 | grep -E 'Format|Signature|flags'
  echo; echo "== exported symbols (nm -gU)"
  echo "total: $(nm -gU "$P" | wc -l)   zend_*: $(nm -gU "$P" | grep -c ' _zend_')   php_*: $(nm -gU "$P" | grep -c ' _php_')"
  nm -gU "$P" | grep -E ' _(zend_register_ini_entries|zend_execute_ex|zend_error_cb|zend_extension_dispatch_message|zend_get_executed_filename|zend_observer_fcall_register)$'
  echo; echo "== php-fpm -v";       "$F" -v
  echo; echo "== php-fpm -n -m (Zend Modules part)"; "$F" -n -m | sed -n '/Zend Modules/,$p'
} > "$RESULTS/prebuilt-inspect.txt" 2>&1
cat "$RESULTS/prebuilt-inspect.txt" | cut -c1-200
