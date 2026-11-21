//! Every error code that can reach a client (the `marsa` command or the
//! window), in one place. A client keeps a text for each code of [`ALL`], and
//! a test in each client fails when a code has none. To add a code: add the
//! constant here and to [`ALL`], use the constant where the code is made, and
//! add a text to `cli/strings/errors.rs` and to `gui/strings/errors.rs`.

pub const AUTOSTART_NO_HOME: &str = "autostart_no_home";
pub const AUTOSTART_PATH_INVALID: &str = "autostart_path_invalid";
pub const AUTOSTART_READ: &str = "autostart_read";
pub const AUTOSTART_UNSUPPORTED: &str = "autostart_unsupported";
pub const AUTOSTART_WRITE: &str = "autostart_write";
pub const CA_NOT_READY: &str = "ca_not_ready";
pub const CA_UNREADABLE: &str = "ca_unreadable";
pub const CADDY_ADMIN_UNREACHABLE: &str = "caddy_admin_unreachable";
pub const CADDY_CONFIG_REJECTED: &str = "caddy_config_rejected";
pub const CADDY_CONFIG_WRITE: &str = "caddy_config_write";
pub const CADDY_NOT_INSTALLED: &str = "caddy_not_installed";
pub const CADDY_NOT_STARTED: &str = "caddy_not_started";
pub const CADDY_ORPHAN_STUCK: &str = "caddy_orphan_stuck";
pub const CADDY_OTHER_MARSA: &str = "caddy_other_marsa";
pub const CADDY_PATH_NOT_UTF8: &str = "caddy_path_not_utf8";
pub const CADDY_RECIPE_INVALID: &str = "caddy_recipe_invalid";
pub const CADDY_START_TIMEOUT: &str = "caddy_start_timeout";
pub const CADDY_UNSUPPORTED_PLATFORM: &str = "caddy_unsupported_platform";
pub const CHECKSUM_MISMATCH: &str = "checksum_mismatch";
pub const CONFIG_INVALID: &str = "config_invalid";
pub const CONFIG_NO_HOME: &str = "config_no_home";
pub const CONFIG_READ: &str = "config_read";
pub const CONFIG_WRITE: &str = "config_write";
pub const DAEMON_ALREADY_RUNNING: &str = "daemon_already_running";
pub const DAEMON_API_TOO_NEW: &str = "daemon_api_too_new";
pub const DAEMON_BAD_RESPONSE: &str = "daemon_bad_response";
pub const DAEMON_FILE_INSECURE: &str = "daemon_file_insecure";
pub const DAEMON_FILE_INVALID: &str = "daemon_file_invalid";
pub const DAEMON_FILE_MISSING: &str = "daemon_file_missing";
pub const DAEMON_FILE_WRITE: &str = "daemon_file_write";
pub const DAEMON_NOT_RUNNING: &str = "daemon_not_running";
pub const DAEMON_START_FAILED: &str = "daemon_start_failed";
pub const DAEMON_STOP_FAILED: &str = "daemon_stop_failed";
pub const DAEMON_UNREACHABLE: &str = "daemon_unreachable";
pub const DNS_NOT_RUNNING: &str = "dns_not_running";
pub const DNS_PORT_IN_USE: &str = "dns_port_in_use";
pub const DNS_START_FAILED: &str = "dns_start_failed";
pub const DOWNLOAD_FAILED: &str = "download_failed";
pub const DOWNLOAD_HTTP_STATUS: &str = "download_http_status";
pub const DOWNLOAD_INSECURE_URL: &str = "download_insecure_url";
pub const DOWNLOAD_IO: &str = "download_io";
pub const DOWNLOAD_SIZE_MISMATCH: &str = "download_size_mismatch";
pub const DOWNLOAD_TOO_LARGE: &str = "download_too_large";
pub const EXITED: &str = "exited";
pub const EXTRACT_CORRUPT: &str = "extract_corrupt";
pub const EXTRACT_DUPLICATE: &str = "extract_duplicate";
pub const EXTRACT_IO: &str = "extract_io";
pub const EXTRACT_TOO_LARGE: &str = "extract_too_large";
pub const EXTRACT_UNSAFE_LINK: &str = "extract_unsafe_link";
pub const EXTRACT_UNSAFE_PATH: &str = "extract_unsafe_path";
pub const EXTRACT_UNSUPPORTED_ENTRY: &str = "extract_unsupported_entry";
pub const FOLDER_NOT_PARKED: &str = "folder_not_parked";
pub const HOST_REJECTED: &str = "host_rejected";
pub const HOSTS_BLOCK_BROKEN: &str = "hosts_block_broken";
pub const HOSTS_NOT_USED: &str = "hosts_not_used";
pub const HOSTS_READ: &str = "hosts_read";
pub const HOSTS_SITES_UNKNOWN: &str = "hosts_sites_unknown";
pub const HOSTS_TOO_MANY_NAMES: &str = "hosts_too_many_names";
pub const HOSTS_WRITE: &str = "hosts_write";
pub const INSTALL_BUSY: &str = "install_busy";
pub const INSTALL_CHECK_FAILED: &str = "install_check_failed";
pub const INSTALL_CRASHED: &str = "install_crashed";
pub const INSTALL_INTERRUPTED: &str = "install_interrupted";
pub const INSTALL_IO: &str = "install_io";
pub const INSTALL_MANIFEST_MISMATCH: &str = "install_manifest_mismatch";
pub const INSTALL_MISSING_VC_RUNTIME: &str = "install_missing_vc_runtime";
pub const INSTALL_NO_SUCH_VERSION: &str = "install_no_such_version";
pub const INSTALL_OS_TOO_OLD: &str = "install_os_too_old";
pub const INSTALL_SERIAL_CORRUPT: &str = "install_serial_corrupt";
pub const INSTALL_UNSAFE_NAME: &str = "install_unsafe_name";
pub const INSTALL_UNSUPPORTED_PLATFORM: &str = "install_unsupported_platform";
pub const INSTALL_XZ_MISSING: &str = "install_xz_missing";
pub const INTERNAL_ERROR: &str = "internal_error";
pub const INVALID_BODY: &str = "invalid_body";
pub const INVALID_ID: &str = "invalid_id";
pub const INVALID_PATH: &str = "invalid_path";
pub const INVALID_QUERY: &str = "invalid_query";
pub const INVALID_VERSION: &str = "invalid_version";
pub const MANIFEST_BAD_SIGNATURE: &str = "manifest_bad_signature";
pub const MANIFEST_INVALID: &str = "manifest_invalid";
pub const MANIFEST_NO_TRUSTED_KEY: &str = "manifest_no_trusted_key";
pub const MANIFEST_ROLLBACK: &str = "manifest_rollback";
pub const MANIFEST_SCHEMA_UNSUPPORTED: &str = "manifest_schema_unsupported";
pub const MANIFEST_TOO_LARGE: &str = "manifest_too_large";
pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
pub const MYSQL_INIT_FAILED: &str = "mysql_init_failed";
pub const MYSQL_NOT_INSTALLED: &str = "mysql_not_installed";
pub const MYSQL_ORPHAN_STUCK: &str = "mysql_orphan_stuck";
pub const MYSQL_PATH_UNSUPPORTED: &str = "mysql_path_unsupported";
pub const MYSQL_START_TIMEOUT: &str = "mysql_start_timeout";
pub const MYSQL_UNSUPPORTED_PLATFORM: &str = "mysql_unsupported_platform";
pub const NO_CURRENT_DIR: &str = "no_current_dir";
pub const NO_DEFAULT_VERSION: &str = "no_default_version";
pub const NO_MATCHING_VERSION: &str = "no_matching_version";
pub const NO_VERSION: &str = "no_version";
pub const NOT_FOUND: &str = "not_found";
pub const NOT_INSTALLED: &str = "not_installed";
pub const NSS_ADD_FAILED: &str = "nss_add_failed";
pub const NSS_DB_REFUSED: &str = "nss_db_refused";
pub const NSS_REMOVE_FAILED: &str = "nss_remove_failed";
pub const NSS_TOOL_MISSING: &str = "nss_tool_missing";
pub const OP_NOT_IMPLEMENTED: &str = "op_not_implemented";
pub const OP_NOT_SUPPORTED: &str = "op_not_supported";
pub const OPEN_FAILED: &str = "open_failed";
pub const OPEN_NO_PROGRAM: &str = "open_no_program";
pub const ORIGIN_REJECTED: &str = "origin_rejected";
pub const PF_DELETE: &str = "pf_delete";
pub const PF_FLUSH_FAILED: &str = "pf_flush_failed";
pub const PF_HELPER_UNREADABLE: &str = "pf_helper_unreadable";
pub const PF_LOAD_FAILED: &str = "pf_load_failed";
pub const PF_WRITE: &str = "pf_write";
pub const PHP_CONFIG_WRITE: &str = "php_config_write";
pub const PHP_FPM_NOT_INSTALLED: &str = "php_fpm_not_installed";
pub const PHP_FPM_ORPHAN_STUCK: &str = "php_fpm_orphan_stuck";
pub const PHP_FPM_UNAVAILABLE: &str = "php_fpm_unavailable";
pub const PHP_PATH_NOT_ALLOWED: &str = "php_path_not_allowed";
pub const PORT_IN_USE: &str = "port_in_use";
pub const PORTS_APPLY_FAILED: &str = "ports_apply_failed";
pub const PORTS_DELETE: &str = "ports_delete";
pub const PORTS_FOREIGN: &str = "ports_foreign";
pub const PORTS_NOT_CHECKED: &str = "ports_not_checked";
pub const PORTS_READ: &str = "ports_read";
pub const PORTS_RESTORE_FAILED: &str = "ports_restore_failed";
pub const PORTS_WRITE: &str = "ports_write";
pub const POSTGRES_INIT_FAILED: &str = "postgres_init_failed";
pub const POSTGRES_NOT_INSTALLED: &str = "postgres_not_installed";
pub const POSTGRES_ORPHAN_STUCK: &str = "postgres_orphan_stuck";
pub const POSTGRES_SHARED_MEMORY_BUSY: &str = "postgres_shared_memory_busy";
pub const POSTGRES_START_TIMEOUT: &str = "postgres_start_timeout";
pub const POSTGRES_UNSUPPORTED_PLATFORM: &str = "postgres_unsupported_platform";
pub const PROGRAM_NOT_ABSOLUTE: &str = "program_not_absolute";
pub const RANDOM_UNAVAILABLE: &str = "random_unavailable";
pub const RECIPE_BAD_PATH: &str = "recipe_bad_path";
pub const RECIPE_BAD_TEMPLATE: &str = "recipe_bad_template";
pub const RECIPE_INVALID: &str = "recipe_invalid";
pub const RECIPE_UNKNOWN_PLATFORM: &str = "recipe_unknown_platform";
pub const REDIRECT_PORT_HELD: &str = "redirect_port_held";
pub const RESOLVED_CHECK_FAILED: &str = "resolved_check_failed";
pub const RESOLVED_DELETE: &str = "resolved_delete";
pub const RESOLVED_FOREIGN: &str = "resolved_foreign";
pub const RESOLVED_NOT_RUNNING: &str = "resolved_not_running";
pub const RESOLVED_READ: &str = "resolved_read";
pub const RESOLVED_RESTART_FAILED: &str = "resolved_restart_failed";
pub const RESOLVED_STOPPED: &str = "resolved_stopped";
pub const RESOLVED_SYSTEMD_UNKNOWN: &str = "resolved_systemd_unknown";
pub const RESOLVED_TOOL_MISSING: &str = "resolved_tool_missing";
pub const RESOLVED_UNIT_FAILED: &str = "resolved_unit_failed";
pub const RESOLVED_USE_HOSTS: &str = "resolved_use_hosts";
pub const RESOLVED_WRITE: &str = "resolved_write";
pub const RESOLVER_DELETE: &str = "resolver_delete";
pub const RESOLVER_FOREIGN: &str = "resolver_foreign";
pub const RESOLVER_READ: &str = "resolver_read";
pub const RESOLVER_WRITE: &str = "resolver_write";
pub const RUNTIMES_READ: &str = "runtimes_read";
pub const SERVICE_BUSY: &str = "service_busy";
pub const SERVICE_DATA_DIR_BUSY: &str = "service_data_dir_busy";
pub const SERVICE_ELEVATED: &str = "service_elevated";
pub const SERVICE_MANAGED_BY_SITES: &str = "service_managed_by_sites";
pub const SERVICE_NOT_FOUND: &str = "service_not_found";
pub const SERVICE_PORT_BUSY: &str = "service_port_busy";
pub const SERVICE_PORT_UNKNOWN: &str = "service_port_unknown";
pub const SETUP_CADDY_PORTS_FAILED: &str = "setup_caddy_ports_failed";
pub const SETUP_CANCELLED: &str = "setup_cancelled";
pub const SETUP_HELPER_BUSY: &str = "setup_helper_busy";
pub const SETUP_HELPER_FAILED: &str = "setup_helper_failed";
pub const SETUP_HELPER_INSTALL_FAILED: &str = "setup_helper_install_failed";
pub const SETUP_HELPER_INSTALL_RUNNING: &str = "setup_helper_install_running";
pub const SETUP_HELPER_MISSING: &str = "setup_helper_missing";
pub const SETUP_HELPER_NO_ANSWER: &str = "setup_helper_no_answer";
pub const SETUP_HELPER_NOT_THE_SERVICE: &str = "setup_helper_not_the_service";
pub const SETUP_HELPER_PARTIAL: &str = "setup_helper_partial";
pub const SETUP_HELPER_PIPE_FAILED: &str = "setup_helper_pipe_failed";
pub const SETUP_HELPER_PROTECTED_FOLDER: &str = "setup_helper_protected_folder";
pub const SETUP_HELPER_REJECTED: &str = "setup_helper_rejected";
pub const SETUP_HELPER_RUNNING: &str = "setup_helper_running";
pub const SETUP_HELPER_UNSAFE: &str = "setup_helper_unsafe";
pub const SETUP_HOSTS_NOT_REMOVED: &str = "setup_hosts_not_removed";
pub const SETUP_HOSTS_NOT_WRITTEN: &str = "setup_hosts_not_written";
pub const SETUP_INTERRUPTED: &str = "setup_interrupted";
pub const SETUP_NOT_AUTHORIZED: &str = "setup_not_authorized";
pub const SETUP_NOT_SUPPORTED: &str = "setup_not_supported";
pub const SETUP_PKEXEC_MISSING: &str = "setup_pkexec_missing";
pub const SETUP_PROMPT_FAILED: &str = "setup_prompt_failed";
pub const SETUP_RUNNING: &str = "setup_running";
pub const SETUP_TIMED_OUT: &str = "setup_timed_out";
pub const SITE_IS_PARKED: &str = "site_is_parked";
pub const SITE_NAME_INVALID: &str = "site_name_invalid";
pub const SITE_NAME_TAKEN: &str = "site_name_taken";
pub const SITE_NOT_A_FOLDER: &str = "site_not_a_folder";
pub const SITE_NOT_FOUND: &str = "site_not_found";
pub const SITE_PATH_NOT_UTF8: &str = "site_path_not_utf8";
pub const SITE_PATH_RELATIVE: &str = "site_path_relative";
pub const SITE_READ_DIR: &str = "site_read_dir";
pub const SITES_LOSE_PHP: &str = "sites_lose_php";
pub const SPAWN_FAILED: &str = "spawn_failed";
pub const TRAY_NOT_FOUND: &str = "tray_not_found";
pub const TRUST_ADD_FAILED: &str = "trust_add_failed";
pub const TRUST_CERT_INVALID: &str = "trust_cert_invalid";
pub const TRUST_DELETE: &str = "trust_delete";
pub const TRUST_READ: &str = "trust_read";
pub const TRUST_RECORD_UNUSABLE: &str = "trust_record_unusable";
pub const TRUST_REMOVE_FAILED: &str = "trust_remove_failed";
pub const TRUST_STORE_UPDATE_FAILED: &str = "trust_store_update_failed";
pub const TRUST_TOOL_MISSING: &str = "trust_tool_missing";
pub const TRUST_WRITE: &str = "trust_write";
pub const UNAUTHORIZED: &str = "unauthorized";
pub const UNINSTALL_ALL_DAEMON_RESTARTED: &str = "uninstall_all_daemon_restarted";
pub const UNINSTALL_ALL_DATA_DELETE_FAILED: &str = "uninstall_all_data_delete_failed";
pub const UNINSTALL_ALL_DATA_DIR_BACK: &str = "uninstall_all_data_dir_back";
pub const UNINSTALL_ALL_DATA_DIR_IN_USE: &str = "uninstall_all_data_dir_in_use";
pub const UNINSTALL_ALL_DATA_DIR_NOT_DEFAULT: &str = "uninstall_all_data_dir_not_default";
pub const UNINSTALL_ALL_DATA_DIR_UNSAFE: &str = "uninstall_all_data_dir_unsafe";
pub const UNINSTALL_ALL_NEEDS_YES: &str = "uninstall_all_needs_yes";
pub const UNINSTALL_ALL_NOT_CONFIRMED: &str = "uninstall_all_not_confirmed";
pub const UNINSTALL_CANCELLED: &str = "uninstall_cancelled";
pub const UNINSTALL_HELPER_BUSY: &str = "uninstall_helper_busy";
pub const UNINSTALL_HELPER_FAILED: &str = "uninstall_helper_failed";
pub const UNINSTALL_HELPER_INSTALL_FAILED: &str = "uninstall_helper_install_failed";
pub const UNINSTALL_HELPER_INSTALL_RUNNING: &str = "uninstall_helper_install_running";
pub const UNINSTALL_HELPER_MISSING: &str = "uninstall_helper_missing";
pub const UNINSTALL_HELPER_NO_ANSWER: &str = "uninstall_helper_no_answer";
pub const UNINSTALL_HELPER_NOT_THE_SERVICE: &str = "uninstall_helper_not_the_service";
pub const UNINSTALL_HELPER_PARTIAL: &str = "uninstall_helper_partial";
pub const UNINSTALL_HELPER_PIPE_FAILED: &str = "uninstall_helper_pipe_failed";
pub const UNINSTALL_HELPER_REJECTED: &str = "uninstall_helper_rejected";
pub const UNINSTALL_HELPER_RUNNING: &str = "uninstall_helper_running";
pub const UNINSTALL_HELPER_UNSAFE: &str = "uninstall_helper_unsafe";
pub const UNINSTALL_INTERRUPTED: &str = "uninstall_interrupted";
pub const UNINSTALL_IO: &str = "uninstall_io";
pub const UNINSTALL_NOT_AUTHORIZED: &str = "uninstall_not_authorized";
pub const UNINSTALL_NOT_SUPPORTED: &str = "uninstall_not_supported";
pub const UNINSTALL_PKEXEC_MISSING: &str = "uninstall_pkexec_missing";
pub const UNINSTALL_PROMPT_FAILED: &str = "uninstall_prompt_failed";
pub const UNINSTALL_RUNNING: &str = "uninstall_running";
pub const UNINSTALL_TIMED_OUT: &str = "uninstall_timed_out";
pub const UNKNOWN_ACTION: &str = "unknown_action";
pub const UNKNOWN_TOOL: &str = "unknown_tool";
pub const UNSUPPORTED_ALIAS: &str = "unsupported_alias";
pub const VALKEY_INIT_FAILED: &str = "valkey_init_failed";
pub const VALKEY_NOT_INSTALLED: &str = "valkey_not_installed";
pub const VALKEY_ORPHAN_STUCK: &str = "valkey_orphan_stuck";
pub const VALKEY_PATH_UNSUPPORTED: &str = "valkey_path_unsupported";
pub const VALKEY_START_TIMEOUT: &str = "valkey_start_timeout";
pub const VALKEY_UNSUPPORTED_PLATFORM: &str = "valkey_unsupported_platform";
pub const VERSION_AMBIGUOUS: &str = "version_ambiguous";
pub const VERSION_IN_USE: &str = "version_in_use";
pub const VERSION_NOT_INSTALLED: &str = "version_not_installed";
pub const XDEBUG_INTERRUPTED: &str = "xdebug_interrupted";

/// Every code above, in alphabetical order.
pub const ALL: &[&str] = &[
    AUTOSTART_NO_HOME,
    AUTOSTART_PATH_INVALID,
    AUTOSTART_READ,
    AUTOSTART_UNSUPPORTED,
    AUTOSTART_WRITE,
    crate::codes::CA_NOT_READY,
    CA_UNREADABLE,
    CADDY_ADMIN_UNREACHABLE,
    CADDY_CONFIG_REJECTED,
    CADDY_CONFIG_WRITE,
    crate::codes::CADDY_NOT_INSTALLED,
    crate::codes::CADDY_NOT_STARTED,
    CADDY_ORPHAN_STUCK,
    CADDY_OTHER_MARSA,
    CADDY_PATH_NOT_UTF8,
    CADDY_RECIPE_INVALID,
    CADDY_START_TIMEOUT,
    CADDY_UNSUPPORTED_PLATFORM,
    CHECKSUM_MISMATCH,
    CONFIG_INVALID,
    CONFIG_NO_HOME,
    CONFIG_READ,
    CONFIG_WRITE,
    DAEMON_ALREADY_RUNNING,
    DAEMON_API_TOO_NEW,
    DAEMON_BAD_RESPONSE,
    DAEMON_FILE_INSECURE,
    DAEMON_FILE_INVALID,
    DAEMON_FILE_MISSING,
    DAEMON_FILE_WRITE,
    DAEMON_NOT_RUNNING,
    DAEMON_START_FAILED,
    DAEMON_STOP_FAILED,
    DAEMON_UNREACHABLE,
    crate::codes::DNS_NOT_RUNNING,
    crate::codes::DNS_PORT_IN_USE,
    crate::codes::DNS_START_FAILED,
    DOWNLOAD_FAILED,
    DOWNLOAD_HTTP_STATUS,
    DOWNLOAD_INSECURE_URL,
    DOWNLOAD_IO,
    DOWNLOAD_SIZE_MISMATCH,
    DOWNLOAD_TOO_LARGE,
    EXITED,
    EXTRACT_CORRUPT,
    EXTRACT_DUPLICATE,
    EXTRACT_IO,
    EXTRACT_TOO_LARGE,
    EXTRACT_UNSAFE_LINK,
    EXTRACT_UNSAFE_PATH,
    EXTRACT_UNSUPPORTED_ENTRY,
    FOLDER_NOT_PARKED,
    HOST_REJECTED,
    HOSTS_BLOCK_BROKEN,
    HOSTS_NOT_USED,
    HOSTS_READ,
    crate::codes::HOSTS_SITES_UNKNOWN,
    crate::codes::HOSTS_TOO_MANY_NAMES,
    HOSTS_WRITE,
    INSTALL_BUSY,
    INSTALL_CHECK_FAILED,
    INSTALL_CRASHED,
    INSTALL_INTERRUPTED,
    INSTALL_IO,
    INSTALL_MANIFEST_MISMATCH,
    INSTALL_MISSING_VC_RUNTIME,
    INSTALL_NO_SUCH_VERSION,
    INSTALL_OS_TOO_OLD,
    INSTALL_SERIAL_CORRUPT,
    INSTALL_UNSAFE_NAME,
    INSTALL_UNSUPPORTED_PLATFORM,
    INSTALL_XZ_MISSING,
    INTERNAL_ERROR,
    INVALID_BODY,
    INVALID_ID,
    INVALID_PATH,
    INVALID_QUERY,
    INVALID_VERSION,
    MANIFEST_BAD_SIGNATURE,
    MANIFEST_INVALID,
    MANIFEST_NO_TRUSTED_KEY,
    MANIFEST_ROLLBACK,
    MANIFEST_SCHEMA_UNSUPPORTED,
    MANIFEST_TOO_LARGE,
    METHOD_NOT_ALLOWED,
    MYSQL_INIT_FAILED,
    MYSQL_NOT_INSTALLED,
    MYSQL_ORPHAN_STUCK,
    MYSQL_PATH_UNSUPPORTED,
    MYSQL_START_TIMEOUT,
    MYSQL_UNSUPPORTED_PLATFORM,
    NO_CURRENT_DIR,
    NO_DEFAULT_VERSION,
    NO_MATCHING_VERSION,
    NO_VERSION,
    NOT_FOUND,
    NOT_INSTALLED,
    crate::codes::NSS_ADD_FAILED,
    crate::codes::NSS_DB_REFUSED,
    crate::codes::NSS_REMOVE_FAILED,
    crate::codes::NSS_TOOL_MISSING,
    OP_NOT_IMPLEMENTED,
    OP_NOT_SUPPORTED,
    OPEN_FAILED,
    OPEN_NO_PROGRAM,
    ORIGIN_REJECTED,
    PF_DELETE,
    PF_FLUSH_FAILED,
    PF_HELPER_UNREADABLE,
    PF_LOAD_FAILED,
    PF_WRITE,
    PHP_CONFIG_WRITE,
    PHP_FPM_NOT_INSTALLED,
    PHP_FPM_ORPHAN_STUCK,
    PHP_FPM_UNAVAILABLE,
    PHP_PATH_NOT_ALLOWED,
    crate::codes::PORT_IN_USE,
    PORTS_APPLY_FAILED,
    PORTS_DELETE,
    PORTS_FOREIGN,
    PORTS_NOT_CHECKED,
    PORTS_READ,
    PORTS_RESTORE_FAILED,
    PORTS_WRITE,
    POSTGRES_INIT_FAILED,
    POSTGRES_NOT_INSTALLED,
    POSTGRES_ORPHAN_STUCK,
    POSTGRES_SHARED_MEMORY_BUSY,
    POSTGRES_START_TIMEOUT,
    POSTGRES_UNSUPPORTED_PLATFORM,
    PROGRAM_NOT_ABSOLUTE,
    RANDOM_UNAVAILABLE,
    RECIPE_BAD_PATH,
    RECIPE_BAD_TEMPLATE,
    RECIPE_INVALID,
    RECIPE_UNKNOWN_PLATFORM,
    REDIRECT_PORT_HELD,
    RESOLVED_CHECK_FAILED,
    RESOLVED_DELETE,
    RESOLVED_FOREIGN,
    RESOLVED_NOT_RUNNING,
    RESOLVED_READ,
    RESOLVED_RESTART_FAILED,
    RESOLVED_STOPPED,
    RESOLVED_SYSTEMD_UNKNOWN,
    RESOLVED_TOOL_MISSING,
    RESOLVED_UNIT_FAILED,
    RESOLVED_USE_HOSTS,
    RESOLVED_WRITE,
    RESOLVER_DELETE,
    RESOLVER_FOREIGN,
    RESOLVER_READ,
    RESOLVER_WRITE,
    RUNTIMES_READ,
    SERVICE_BUSY,
    SERVICE_DATA_DIR_BUSY,
    SERVICE_ELEVATED,
    SERVICE_MANAGED_BY_SITES,
    SERVICE_NOT_FOUND,
    SERVICE_PORT_BUSY,
    SERVICE_PORT_UNKNOWN,
    SETUP_CADDY_PORTS_FAILED,
    SETUP_CANCELLED,
    SETUP_HELPER_BUSY,
    SETUP_HELPER_FAILED,
    SETUP_HELPER_INSTALL_FAILED,
    SETUP_HELPER_INSTALL_RUNNING,
    SETUP_HELPER_MISSING,
    SETUP_HELPER_NO_ANSWER,
    SETUP_HELPER_NOT_THE_SERVICE,
    SETUP_HELPER_PARTIAL,
    SETUP_HELPER_PIPE_FAILED,
    SETUP_HELPER_PROTECTED_FOLDER,
    SETUP_HELPER_REJECTED,
    SETUP_HELPER_RUNNING,
    SETUP_HELPER_UNSAFE,
    crate::codes::SETUP_HOSTS_NOT_REMOVED,
    crate::codes::SETUP_HOSTS_NOT_WRITTEN,
    SETUP_INTERRUPTED,
    SETUP_NOT_AUTHORIZED,
    SETUP_NOT_SUPPORTED,
    SETUP_PKEXEC_MISSING,
    SETUP_PROMPT_FAILED,
    SETUP_RUNNING,
    SETUP_TIMED_OUT,
    SITE_IS_PARKED,
    SITE_NAME_INVALID,
    SITE_NAME_TAKEN,
    SITE_NOT_A_FOLDER,
    SITE_NOT_FOUND,
    SITE_PATH_NOT_UTF8,
    SITE_PATH_RELATIVE,
    SITE_READ_DIR,
    SITES_LOSE_PHP,
    SPAWN_FAILED,
    TRAY_NOT_FOUND,
    crate::codes::TRUST_ADD_FAILED,
    TRUST_CERT_INVALID,
    TRUST_DELETE,
    TRUST_READ,
    crate::codes::TRUST_RECORD_UNUSABLE,
    crate::codes::TRUST_REMOVE_FAILED,
    TRUST_STORE_UPDATE_FAILED,
    TRUST_TOOL_MISSING,
    TRUST_WRITE,
    UNAUTHORIZED,
    UNINSTALL_ALL_DAEMON_RESTARTED,
    UNINSTALL_ALL_DATA_DELETE_FAILED,
    UNINSTALL_ALL_DATA_DIR_BACK,
    UNINSTALL_ALL_DATA_DIR_IN_USE,
    UNINSTALL_ALL_DATA_DIR_NOT_DEFAULT,
    UNINSTALL_ALL_DATA_DIR_UNSAFE,
    UNINSTALL_ALL_NEEDS_YES,
    UNINSTALL_ALL_NOT_CONFIRMED,
    UNINSTALL_CANCELLED,
    UNINSTALL_HELPER_BUSY,
    UNINSTALL_HELPER_FAILED,
    UNINSTALL_HELPER_INSTALL_FAILED,
    UNINSTALL_HELPER_INSTALL_RUNNING,
    UNINSTALL_HELPER_MISSING,
    UNINSTALL_HELPER_NO_ANSWER,
    UNINSTALL_HELPER_NOT_THE_SERVICE,
    UNINSTALL_HELPER_PARTIAL,
    UNINSTALL_HELPER_PIPE_FAILED,
    UNINSTALL_HELPER_REJECTED,
    UNINSTALL_HELPER_RUNNING,
    UNINSTALL_HELPER_UNSAFE,
    UNINSTALL_INTERRUPTED,
    UNINSTALL_IO,
    UNINSTALL_NOT_AUTHORIZED,
    UNINSTALL_NOT_SUPPORTED,
    UNINSTALL_PKEXEC_MISSING,
    UNINSTALL_PROMPT_FAILED,
    UNINSTALL_RUNNING,
    UNINSTALL_TIMED_OUT,
    UNKNOWN_ACTION,
    UNKNOWN_TOOL,
    UNSUPPORTED_ALIAS,
    VALKEY_INIT_FAILED,
    VALKEY_NOT_INSTALLED,
    VALKEY_ORPHAN_STUCK,
    VALKEY_PATH_UNSUPPORTED,
    VALKEY_START_TIMEOUT,
    VALKEY_UNSUPPORTED_PLATFORM,
    VERSION_AMBIGUOUS,
    VERSION_IN_USE,
    VERSION_NOT_INSTALLED,
    XDEBUG_INTERRUPTED,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_listed_once_and_is_a_snake_case_word() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ALL {
            assert!(seen.insert(*code), "{code} is listed twice");
            assert!(
                code.split('_').all(|part| !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
                "{code} is not snake_case"
            );
        }
    }
}
