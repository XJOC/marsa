//! The sentence for each error code. A table of data, so it may pass 800 lines.

use super::*;

/// The window's own code: the daemon stopped before an install ended, as
/// `marsa install` says it.
pub const INSTALL_INTERRUPTED: &str = "install_interrupted";

/// The message for an error code, or a general one with the code for a code
/// this build does not know (a newer daemon may send one).
pub fn error_text(code: &str) -> String {
    error_message(code).map_or_else(|| format!("Unexpected error ({code})."), str::to_owned)
}

pub(super) fn error_message(code: &str) -> Option<&'static str> {
    Some(match code {
        // `marsa_core::client::ClientError`.
        "daemon_not_running" | "daemon_file_missing" => "The daemon is not running.",
        "daemon_start_failed" => "Cannot start the daemon (marsad).",
        "daemon_stop_failed" => "The running daemon did not stop in time.",
        "daemon_unreachable" => "Cannot reach the daemon.",
        "daemon_api_too_new" => "The daemon is newer than this window. Update Marsa.",
        "daemon_bad_response" => "The daemon sent an answer this window cannot read.",
        "invalid_id" => "That name is not valid.",
        "daemon_file_invalid" => "The daemon's run file cannot be read.",
        "daemon_file_insecure" => "The daemon's run file is readable by other users.",
        "daemon_file_write" => "Cannot write the daemon's run file.",
        "random_unavailable" => "The system random source failed.",
        // Codes the daemon answers with (`ErrorBody`).
        "unauthorized" | "origin_rejected" | "host_rejected" => "The daemon refused the request.",
        "not_found" | "method_not_allowed" => "This daemon does not support that request.",
        "invalid_path" | "invalid_query" | "invalid_body" => {
            "The daemon could not read that request."
        }
        "service_not_found" => "No such service.",
        "unknown_action" => "Unknown service action.",
        "service_busy" => "The service is busy with another action. Try again.",
        // `marsa_core::platform::OpenError`, and the window's own folder opener.
        "open_no_program" => "This system has no program to open a folder with.",
        "open_failed" => "Cannot open the folder.",
        // Codes that come as an answer or in `ServiceState::Failed`.
        "caddy_not_installed" => "Caddy is not installed.",
        "caddy_config_write" => "Cannot write the Caddy config file.",
        "caddy_start_timeout" => "Caddy did not become ready in time.",
        "port_in_use" => "A port it needs is used by another program.",
        "caddy_orphan_stuck" => {
            "An old Caddy from a crashed daemon holds the ports and could not be stopped safely."
        }
        "caddy_other_marsa" => "Another Marsa's Caddy uses the ports it needs.",
        "service_managed_by_sites" => {
            "This service starts and stops with the sites that need its PHP version."
        }
        "php_fpm_orphan_stuck" => {
            "An old php-fpm from a crashed daemon holds the socket or port and could not be \
             stopped safely."
        }
        "exited" => "The process exited.",
        // The DNS server, and why setup skipped its DNS step.
        "dns_port_in_use" => {
            "Another program holds port 18053 and may answer .test names. Its log names it."
        }
        "dns_start_failed" => "Marsa's DNS server did not start. Its log says why.",
        "dns_not_running" => "Marsa's DNS server is stopped. Start it, then run setup again.",
        "spawn_failed" => "The process could not be started.",
        "program_not_absolute" => "The service program has no full path.",
        "internal_error" => "The daemon hit a bug. Try again; if it repeats, report it.",
        // The sites routes, `SiteError`, and `ClientError::PathNotUtf8`.
        "site_not_found" => "No such site. It may have just been removed.",
        "site_name_invalid" => "That is not a valid site name.",
        "site_name_taken" => "Another folder already has that site name.",
        "site_not_a_folder" => "That path is not a folder.",
        "site_path_relative" => "That is not a full path of a folder.",
        "site_path_not_utf8" => {
            "That folder's path is not valid UTF-8, so Marsa cannot serve it. Rename the folder."
        }
        "site_read_dir" => "Cannot read that folder. Check its permissions.",
        // `DELETE /v1/sites/{name}` and `DELETE /v1/sites/park`.
        "site_is_parked" => "This site is part of a parked folder, so it cannot be unlinked alone.",
        "folder_not_parked" => "That folder is not parked. It may have just been removed.",
        // `ConfigError`, for the sites settings file.
        "config_read" => "Cannot read the Marsa settings file.",
        "config_invalid" => "The Marsa settings file has an error. Fix the file, then try again.",
        "config_write" => "Cannot write the Marsa settings file.",
        // `PUT /v1/runtimes/{tool}/default`, `GET /v1/runtimes`, and
        // the PHP version of a site.
        "version_not_installed" => "That version is not installed. Install it first.",
        "no_default_version" => "This program has no default version: the newest one is used.",
        "invalid_version" => "That is not a valid version number.",
        "unsupported_alias" => "Version names such as \"latest\" are not supported. Pick a number.",
        "unknown_tool" => "This daemon does not know that program.",
        "runtimes_read" => "Cannot read the folder of installed versions.",
        // `DELETE /v1/runtimes/{tool}/{version}`.
        "version_in_use" => {
            "This version is in use: it is a default, a site is set to it, or Caddy runs from \
             it. Nothing was uninstalled."
        }
        // The only PHP and the default, and sites run it.
        "sites_lose_php" => {
            "This is the only PHP version installed, and sites run it: they would have no PHP. \
             Install another PHP version and pick it as the default first."
        }
        "version_ambiguous" => "More than one installed version matches. Pick one in full.",
        "uninstall_io" => {
            "Cannot delete this version: a program from it may still run, or its folder is not \
             writable. Quit that program, then try again."
        }
        // `PUT /v1/php/xdebug`.
        "xdebug_interrupted" => {
            "The daemon stopped before the change reached PHP. The setting is saved: Marsa \
             applies it when it starts again."
        }
        // `GET` and `PUT /v1/autostart`, behind the tray's "Start at
        // Login" line.
        "tray_not_found" => {
            "marsa-tray was not found next to marsad, so Marsa cannot start at login."
        }
        "autostart_path_invalid" => "The path of marsa-tray cannot go into a login file.",
        "autostart_unsupported" => "Starting at login is not supported on this system yet.",
        "autostart_no_home" => "The system gives no home folder for the login file.",
        "autostart_read" => "Cannot read the login file.",
        "autostart_write" => "Cannot write the login file.",
        // Installs: `InstallError` and the codes under it, in
        // the `failed` event or the answer to the request.
        "install_busy" => {
            "Another install or uninstall is running. Wait for it to end, then try again."
        }
        "install_unsupported_platform" => "Marsa has no build of this program for this system.",
        "install_os_too_old" => {
            "This program needs a newer system than this one. Nothing was installed."
        }
        "install_no_such_version" => "The version list has no version that matches.",
        "install_manifest_mismatch" => {
            "The version list and this build of Marsa disagree on the files to install. \
             Nothing was installed. Update Marsa, or report it."
        }
        "install_unsafe_name" => {
            "The version list has a file name Marsa refuses. Nothing was installed. Report it."
        }
        "install_check_failed" => "The new program failed its test run, so it was not installed.",
        "install_xz_missing" => {
            "This install needs /usr/bin/xz. Install the package xz-utils (or xz), then try again."
        }
        "install_missing_vc_runtime" => {
            "This program needs the Microsoft Visual C++ Redistributable: \
             https://aka.ms/vs/17/release/vc_redist.x64.exe"
        }
        "install_serial_corrupt" => {
            "The file runtimes/.manifest-serial in the data folder is damaged. Delete it, then \
             try again."
        }
        "install_io" => {
            "Cannot write to the data folder. Check the free disk space and the folder's \
             permissions."
        }
        "install_crashed" => {
            "The install stopped because of a bug in Marsa. Try again; if it repeats, report it \
             with the file logs/marsad.log."
        }
        INSTALL_INTERRUPTED => "The daemon stopped before the install ended. Try again.",
        "manifest_no_trusted_key" => {
            "This build of Marsa has no trusted key for the version list, so it cannot check \
             it. Nothing was installed."
        }
        "manifest_bad_signature" => {
            "The version list failed its signature check. Nothing was installed. Try again \
             later; if it repeats, report it."
        }
        "manifest_too_large" | "manifest_invalid" => {
            "The version list cannot be read. Nothing was installed. Try again later; if it \
             repeats, report it."
        }
        "manifest_schema_unsupported" => {
            "The version list uses a newer format. Update Marsa, then try again."
        }
        "manifest_rollback" => {
            "The version list is older than one this computer already accepted, so it was \
             refused. Try again later; if it repeats, report it."
        }
        "download_failed" => "A download failed. Check your connection, then try again.",
        "download_http_status" => "The download server answered with an error. Try again later.",
        "download_insecure_url" => {
            "Marsa refused a download that is not https. Nothing was installed. Report it."
        }
        "download_size_mismatch" => {
            "A downloaded file has the wrong size. Nothing was installed. Try again."
        }
        "download_too_large" => {
            "A download is larger than allowed. Nothing was installed. Report it."
        }
        "download_io" => {
            "Cannot save a download in the data folder. Check the free disk space and the \
             folder's permissions."
        }
        "checksum_mismatch" => {
            "A downloaded file does not match its signed checksum. Nothing was installed."
        }
        "extract_unsafe_path"
        | "extract_unsafe_link"
        | "extract_unsupported_entry"
        | "extract_duplicate"
        | "extract_too_large" => {
            "A downloaded archive has content Marsa refuses. Nothing was installed. Report it."
        }
        "extract_corrupt" => "A downloaded archive is damaged. Nothing was installed. Try again.",
        "extract_io" => {
            "Cannot unpack a download in the data folder. Check the free disk space and the \
             folder's permissions."
        }
        "recipe_invalid"
        | "recipe_unknown_platform"
        | "recipe_bad_path"
        | "recipe_bad_template" => "This build of Marsa has a broken install recipe. Report it.",
        // Setup (`POST /v1/setup`): why it did not run, the
        // steps it skipped, and the codes of `marsa-helper`.
        "setup_running" => "Another setup is running. Wait for it to end, then try again.",
        // `marsa uninstall --all` runs: setup waits for it.
        "uninstall_running" => {
            "Marsa is being removed from this computer (marsa uninstall --all). Wait for it to \
             end, then try again."
        }
        "setup_not_supported" => "Setup from Marsa works on macOS, Linux, and Windows only.",
        // The window runs on the machine marsad runs on, so `cfg!` is the
        // daemon's system too. Its admin prompt is UAC (ADR 0018).
        "setup_cancelled" if cfg!(windows) => {
            "The UAC prompt was declined, so nothing was changed. Run setup again when you are \
             ready."
        }
        "setup_cancelled" => {
            "The password prompt was cancelled, so nothing was changed. Run setup again when \
             you are ready."
        }
        "setup_timed_out" => {
            "Marsa stopped waiting for setup after 5 minutes. Some steps may already have run; \
             running setup again is safe."
        }
        "setup_interrupted" => {
            "The daemon stopped during setup. Some steps may already have run; running setup \
             again is safe."
        }
        "setup_not_authorized" => {
            "Your system did not give Marsa admin rights, so nothing was changed: a wrong \
             password, or no password prompt (as over SSH). Run setup again from your desktop \
             and enter the password of an administrator account."
        }
        // Windows: the helper service and the hosts file (ADR 0018).
        "setup_helper_not_the_service" => {
            "Another program holds the name of Marsa's helper service, so Marsa sent it nothing \
             and nothing was changed. Restart Windows, then run setup again; if it repeats, \
             report it with the file logs/marsad.log."
        }
        "setup_helper_install_failed" => {
            "Marsa could not install its helper service, so nothing was changed. Windows' \
             service log and the file logs/marsad.log say why. Run setup again; if it repeats, \
             report it."
        }
        "setup_helper_install_running" => {
            "The install of Marsa's helper service is still running. Wait a few minutes, then \
             run setup again."
        }
        "setup_helper_pipe_failed" => {
            "Marsa could not reach its helper service, so nothing was changed. Run setup again; \
             if it repeats, report it with the file logs/marsad.log."
        }
        "setup_helper_busy" => {
            "Marsa's helper service is busy with another request. Wait a moment, then run \
             setup again."
        }
        "setup_helper_no_answer" => {
            "Marsa's helper service gave no answer, so the result of setup is not known. Run \
             \"marsa doctor\" in a terminal to see what is set up, then run setup again."
        }
        // Linux.
        "setup_pkexec_missing" => {
            "pkexec is not installed, so Marsa cannot ask for the administrator password. \
             Install the package pkexec (Debian, Ubuntu), polkit (Fedora), or policykit-1 \
             (older systems), then run setup again."
        }
        // Linux, the second request of setup.
        "setup_hosts_not_written" => {
            "The steps above are done, but the second password prompt did not go through, so \
             the sites' .test names are not in /etc/hosts yet and the sites do not open. Run \
             setup again."
        }
        "setup_hosts_not_removed" => {
            "The steps above are done, and .test names now reach Marsa without /etc/hosts. The \
             second password prompt did not go through, so Marsa's old lines stay in \
             /etc/hosts; they do no harm. Run setup again to take them out."
        }
        "hosts_not_used" => {
            "Marsa has no lines in /etc/hosts for this data folder yet. Run setup first."
        }
        "hosts_sites_unknown" => {
            "Marsa could not read its site list, so it left the hosts file as it is. Look at \
             the sites, then run setup again."
        }
        "hosts_too_many_names" => {
            "There are more sites than Marsa can put in the hosts file at once (1000 names), \
             so it left the file as it is. Remove sites you do not use, then run setup again."
        }
        "setup_caddy_ports_failed" => {
            "Linux now lets Caddy use ports 80 and 443, but Caddy did not take them. Most often \
             another program listens there (Apache, nginx). Stop it, then restart Caddy; the \
             Caddy log says why."
        }
        "resolved_foreign" => {
            "/etc/systemd/system/marsa-dns-link.service belongs to another tool, so Marsa left \
             it as it is. Move that file away, then run setup again."
        }
        "resolved_read" | "resolved_write" | "resolved_delete" => {
            "Linux did not let Marsa change its files for .test names. Run setup again; if it \
             repeats, report it."
        }
        // Setup then puts the names in /etc/hosts.
        "resolved_not_running" => {
            "systemd-resolved does not run here, so Marsa did not make its link for .test \
             names: they go into /etc/hosts instead."
        }
        // Linux and Windows.
        "hosts_read" => {
            "Marsa did not change the hosts file: it cannot be read, or it is a link, \
             read-only, or larger than 16 MiB. Fix the file, then run setup again."
        }
        "hosts_write" => {
            "Marsa could not write the hosts file. Another program may hold it (some security \
             tools do); close it, then run setup again."
        }
        "hosts_block_broken" => {
            "The hosts file has a \"# Marsa start\" line without its \"# Marsa end\" line, or \
             the other way round, so Marsa left it as it is. Remove Marsa's lines from it, then \
             run setup again."
        }
        "resolved_tool_missing" => {
            "The ip program (package iproute2) is not on this system, so Marsa could not make \
             its link for .test names. Install iproute2, then run setup again."
        }
        "resolved_unit_failed" => {
            "systemd did not start or stop Marsa's link for .test names \
             (marsa-dns-link.service). Run setup again; if it repeats, report it."
        }
        "resolved_check_failed" => {
            "Marsa could not confirm that systemd-resolved sends .test names, and only them, to \
             its DNS server, so it took its link out again. Run setup again; if it repeats, \
             report it."
        }
        "resolved_use_hosts" => {
            "This system's systemd is older than version 250 and systemd-networkd may run, so \
             systemd-resolved can drop the setting that sends .test names to Marsa. Marsa did \
             not make it: .test names here go into /etc/hosts instead."
        }
        "resolved_systemd_unknown" => {
            "Marsa could not read the version of systemd here, so it did not make its link for \
             .test names. Report it."
        }
        "resolved_restart_failed" => {
            "systemd-resolved did not restart after Marsa removed an old setting, so it may \
             still send every name to Marsa's DNS server. Restart systemd-resolved."
        }
        "resolved_stopped" => {
            "systemd-resolved stopped and did not start again, so no name resolves on this \
             computer now. Start systemd-resolved, then run setup again."
        }
        "ports_foreign" => {
            "/etc/sysctl.d/60-marsa.conf belongs to another tool, so Caddy cannot use ports 80 \
             and 443. Move that file away, then run setup again."
        }
        "ports_read" | "ports_write" | "ports_delete" => {
            "Linux did not let Marsa change /etc/sysctl.d/60-marsa.conf. Run setup again; if it \
             repeats, report it."
        }
        "ports_apply_failed" => {
            "Linux did not let Caddy use ports 80 and 443, so Marsa took its setting out again. \
             Run setup again; if it repeats, report it."
        }
        "ports_restore_failed" => {
            "Marsa removed its setting for ports 80 and 443, but every program may use ports 80 \
             to 1023 until the computer restarts."
        }
        "trust_tool_missing" => {
            "This system has no tool Marsa knows for its trusted certificates, so the trust in \
             Marsa's certificate authority was not changed. On Debian, Ubuntu, or Fedora, \
             install the ca-certificates package, then run setup again."
        }
        // Chrome and Firefox on Linux.
        "nss_tool_missing" => {
            "Chrome and Firefox need certutil: install libnss3-tools (Debian, Ubuntu) or \
             nss-tools (Fedora), then run setup again."
        }
        "nss_db_refused" => {
            "A certificate database of Chrome or Firefox is a link or another user's (often \
             after sudo). Make it a real folder of your user, then run setup again."
        }
        "nss_add_failed" => {
            "A Chrome or Firefox certificate database did not take Marsa's CA. Remove its \
             password if it has one, or close the browser, then run setup again."
        }
        "trust_store_update_failed" => {
            "The system's store of trusted certificates could not be updated. Run setup again; \
             if it repeats, report it."
        }
        "setup_helper_missing" => {
            "The marsa-helper program is not next to marsad, so nothing was changed. Reinstall \
             Marsa, then try again."
        }
        "setup_helper_unsafe" => {
            "Another user could change the marsa-helper program or its folder, so Marsa did not \
             ask for the password. Make both writable only by you, then run setup again."
        }
        "setup_prompt_failed" => {
            "macOS could not show the administrator password prompt, so nothing was changed. \
             Make sure this Mac's desktop is unlocked. \
             Quit the Marsa window and the menu-bar icon, run \"marsa daemon stop\" in \
             Terminal, then run \"marsa setup\" there, on your desktop."
        }
        "setup_helper_failed" => {
            "marsa-helper gave an answer Marsa cannot read, so the result of setup is not known. \
             Run setup again; if it repeats, report it with the file logs/marsad.log."
        }
        "setup_helper_rejected" => {
            "marsa-helper refused the setup request, so nothing was changed. Report it with the \
             file logs/marsad.log."
        }
        "redirect_port_held" => {
            "Another program uses port 80 or 443, and the redirect would take its traffic. Stop \
             that program, or turn off its web server, then run setup again."
        }
        "ports_not_checked" => {
            "Marsa could not check which programs use ports 80 and 443, so it left them alone. \
             Run setup again; if it repeats, report it."
        }
        "ca_not_ready" => {
            "Caddy did not make Marsa's certificate authority in time. See why in the Caddy \
             log, then run setup again."
        }
        "caddy_not_started" => {
            "Caddy did not start, so Marsa's certificate authority is not there yet. See why in \
             the Caddy log, then run setup again."
        }
        "ca_unreadable" => {
            "Cannot read the root certificate of Marsa's certificate authority, or it is not a \
             certificate. Run setup again; if it repeats, report it."
        }
        "op_not_implemented" => "This build of marsa-helper cannot do this yet. Update Marsa.",
        "op_not_supported" => "marsa-helper cannot do this on this system.",
        "resolver_foreign" => {
            "/etc/resolver/test belongs to another tool, so Marsa left it as it is. Remove that \
             tool's .test setting, then run setup again."
        }
        "resolver_read" | "resolver_write" => {
            "macOS did not let Marsa write /etc/resolver/test. Run setup again; if it repeats, \
             report it."
        }
        "trust_cert_invalid" => {
            "The root certificate of Marsa's certificate authority is not valid, so it was not \
             trusted. Report it."
        }
        // Setup's trust folder in the data folder.
        "trust_record_unusable" => {
            "Marsa cannot use its folder for the trusted certificate (trust in the data folder): \
             it must be your own folder, not a link. Fix or delete it, then run setup again."
        }
        "trust_read" | "trust_write" | "trust_add_failed" => {
            "Marsa's certificate authority was not trusted. Run setup again; if it repeats, \
             report it."
        }
        "pf_write" | "pf_load_failed" => {
            "macOS did not load the redirect of ports 80 and 443. Run setup again; if it \
             repeats, report it."
        }
        // Codes the window shows only through a general error (the
        // command line has fuller texts for them).
        "config_no_home" => {
            "Cannot find your home folder, so Marsa does not know where to keep its data."
        }
        "no_current_dir" => "Cannot read the current folder.",
        "mysql_init_failed" => "Marsa could not make MySQL's data folder.",
        "mysql_not_installed" => "MySQL of the version set in global.toml is not installed.",
        "mysql_orphan_stuck" => "An old MySQL of this Marsa still runs and did not stop.",
        "mysql_path_unsupported" => {
            "The path of Marsa's data folder has a character that MySQL's settings file cannot hold."
        }
        "mysql_start_timeout" => "MySQL started but took no connections in time.",
        "mysql_unsupported_platform" => "Marsa runs MySQL on macOS and Linux only for now.",
        "valkey_init_failed" => "Marsa could not make Valkey's data folder or settings file.",
        "valkey_not_installed" => "Valkey of the version set in global.toml is not installed.",
        "valkey_orphan_stuck" => "An old Valkey of this Marsa still runs and did not stop.",
        "valkey_path_unsupported" => {
            "The path of Marsa's data folder has a character that Valkey's settings file cannot hold."
        }
        "valkey_start_timeout" => "Valkey started but did not answer in time.",
        "valkey_unsupported_platform" => "Marsa runs Valkey on macOS and Linux only for now.",
        "nss_remove_failed" => {
            "Marsa's certificate authority is still in a browser certificate database. Run \"marsa uninstall --all\" again."
        }
        "pf_delete" | "pf_flush_failed" => {
            "macOS did not let Marsa remove the redirect of ports 80 and 443."
        }
        "pf_helper_unreadable" => {
            "The admin part of Marsa cannot read marsa-helper. Move Marsa out of Desktop, Documents, and Downloads."
        }
        "postgres_init_failed" => "Marsa could not make PostgreSQL's data folder.",
        "postgres_not_installed" => {
            "PostgreSQL of the version set in global.toml is not installed."
        }
        "postgres_orphan_stuck" => "An old PostgreSQL of this Marsa still runs and did not stop.",
        "postgres_start_timeout" => "PostgreSQL started but took no connections in time.",
        "postgres_unsupported_platform" => "Marsa runs PostgreSQL on macOS and Linux only for now.",
        "resolver_delete" => "macOS did not let Marsa delete /etc/resolver/test.",
        "service_data_dir_busy" => {
            "Another server already runs on this service's data folder, so Marsa started no second one."
        }
        "service_port_busy" => {
            "Another program uses the port this service needs. Quit it, or set another port in global.toml."
        }
        "service_port_unknown" => {
            "Marsa could not check which program uses the port this service needs, so it did not start it."
        }
        "setup_helper_protected_folder" => {
            "marsa-helper is inside Desktop, Documents, or Downloads, where macOS keeps root out. Move Marsa, then run setup again."
        }
        "trust_delete" => "Marsa could not delete its copy of its certificate authority.",
        "trust_remove_failed" => {
            "Windows did not remove Marsa's certificate authority from the trusted root certificates."
        }
        "uninstall_all_daemon_restarted"
        | "uninstall_all_data_dir_back"
        | "uninstall_all_data_dir_in_use"
        | "uninstall_all_data_dir_not_default"
        | "uninstall_all_data_dir_unsafe"
        | "uninstall_all_data_delete_failed" => {
            "Marsa could not delete its data folder. Delete it yourself."
        }
        "uninstall_all_needs_yes" | "uninstall_all_not_confirmed" => {
            "The uninstall was not confirmed, so nothing was removed."
        }
        "uninstall_cancelled" => "The prompt was cancelled, so nothing was removed.",
        "uninstall_helper_failed" | "uninstall_helper_rejected" => {
            "marsa-helper could not do the uninstall, so nothing was removed."
        }
        "uninstall_helper_missing" => "The marsa-helper program is not next to marsad.",
        "uninstall_helper_partial" => {
            "Marsa's helper could not remove everything. Run \"marsa uninstall --all\" again."
        }
        "uninstall_helper_running" => "A run of Marsa's helper is still going on. Try again later.",
        "uninstall_helper_unsafe" => {
            "Another user could change marsa-helper, so Marsa did not run it."
        }
        "uninstall_interrupted" | "uninstall_timed_out" => {
            "Marsa stopped waiting for the uninstall. It may have run: check, then run it again."
        }
        "uninstall_not_authorized" => "The system did not give Marsa admin rights.",
        "uninstall_not_supported" => "The uninstall works on macOS, Linux, and Windows only.",
        "uninstall_pkexec_missing" => {
            "pkexec is not installed, so Marsa cannot ask for the password."
        }
        "uninstall_prompt_failed" => "The system could not show the administrator prompt.",
        "caddy_admin_unreachable" => "Marsa cannot reach Caddy's admin port.",
        "caddy_config_rejected" => "Caddy refused the new configuration.",
        "caddy_path_not_utf8" => "A path Caddy needs is not valid UTF-8.",
        "caddy_recipe_invalid" => "The Caddy recipe is not valid.",
        "caddy_unsupported_platform" => "Caddy has no build for this system.",
        "daemon_already_running" => "The daemon is already running.",
        "no_matching_version" => "No installed version matches that request.",
        "no_version" => "No version is set for this folder.",
        "not_installed" => "That version is not installed.",
        "php_config_write" => "Cannot write the PHP configuration.",
        "php_fpm_not_installed" => "This PHP version has no php-fpm.",
        "php_fpm_unavailable" => "php-fpm is not available for this PHP version.",
        "php_path_not_allowed" => "That PHP path is not allowed.",
        "setup_helper_partial" => "Marsa's helper could not do everything. Run setup again.",
        "setup_helper_running" => {
            "A run of Marsa's helper is still going on. Wait, then run setup again."
        }
        "uninstall_helper_pipe_failed" => {
            "Marsa could not reach its helper service, so nothing was removed."
        }
        "uninstall_helper_install_failed" => {
            "Marsa could not start its helper service, so nothing was removed."
        }
        "uninstall_helper_install_running" => {
            "The install of Marsa's helper service is still running. Try again in a few minutes."
        }
        "uninstall_helper_not_the_service" => {
            "Another program holds the name of Marsa's helper service. Restart Windows, then try again."
        }
        "uninstall_helper_busy" => "Marsa's helper service is busy. Try again in a moment.",
        "uninstall_helper_no_answer" => {
            "Marsa's helper service gave no answer, so the result of the uninstall is not known."
        }
        BRIDGE_FAILED => "The window could not start its background worker.",
        TRAY_WORKER_FAILED => "The menu-bar icon could not start its background worker.",
        WINDOW_START_FAILED => "Cannot start the window (marsa-gui).",
        _ => return None,
    })
}
