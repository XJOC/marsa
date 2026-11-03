//! Deterministic content for the three tabs.
//!
//! Every value is a pure function of its index: the same row number gives
//! the same text in every run and in both programs. There is no randomness and
//! no stored copy of the 100,000 rows or 50,000 log lines: like a database
//! cursor, each program asks for the rows it shows. Memory differences
//! between the programs therefore come from the UI toolkit, not from the
//! data.

/// Rows in the Table tab.
pub const TABLE_ROWS: usize = 100_000;

/// Column titles in the Table tab.
pub const TABLE_COLUMNS: [&str; 8] = [
    "id",
    "name",
    "email",
    "city",
    "status",
    "amount",
    "created_at",
    "note",
];

/// Lines in the SQL tab.
pub const SQL_LINES: usize = 5_000;

/// Lines in the Logs tab.
pub const LOG_LINES: usize = 50_000;

/// Rows (or log lines) per `invoke` call in the Tauri app, the way a real
/// database browser pages a result set (task S-09).
pub const BATCH_SIZE: usize = 500;

const FIRST_NAMES: [&str; 16] = [
    "Omar", "Layla", "Yusuf", "Mariam", "Khaled", "Huda", "Samir", "Noura", "Adam", "Sara",
    "Faisal", "Reem", "Tariq", "Dana", "Ziad", "Lina",
];

const LAST_NAMES: [&str; 16] = [
    "Haddad", "Mansour", "Khalil", "Nasser", "Saleh", "Aziz", "Darwish", "Hamdan", "Rashid",
    "Qasim", "Salem", "Bakr", "Farouk", "Jaber", "Karim", "Nour",
];

const CITIES: [&str; 12] = [
    "Riyadh",
    "Jeddah",
    "Cairo",
    "Amman",
    "Dubai",
    "Doha",
    "Kuwait City",
    "Muscat",
    "Beirut",
    "Tunis",
    "Rabat",
    "Manama",
];

const STATUSES: [&str; 4] = ["active", "pending", "suspended", "closed"];

const NOTES: [&str; 10] = [
    "Prefers email contact",
    "Invoice sent, awaiting payment",
    "VIP customer since 2021",
    "Requested a refund last month",
    "Account migrated from the old system",
    "Shipping address needs review",
    "Pays by bank transfer",
    "Subscribed to the newsletter",
    "Asked for an Arabic invoice",
    "No notes",
];

const LOG_TARGETS: [&str; 6] = [
    "marsad::sites",
    "marsad::runtime",
    "marsad::caddy",
    "marsad::php_fpm",
    "marsad::watcher",
    "marsad::queue",
];

const SITES: [&str; 8] = [
    "shop", "blog", "crm", "api", "portal", "admin", "docs", "billing",
];

/// Days in each month of 2026 (not a leap year).
const MONTH_DAYS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Day of the year (0-based) of 2026-11-03, the date of every log line.
const LOG_DAY: u64 = 306;

/// SplitMix64: a fixed bit mixer, so neighbouring rows get unrelated values
/// without any random number generator.
fn mix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Picks an item from a non-empty table with bits of a hash.
fn pick<'a>(items: &[&'a str], bits: u64) -> &'a str {
    items[(bits % items.len() as u64) as usize]
}

/// `(month, day)` of a 0-based day of 2026, both 1-based.
fn month_day(day_of_year: u64) -> (u64, u64) {
    let mut day = day_of_year % 365;
    for (month, days) in MONTH_DAYS.iter().enumerate() {
        if day < *days {
            return (month as u64 + 1, day + 1);
        }
        day -= days;
    }
    // `day < 365` and the months sum to 365, so the loop always returns.
    (12, 31)
}

/// One table row: 8 cells, in [`TABLE_COLUMNS`] order.
pub fn table_row(row: usize) -> [String; 8] {
    let h = mix(row as u64);
    let first = pick(&FIRST_NAMES, h);
    let last = pick(&LAST_NAMES, h >> 4);
    let (month, day) = month_day((h >> 16) % 365);
    let seconds = (h >> 32) % 86_400;
    let cents = (h >> 20) % 10_000_000;
    [
        (row + 1).to_string(),
        format!("{first} {last}"),
        format!(
            "{}.{}{}@example.test",
            first.to_lowercase(),
            last.to_lowercase(),
            row % 1000
        ),
        pick(&CITIES, h >> 8).to_string(),
        pick(&STATUSES, h >> 12).to_string(),
        format!("{}.{:02}", cents / 100, cents % 100),
        format!(
            "2026-{month:02}-{day:02} {:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
        pick(&NOTES, h >> 40).to_string(),
    ]
}

/// Rows `start..start + count`, cut at [`TABLE_ROWS`].
pub fn table_rows(start: usize, count: usize) -> Vec<[String; 8]> {
    let end = start.saturating_add(count).min(TABLE_ROWS);
    (start.min(end)..end).map(table_row).collect()
}

/// One log line: `2026-11-03T00:00:00.000Z LEVEL target: message`.
pub fn log_line(index: usize) -> String {
    let h = mix(index as u64 ^ 0x5151_5151);
    let millis = index as u64 * 173 + h % 173;
    let seconds = millis / 1000;
    let (month, day) = month_day(LOG_DAY);
    let level = match h % 100 {
        0..70 => "INFO",
        70..85 => "DEBUG",
        85..95 => "WARN",
        _ => "ERROR",
    };
    let site = pick(&SITES, h >> 8);
    let number = (h >> 16) % 100_000;
    let duration = (h >> 24) % 900 + 1;
    let message = match (h >> 32) % 6 {
        0 => format!(
            "request handled site={site} method=GET path=/api/items/{number} status=200 duration_ms={duration}"
        ),
        1 => format!(
            "php-fpm pool started site={site} workers={}",
            (h >> 40) % 16 + 1
        ),
        2 => format!("certificate renewed for {site}.test expires_in_days={duration}"),
        3 => format!(
            "slow query site={site} duration_ms={duration} sql=\"SELECT * FROM orders WHERE customer_id = {number}\""
        ),
        4 => format!(
            "file changed path=/Users/dev/Sites/{site}/app/Http/Controllers/OrderController.php"
        ),
        _ => format!(
            "queue job processed site={site} job=App\\Jobs\\SendInvoice id={number} attempts={}",
            (h >> 40) % 3 + 1
        ),
    };
    format!(
        "2026-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z {level:<5} {}: {message}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        millis % 1000,
        pick(&LOG_TARGETS, h >> 4),
    )
}

/// Log lines `start..start + count`, cut at [`LOG_LINES`].
pub fn log_lines(start: usize, count: usize) -> Vec<String> {
    let end = start.saturating_add(count).min(LOG_LINES);
    (start.min(end)..end).map(log_line).collect()
}

/// The SQL tab's text: exactly [`SQL_LINES`] lines, joined with `\n`.
pub fn sql_text() -> String {
    let mut lines: Vec<String> = Vec::with_capacity(SQL_LINES + 16);
    let mut block = 0;
    while lines.len() < SQL_LINES {
        push_sql_block(&mut lines, block);
        block += 1;
    }
    lines.truncate(SQL_LINES);
    lines.join("\n")
}

fn push_sql_block(lines: &mut Vec<String>, block: usize) {
    let city = pick(&CITIES, block as u64);
    let status = pick(&STATUSES, block as u64);
    match block % 4 {
        0 => {
            lines.push(format!("-- Table {block}: customer accounts for {city}"));
            lines.push(format!("CREATE TABLE IF NOT EXISTS customers_{block} ("));
            lines.push("    id BIGINT PRIMARY KEY,".to_string());
            lines.push("    name VARCHAR(120) NOT NULL,".to_string());
            lines.push("    email VARCHAR(255) UNIQUE,".to_string());
            lines.push(format!("    city VARCHAR(64) DEFAULT '{city}',"));
            lines.push(
                "    status VARCHAR(16) NOT NULL CHECK (status IN ('active', 'pending', 'suspended', 'closed')),"
                    .to_string(),
            );
            lines.push("    amount DECIMAL(12, 2) DEFAULT 0.00,".to_string());
            lines.push("    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP".to_string());
            lines.push(");".to_string());
        }
        1 => {
            lines.push(format!(
                "-- Report {block}: paid orders per customer in {city}"
            ));
            lines.push(
                "SELECT c.id, c.name, COUNT(o.id) AS orders, SUM(o.amount) AS total".to_string(),
            );
            lines.push(format!("FROM customers_{} AS c", block - 1));
            lines.push(
                "LEFT JOIN orders AS o ON o.customer_id = c.id AND o.status = 'paid'".to_string(),
            );
            lines.push(format!(
                "WHERE c.created_at >= '2026-01-01' AND c.city = '{city}'"
            ));
            lines.push("GROUP BY c.id, c.name".to_string());
            lines.push(format!("HAVING SUM(o.amount) > {}", block * 10));
            lines.push("ORDER BY total DESC".to_string());
            lines.push("LIMIT 50;".to_string());
        }
        2 => {
            lines.push(format!("-- Seed {block}: three sample rows"));
            lines.push(format!(
                "INSERT INTO customers_{} (id, name, email, city, status, amount) VALUES",
                block - 2
            ));
            for offset in 0..3 {
                let row = table_row(block * 3 + offset);
                let end = if offset == 2 { ";" } else { "," };
                lines.push(format!(
                    "    ({}, '{}', '{}', '{}', '{}', {}){end}",
                    row[0], row[1], row[2], row[3], row[4], row[5]
                ));
            }
        }
        _ => {
            lines.push(format!("-- Cleanup {block}: close {status} accounts"));
            lines.push(format!("UPDATE customers_{}", block - 3));
            lines.push("SET status = 'closed', amount = 0".to_string());
            lines.push(format!("WHERE status = '{status}'"));
            lines.push(
                "  AND id NOT IN (SELECT customer_id FROM orders WHERE created_at > NOW() - INTERVAL '90 days');"
                    .to_string(),
            );
        }
    }
    lines.push(String::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_deterministic() {
        assert_eq!(table_row(0), table_row(0));
        assert_eq!(table_row(99_999), table_row(99_999));
        assert_ne!(table_row(1), table_row(2));
        assert_eq!(table_row(41)[0], "42");
    }

    #[test]
    fn row_batches_stop_at_the_end() {
        assert_eq!(table_rows(0, BATCH_SIZE).len(), BATCH_SIZE);
        assert_eq!(table_rows(TABLE_ROWS - 10, BATCH_SIZE).len(), 10);
        assert!(table_rows(TABLE_ROWS + 5, BATCH_SIZE).is_empty());
        assert!(table_rows(usize::MAX, usize::MAX).is_empty());
    }

    #[test]
    fn dates_stay_in_2026() {
        assert_eq!(month_day(0), (1, 1));
        assert_eq!(month_day(31), (2, 1));
        assert_eq!(month_day(306), (11, 3));
        assert_eq!(month_day(364), (12, 31));
        for row in 0..2_000 {
            assert!(table_row(row)[6].starts_with("2026-"));
        }
    }

    #[test]
    fn log_lines_have_the_expected_shape() {
        let first = log_line(0);
        assert!(first.starts_with("2026-11-03T00:00:00."), "{first}");
        assert_eq!(log_lines(LOG_LINES - 1, BATCH_SIZE).len(), 1);
        let last = log_line(LOG_LINES - 1);
        assert!(last.starts_with("2026-11-03T02:"), "{last}");
    }

    #[test]
    fn sql_text_has_exactly_the_planned_lines() {
        let text = sql_text();
        assert_eq!(text.lines().count(), SQL_LINES);
        assert_eq!(text, sql_text());
        assert!(text.starts_with("-- Table 0"));
    }
}
