//! The SQL text of the browser's catalog reads: the databases of a server,
//! the tables of a source, and the columns of a table. Each is fixed text;
//! a table's name is bound. Each engine's statement gives the same columns,
//! and every column is text or null (numbers are cast to text), so the daemon
//! reads all three engines one way, whatever types their drivers know.

use super::Engine;
use crate::types::DbServer;

/// The databases of a server, by name, without the server's own. Gives one
/// column: the name. Binds nothing.
pub fn databases_sql(server: DbServer) -> &'static str {
    match server {
        DbServer::Mysql => {
            "SELECT schema_name FROM information_schema.schemata \
             WHERE schema_name NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys') \
             ORDER BY schema_name"
        }
        DbServer::Postgres => {
            "SELECT datname::text FROM pg_catalog.pg_database \
             WHERE datallowconn AND NOT datistemplate \
             AND datname NOT IN ('postgres', 'template0', 'template1') \
             ORDER BY datname"
        }
    }
}

/// The tables and views of the source, by schema and name. Binds nothing.
/// Gives four columns: the schema (null outside PostgreSQL), the name, the
/// kind (`table` or `view`), and the server's estimate of the rows (digits,
/// or null when it has none).
pub fn tables_sql(engine: Engine) -> &'static str {
    match engine {
        Engine::Mysql => {
            "SELECT NULL, table_name, \
             CASE WHEN table_type = 'BASE TABLE' THEN 'table' ELSE 'view' END, \
             CAST(table_rows AS CHAR) \
             FROM information_schema.tables \
             WHERE table_schema = DATABASE() \
             ORDER BY table_name"
        }
        // Partitions are left out: their table lists their rows. Every schema
        // whose name starts with `pg_` is PostgreSQL's own.
        Engine::Postgres => {
            "SELECT n.nspname::text, c.relname::text, \
             CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END, \
             CASE WHEN c.relkind IN ('r', 'm') AND c.reltuples >= 0 THEN c.reltuples::bigint::text END \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition \
             AND n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%' \
             ORDER BY n.nspname, c.relname"
        }
        Engine::Sqlite => {
            "SELECT NULL, name, type, NULL \
             FROM sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name"
        }
    }
}

/// The columns of one table, in their order. Binds the table's name; in
/// PostgreSQL first its schema (null for the current one), then its name.
/// Gives six columns: the name, the type as the server writes it, nullable
/// (`1` or `0`), the default (or null), the key (`primary`, `unique` for a
/// unique index on that column alone, or null), and the column's place in
/// the primary key (`1` and up; `0` when not in it).
pub fn columns_sql(engine: Engine) -> &'static str {
    match engine {
        // `UNI` marks a column with a unique index of its own: the first
        // column of a unique index on several columns is `MUL`.
        Engine::Mysql => {
            "SELECT c.column_name, c.column_type, \
             CASE WHEN c.is_nullable = 'YES' THEN '1' ELSE '0' END, c.column_default, \
             CASE c.column_key WHEN 'PRI' THEN 'primary' WHEN 'UNI' THEN 'unique' END, \
             CAST(COALESCE(k.ordinal_position, 0) AS CHAR) \
             FROM information_schema.columns c \
             LEFT JOIN information_schema.key_column_usage k \
             ON k.table_schema = c.table_schema AND k.table_name = c.table_name \
             AND k.column_name = c.column_name AND k.constraint_name = 'PRIMARY' \
             WHERE c.table_schema = DATABASE() AND c.table_name = ? \
             ORDER BY c.ordinal_position"
        }
        Engine::Postgres => {
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
             CASE WHEN a.attnotnull THEN '0' ELSE '1' END, \
             pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
             CASE WHEN pk.place IS NOT NULL THEN 'primary' \
             WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_index u \
             WHERE u.indrelid = c.oid AND u.indisunique AND u.indnkeyatts = 1 \
             AND u.indkey[0] = a.attnum) THEN 'unique' END, \
             COALESCE(pk.place, 0)::text \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum \
             LEFT JOIN LATERAL (SELECT k.place::integer AS place \
             FROM pg_catalog.pg_index i, unnest(i.indkey) WITH ORDINALITY AS k(attnum, place) \
             WHERE i.indrelid = c.oid AND i.indisprimary AND k.attnum = a.attnum) pk ON true \
             WHERE n.nspname = COALESCE($1, current_schema()) AND c.relname = $2 \
             AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum"
        }
        // SQLite lets a primary key column other than an INTEGER one hold a
        // null, but a key with a null is not useful to show as nullable.
        Engine::Sqlite => {
            "SELECT p.name, p.type, \
             CASE WHEN p.\"notnull\" = 0 AND p.pk = 0 THEN '1' ELSE '0' END, p.dflt_value, \
             CASE WHEN p.pk > 0 THEN 'primary' \
             WHEN EXISTS (SELECT 1 FROM pragma_index_list(?1) AS l \
             WHERE l.\"unique\" = 1 AND l.origin <> 'pk' \
             AND (SELECT COUNT(*) FROM pragma_index_info(l.name)) = 1 \
             AND (SELECT i.name FROM pragma_index_info(l.name) AS i) = p.name) THEN 'unique' END, \
             CAST(p.pk AS TEXT) \
             FROM pragma_table_info(?1) AS p \
             ORDER BY p.cid"
        }
    }
}
