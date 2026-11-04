<?php
// S-10 runtime checks. Run with the PHP you just built, with no ini file:
//   buildroot/bin/php -n spikes/s10-php-build/runtime-checks.php
// Each line is STATUS<TAB>name<TAB>detail, where STATUS is PASS, FAIL, or INFO.
// The exit code is 1 if any line is FAIL.
// INFO lines never fail the run (network, environment details).
//
// Optional environment: CAFILE = a CA bundle to try for the HTTPS check.

$fails = 0;

function t(string $name, callable $f): void
{
    global $fails;
    try {
        $r = $f();
        if ($r === true) {
            echo "PASS\t$name\t\n";
        } else {
            $fails++;
            echo "FAIL\t$name\t" . (is_string($r) ? $r : var_export($r, true)) . "\n";
        }
    } catch (Throwable $e) {
        $fails++;
        echo "FAIL\t$name\t" . get_class($e) . ': ' . $e->getMessage() . "\n";
    }
}

function info(string $name, $value): void
{
    echo "INFO\t$name\t" . (is_string($value) ? $value : json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE)) . "\n";
}

function eq($actual, $expected): bool|string
{
    return $actual === $expected ? true : 'expected ' . var_export($expected, true) . ', got ' . var_export($actual, true);
}

info('php', [PHP_VERSION, PHP_OS, PHP_SAPI, ZEND_THREAD_SAFE ? 'ZTS' : 'NTS', PHP_DEBUG ? 'debug' : 'release']);
info('extension_dir', PHP_EXTENSION_DIR);

// --- Databases -------------------------------------------------------------
t('pdo_sqlite: in-memory query', function () {
    $pdo = new PDO('sqlite::memory:');
    $pdo->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);
    $pdo->exec('create table t (id integer primary key, name text)');
    $st = $pdo->prepare('insert into t (name) values (?)');
    $st->execute(['marsa']);
    return eq($pdo->query('select name from t where id = 1')->fetchColumn(), 'marsa');
});
t('sqlite3: in-memory query', function () {
    $db = new SQLite3(':memory:');
    return eq($db->querySingle('select 40 + 2'), 42);
});
info('sqlite_version', (new PDO('sqlite::memory:'))->query('select sqlite_version()')->fetchColumn());
t('pdo drivers: mysql, pgsql, sqlite', function () {
    $d = PDO::getAvailableDrivers();
    $missing = array_diff(['mysql', 'pgsql', 'sqlite'], $d);
    return $missing === [] ? true : 'missing ' . implode(',', $missing) . '; have ' . implode(',', $d);
});
t('mysqlnd + pgsql loaded', fn () => extension_loaded('mysqlnd') && extension_loaded('pgsql') ? true : 'not loaded');
info('pgsql libpq', function_exists('pg_version') ? 'pg_version() exists' : 'no pg_version');

// --- intl, mbstring, iconv -------------------------------------------------
t('intl: NumberFormatter en_US', fn () => eq((new NumberFormatter('en_US', NumberFormatter::DECIMAL))->format(1234567.891), '1,234,567.891'));
t('intl: ICU locale data (ar digits)', fn () => eq((new NumberFormatter('ar_EG@numbers=arab', NumberFormatter::DECIMAL))->format(123), '١٢٣'));
t('intl: Locale display language', fn () => eq(Locale::getDisplayLanguage('ar', 'en'), 'Arabic'));
t('intl: Collator + Normalizer', function () {
    $c = new Collator('en_US');
    $a = ['b', 'a', 'C'];
    $c->sort($a);
    return $a === ['a', 'b', 'C'] && Normalizer::isNormalized('é') ? true : json_encode($a);
});
t('intl: IntlDateFormatter', fn () => (new IntlDateFormatter('en_US', IntlDateFormatter::SHORT, IntlDateFormatter::NONE, 'UTC'))->format(0) === '1/1/70' ? true : 'unexpected');
info('icu', [INTL_ICU_VERSION, INTL_ICU_DATA_VERSION]);
t('mbstring: Arabic length and case', fn () => eq([mb_strlen('مرسى'), mb_strtoupper('é')], [4, 'É']));
t('iconv: UTF-8 to ISO-8859-1', fn () => eq(strlen(iconv('UTF-8', 'ISO-8859-1', 'é')), 1));
info('iconv impl', [ICONV_IMPL, ICONV_VERSION]);

// --- Crypto and TLS --------------------------------------------------------
t('openssl: aes-256-gcm round trip', function () {
    $key = random_bytes(32);
    $iv = random_bytes(12);
    $c = openssl_encrypt('hello', 'aes-256-gcm', $key, OPENSSL_RAW_DATA, $iv, $tag);
    return eq(openssl_decrypt($c, 'aes-256-gcm', $key, OPENSSL_RAW_DATA, $iv, $tag), 'hello');
});
t('sodium: secretbox round trip', function () {
    $key = sodium_crypto_secretbox_keygen();
    $n = random_bytes(SODIUM_CRYPTO_SECRETBOX_NONCEBYTES);
    return eq(sodium_crypto_secretbox_open(sodium_crypto_secretbox('x', $n, $key), $n, $key), 'x');
});
t('password_hash bcrypt', fn () => password_verify('p', password_hash('p', PASSWORD_BCRYPT)) ? true : 'verify failed');
info('password_algos', password_algos());
info('openssl', [OPENSSL_VERSION_TEXT, ini_get('openssl.cafile'), openssl_get_cert_locations()['default_cert_file'] ?? '']);
t('curl: https + openssl', function () {
    $v = curl_version();
    return in_array('https', $v['protocols'], true) && stripos($v['ssl_version'], 'openssl') !== false ? true : json_encode($v);
});
info('curl', ['version' => curl_version()['version'], 'ssl' => curl_version()['ssl_version'], 'libz' => curl_version()['libz_version'], 'http2' => (bool) (curl_version()['features'] & CURL_VERSION_HTTP2)]);

// HTTPS with the default CA lookup, then with CAFILE. Network: INFO only.
$ctx = stream_context_create(['http' => ['timeout' => 15, 'ignore_errors' => true]]);
$r = @file_get_contents('https://repo.packagist.org/packages.json', false, $ctx);
info('https default CA', $r !== false ? 'works' : 'fails (static OpenSSL cannot find the system CA bundle: set openssl.cafile and curl.cainfo)');
if (($cafile = getenv('CAFILE')) && is_file($cafile)) {
    $ctx = stream_context_create(['http' => ['timeout' => 15, 'ignore_errors' => true], 'ssl' => ['cafile' => $cafile]]);
    $r = @file_get_contents('https://repo.packagist.org/packages.json', false, $ctx);
    info("https with cafile=$cafile", $r !== false ? 'works' : 'fails');
}

// --- Images, archives, XML -------------------------------------------------
t('gd: PNG, JPEG, WebP, FreeType', function () {
    $i = gd_info();
    $missing = [];
    foreach (['PNG Support', 'JPEG Support', 'WebP Support', 'FreeType Support'] as $k) {
        if (empty($i[$k])) {
            $missing[] = $k;
        }
    }
    if ($missing !== []) {
        return 'missing ' . implode(', ', $missing);
    }
    $im = imagecreatetruecolor(8, 8);
    ob_start();
    imagepng($im);
    $png = ob_get_clean();
    return str_starts_with($png, "\x89PNG") ? true : 'bad png';
});
info('gd', gd_info()['GD Version'] ?? '');
t('zip: write and read', function () {
    $f = tempnam(sys_get_temp_dir(), 'marsa') . '.zip';
    $z = new ZipArchive();
    $z->open($f, ZipArchive::CREATE);
    $z->addFromString('a.txt', 'hi');
    $z->close();
    $z = new ZipArchive();
    $z->open($f);
    $r = $z->getFromName('a.txt');
    $z->close();
    @unlink($f);
    return eq($r, 'hi');
});
t('dom + simplexml + xmlreader + xmlwriter', function () {
    $xml = '<a><b id="1">x</b></a>';
    $d = new DOMDocument();
    $d->loadXML($xml);
    $x = (new DOMXPath($d))->evaluate('string(//b/@id)');
    $s = (string) simplexml_load_string($xml)->b;
    $r = new XMLReader();
    $r->XML($xml);
    $r->read();
    $w = new XMLWriter();
    $w->openMemory();
    $w->startElement('z');
    $w->endElement();
    return eq([$x, $s, $r->name, trim($w->outputMemory())], ['1', 'x', 'a', '<z/>']);
});
info('libxml', LIBXML_DOTTED_VERSION);
t('phar class', fn () => class_exists('Phar') ? true : 'no Phar');
t('zlib: gzencode round trip', fn () => eq(gzdecode(gzencode('abc')), 'abc'));
t('bcmath', fn () => eq(bcadd('0.1', '0.2', 1), '0.3'));
t('exif + fileinfo', function () {
    $f = new finfo(FILEINFO_MIME_TYPE);
    return function_exists('exif_read_data') ? eq([$f->buffer('hello'), $f->buffer("%PDF-1.4\n")], ['text/plain', 'application/pdf']) : 'no exif';
});
t('tokenizer + ctype + filter', fn () => eq([count(token_get_all('<?php echo 1;')) > 0, ctype_digit('12'), filter_var('a@b.co', FILTER_VALIDATE_EMAIL)], [true, true, 'a@b.co']));

// --- Process, sockets, cache, redis, readline, ftp --------------------------
t('pcntl + posix', fn () => function_exists('pcntl_fork') && posix_getpid() > 0 ? true : 'missing');
t('sockets: create TCP socket', fn () => socket_create(AF_INET, SOCK_STREAM, SOL_TCP) !== false ? true : 'failed');
t('redis: class and constants', fn () => class_exists('Redis') && defined('Redis::SERIALIZER_PHP') ? true : 'no Redis');
info('redis', [phpversion('redis'), 'zstd' => defined('Redis::COMPRESSION_ZSTD'), 'lz4' => defined('Redis::COMPRESSION_LZ4'), 'igbinary' => defined('Redis::SERIALIZER_IGBINARY')]);
t('apcu functions', fn () => function_exists('apcu_store') ? true : 'no apcu');
t('readline functions', fn () => function_exists('readline') ? true : 'no readline');
info('readline', [READLINE_LIB]);
t('ftp functions', fn () => function_exists('ftp_connect') ? true : 'no ftp');
t('opcache functions', fn () => function_exists('opcache_get_status') ? true : 'no opcache');

exit($fails > 0 ? 1 : 0);
