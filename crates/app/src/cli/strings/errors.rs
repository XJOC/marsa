//! The sentence for each error code. A table of data, so it may pass 800 lines.

use marsa_core::runtimes::Runtime;

use super::*;

/// The sentence for `code`, or `None` if this file has none.
pub(super) fn known(code: &str, d: &Details<'_>) -> Option<String> {
    let name = d.runtime.map_or("the program", runtime_name);
    let path = d
        .path
        .map_or("the data folder".to_owned(), |p| p.display().to_string());
    let url = |prefix: &str| d.url.map_or(String::new(), |u| format!("{prefix}{u}"));
    // A service by its id (`caddy`), and by its name in a sentence (`Caddy`).
    let id = d.service.unwrap_or("<service>");
    let service = d.service.map_or("the service", service_name);
    let service_first = d.service.map_or("The service", service_name);
    // A path at the start of a sentence, or what to say without one.
    let shown = |default: &str| {
        d.path
            .map_or(default.to_owned(), |p| p.display().to_string())
    };
    let site = d.site.unwrap_or("<name>");
    const REPORT: &str = "Nothing was installed. Report it.";
    const AGAIN: &str = "Nothing was installed. Try again later; if it repeats, report it.";
    const DISK: &str = "Check the free disk space and the folder's permissions.";
    const RESTART_DAEMON: &str =
        "Try again; if it repeats, run \"marsa daemon stop\", then try again.";
    const UPDATE: &str = "Update Marsa, or report it.";

    let text = match code {
        // Installer: `InstallError`.
        "install_unsupported_platform" => format!(
            "Marsa has no {name} build for {}.",
            d.platform.unwrap_or("this system")
        ),
        "install_os_too_old" => match (d.needs, d.found) {
            (Some(needs), Some(found)) => format!(
                "{name} needs {needs} or newer, and this system has {found}. \
                 Nothing was installed."
            ),
            _ => format!("{name} needs a newer system than this one. Nothing was installed."),
        },
        "install_busy" => "Another install is running. Wait for it to end, then try again.".into(),
        "install_no_such_version" => no_such_version(name, d.requested, d.available),
        "install_manifest_mismatch" => format!(
            "The version list and this build of Marsa disagree on the files of {name}. \
             Nothing was installed. Update Marsa, or report it."
        ),
        "install_unsafe_name" => {
            format!("The version list has a file name Marsa refuses. {REPORT}")
        }
        "install_check_failed" => match d.output.map(str::trim).filter(|o| !o.is_empty()) {
            Some(output) => {
                format!(
                    "The new {name} failed its test run, so it was not installed. It printed: {output}"
                )
            }
            None => format!("The new {name} failed its test run, so it was not installed."),
        },
        "install_xz_missing" => format!(
            "{name} comes as a .tar.xz archive, and this system has no /usr/bin/xz. \
             Install the package xz-utils (Debian, Ubuntu) or xz (Fedora, Arch), then try \
             again. Nothing was installed."
        ),
        "install_missing_vc_runtime" => format!(
            "{name} needs the Microsoft Visual C++ Redistributable: \
             https://aka.ms/vs/17/release/vc_redist.x64.exe"
        ),
        "install_serial_corrupt" => format!(
            "{path} is damaged. Delete it, then try again: Marsa then accepts the current \
             version list."
        ),
        "install_io" => format!("Cannot write to {path}. {DISK}"),
        "install_crashed" => "The install stopped because of a bug in Marsa. Run \
             \"marsa runtimes\" to see what is installed, then try again. If it repeats, \
             report it with the file logs/marsad.log."
            .into(),
        "install_interrupted" => "The Marsa daemon stopped before the install ended. Run \
             \"marsa runtimes\" to see what is installed, then try again."
            .into(),

        // The signed version list: `ManifestError`.
        "manifest_no_trusted_key" => "This build of Marsa has no trusted manifest key yet, so it \
             cannot check the version list. Nothing was installed."
            .into(),
        "manifest_bad_signature" => format!("The version list failed its signature check. {AGAIN}"),
        "manifest_too_large" => format!("The version list is larger than allowed. {AGAIN}"),
        "manifest_invalid" => format!("The version list cannot be read. {AGAIN}"),
        "manifest_schema_unsupported" => {
            "The version list uses a newer format. Update Marsa, then try again.".into()
        }
        "manifest_rollback" => format!(
            "The version list is older than one this computer already accepted, so it was \
             refused. {AGAIN}"
        ),

        // Downloads: `FetchError`.
        "download_failed" if d.cause == Some("download_http_status") => format!(
            "Download failed: the server answered with an error{}. Try again later.",
            url(" for ")
        ),
        "download_failed" => format!("Download failed{}. Check your connection.", url(": ")),
        "download_http_status" => format!(
            "The server answered with an error{}. Try again later.",
            url(" for ")
        ),
        "download_insecure_url" => format!(
            "Refused a download that is not https{}. {REPORT}",
            url(": ")
        ),
        "download_size_mismatch" => {
            format!("A downloaded file has the wrong size{}. {AGAIN}", url(": "))
        }
        "download_too_large" => format!("A download is larger than allowed{}. {REPORT}", url(": ")),
        "download_io" => format!("Cannot save a download to {path}. {DISK}"),
        "dump_invalid" => "A dump that PHP sent could not be read, so it was dropped.".into(),
        "dumps_port_in_use" => "Another program uses port 9912, so dump() stays in the page. If it is \
             \"php artisan dump-server\", stop it, then run \"marsa dumps on\"."
            .into(),
        "dump_too_large" => "A dump was too large to keep, so only a note was saved.".into(),
        "checksum_mismatch" => {
            "A downloaded file does not match its signed checksum. Nothing was installed.".into()
        }

        // Archives: `ExtractError`.
        "extract_unsafe_path"
        | "extract_unsafe_link"
        | "extract_unsupported_entry"
        | "extract_duplicate" => {
            format!("A downloaded archive has an entry Marsa refuses. {REPORT}")
        }
        "extract_too_large" => {
            format!("A downloaded archive unpacks to more than allowed. {REPORT}")
        }
        "extract_corrupt" => format!("A downloaded archive is damaged. {AGAIN}"),
        "extract_io" => format!("Cannot unpack a download to {path}. {DISK}"),

        // The version the user wrote: `BadRequest`.
        "unsupported_alias" => format!(
            "\"{}\" is a name, not a version. Write a version number, such as {}.",
            d.requested.unwrap_or_default(),
            example_version(d.runtime)
        ),
        "invalid_version" => format!(
            "\"{}\" is not a version. Write a version number, such as {}.",
            d.requested.unwrap_or_default(),
            example_version(d.runtime)
        ),

        // `marsa use`: `PUT /v1/runtimes/{tool}/default`.
        "version_not_installed" => match (d.runtime, d.requested) {
            (Some(runtime), Some(version)) => format!(
                "{name} {version} is not installed. Run \"marsa install {} {version}\", then \
                 try again.",
                runtime.dir_name()
            ),
            _ => "This version is not installed. Run \"marsa runtimes\" to see the installed \
                  versions."
                .into(),
        },
        "no_default_version" => format!(
            "{} has no default version: Marsa always runs the newest installed one.",
            d.runtime.map_or("This program", runtime_name)
        ),

        // `marsa uninstall`: `DELETE /v1/runtimes/{tool}/{version}`.
        // `version_in_use` has its facts in `version_in_use()`.
        "version_in_use" => "This version is in use: it is a default, a site is set to it, \
             or Caddy runs from it. Nothing was uninstalled."
            .into(),
        // The only PHP, and sites run it.
        "sites_lose_php" => "This is the only PHP version installed, and sites run it: they \
             would have no PHP. Install another PHP version and make it the default first. \
             Nothing was uninstalled."
            .into(),
        "version_ambiguous" => format!(
            "{} matches more than one installed {}version. Name one in full; \"marsa \
             runtimes{}\" lists them.",
            d.requested
                .map_or("This version".to_owned(), |r| format!("\"{r}\"")),
            d.runtime
                .map_or(String::new(), |r| format!("{} ", runtime_name(r))),
            d.runtime
                .map_or(String::new(), |r| format!(" {}", r.dir_name()))
        ),
        "uninstall_io" => "Cannot delete this version: a program from it may still run, or \
             its folder is not writable. Quit that program, then try again."
            .into(),

        // Built-in recipes: `RecipeError`. Tests check them, so a user should
        // never see these.
        "recipe_invalid"
        | "recipe_unknown_platform"
        | "recipe_bad_path"
        | "recipe_bad_template" => {
            format!("This build of Marsa has a broken install recipe for {name}. Report it.")
        }

        // Folders.
        "runtimes_read" => format!("Cannot read {path}. Check the folder's permissions."),
        "config_no_home" => "Cannot find your home folder, so Marsa does not know where to keep \
             its data."
            .into(),

        // The daemon client: `ClientError` and `DaemonFileError`.
        "daemon_not_running" | "daemon_file_missing" => {
            "The Marsa daemon is not running. Start it with \"marsa daemon start\".".into()
        }
        "daemon_start_failed" => {
            format!("Cannot start the Marsa daemon (marsad). Its log may say why: {path}")
        }
        "daemon_stop_failed" => "The Marsa daemon did not stop in time. Try again; if it \
             repeats, quit marsad in your system's process list."
            .into(),
        "daemon_unreachable" => format!("Cannot reach the Marsa daemon. {RESTART_DAEMON}"),
        "daemon_api_too_new" => {
            "The running Marsa daemon is newer than this marsa command. Update Marsa.".into()
        }
        "daemon_bad_response" => {
            format!("The Marsa daemon sent an answer this command cannot read. {UPDATE}")
        }
        "invalid_id" => format!("\"{id}\" is not a valid service name."),
        "daemon_file_invalid" => format!("Cannot read {path}. {RESTART_DAEMON}"),
        "daemon_file_insecure" => format!(
            "Other users can read {path}, so Marsa does not use it. Quit marsad in your \
             system's process list, then try again."
        ),
        "daemon_file_write" => format!("Cannot write {path}. {DISK}"),
        "random_unavailable" => {
            "The system's random number source failed, so the daemon cannot make its key. \
             Report it."
                .into()
        }

        // The daemon's own codes: its `ErrorBody`, and `Failed { code }` in
        // a service status.
        "service_not_found" => {
            format!("There is no service named \"{id}\". Run \"marsa services\" to see them.")
        }
        "unknown_action" => {
            "The daemon does not know this action. Use start, stop, or restart.".into()
        }
        "service_busy" => format!(
            "Another start, stop, or restart of {service} is running. Wait for it to end, \
             then try again."
        ),
        "caddy_not_installed" => format!(
            "Caddy is not installed in {path}. Run \"marsa install caddy\", then try again."
        ),
        "caddy_config_write" => format!("Cannot write the Caddy config file {path}. {DISK}"),
        "caddy_start_timeout" => "Caddy started but did not answer in time, so it was stopped. \
             Run \"marsa logs caddy\" to see why."
            .into(),
        "port_in_use" => format!(
            "A port {service} needs is used by another program. Run \"marsa logs {id}\" to \
             see which one."
        ),
        // The DNS server for .test: a service, and a reason
        // setup skips its DNS step.
        "dns_port_in_use" => "Another program holds port 18053, where Marsa answers .test \
             names, so it may answer them instead. Run \"marsa logs dns\" to see which one \
             and stop it. Then run \"marsa services start dns\", and \"marsa setup\" if it \
             skipped this step."
            .into(),
        "dns_start_failed" => "Marsa's DNS server for .test names did not start. Run \
             \"marsa logs dns\" to see why. Then run \"marsa services start dns\", and \
             \"marsa setup\" if it skipped this step."
            .into(),
        "dns_not_running" => "Marsa's DNS server for .test names is stopped. Run \"marsa \
             services start dns\", then \"marsa setup\" again."
            .into(),
        "caddy_orphan_stuck" => format!(
            "An old Caddy, left running when the Marsa daemon crashed, still holds the ports, \
             and Marsa could not stop it safely. Run \"marsa logs {id}\" to see its process, \
             stop it, then try again."
        ),
        "service_managed_by_sites" => format!(
            "{service_first} starts and stops with the sites that need its PHP version, so it \
             cannot be started or stopped by hand. Run \"marsa logs {id}\" to see its lines."
        ),
        "php_not_installed" => format!(
            "{id} needs the site's PHP, and none that the site can run is installed. Run \
             \"marsa sites\" to see the site's PHP, then \"marsa install php <version>\"."
        ),
        "worker_failed" => format!(
            "{id} stopped after it crashed again and again. Run \"marsa logs {id}\" to see why, \
             fix it, then run \"marsa services restart {id}\"."
        ),
        "worker_orphan_stuck" => format!(
            "{id} did not start: the one that an earlier marsad left running did not stop, and \
             two would take the same jobs or run the same tasks. Run \"marsa logs {id}\" to \
             see its pid, end that process, then run \"marsa services restart {id}\"."
        ),
        "worker_follows_site" => format!(
            "{id} runs while its site asks for it, so it cannot be started or stopped by hand. \
             Use \"marsa queue on|off <site>\" or \"marsa schedule on|off <site>\"."
        ),
        "php_fpm_orphan_stuck" => format!(
            "An old php-fpm, left running when the Marsa daemon crashed, still holds the socket \
             or port {service} needs, and Marsa could not stop it safely. Run \"marsa logs \
             {id}\" to see its process and stop it. Then run \"marsa services restart caddy\" \
             to try again; it briefly interrupts every site."
        ),
        // PostgreSQL: `databases::ActionError` and
        // `ServiceBusy` in core. Marsa never stops the other program.
        "service_port_busy" => format!(
            "Another program uses the port {service} needs, and Marsa stops nothing. Quit it, or \
             set another port in global.toml ([{id}] port), then try again. Run \"marsa logs \
             {id}\" to see which program."
        ),
        "service_data_dir_busy" => format!(
            "Another {service} server already runs on its data folder, so Marsa started no \
             second one and stopped nothing. Run \"marsa logs {id}\" to see its process."
        ),
        "service_port_unknown" => format!(
            "Marsa could not check which program uses the port {service} needs, so it did not \
             start it. Try again; run \"marsa logs {id}\" for details."
        ),
        // Windows: `postgres.exe` refuses an administrator's token.
        "service_elevated" => format!(
            "{service_first} does not run with administrator rights, and the Marsa daemon runs \
             with them, so it was not started. Run Marsa without administrator rights: run \
             \"marsa daemon stop\", then \"marsa daemon start\" from a terminal that is not \
             \"Run as administrator\"."
        ),
        "postgres_not_installed" => "PostgreSQL of the version set in global.toml is not \
             installed. Run \"marsa install postgres\", then try again."
            .into(),
        "postgres_init_failed" => "Marsa could not make PostgreSQL's data folder. Run \"marsa \
             logs postgres\" to see why."
            .into(),
        "postgres_start_timeout" => "PostgreSQL started but took no connections in time, so it \
             was stopped. Run \"marsa logs postgres\" to see why."
            .into(),
        "postgres_orphan_stuck" => "An old PostgreSQL of this Marsa, left running when the \
             Marsa daemon stopped, still runs after SIGKILL, or can no longer be told apart \
             from another program. Run \"marsa logs postgres\" to see its pid; quit that \
             process in your system's process list only if it is PostgreSQL, then try again."
            .into(),
        "postgres_unsupported_platform" => {
            "Marsa has no PostgreSQL for this system.".into()
        }
        // After a SIGKILL of the old postmaster, its backends end only once
        // they see that it is gone.
        "postgres_shared_memory_busy" => "Processes of an earlier PostgreSQL still use the \
             shared memory of its data folder, so it did not start. They end by themselves \
             within a few seconds: wait a moment, then try again. Run \"marsa logs postgres\" \
             to see why."
            .into(),
        // MySQL, the same way.
        "mysql_not_installed" => "MySQL of the version set in global.toml is not installed. \
             Run \"marsa install mysql\", then try again."
            .into(),
        "mysql_init_failed" => "Marsa could not make MySQL's data folder. Run \"marsa logs \
             mysql\" to see why."
            .into(),
        "mysql_start_timeout" => "MySQL started but took no connections in time, so it was \
             stopped. Run \"marsa logs mysql\" to see why."
            .into(),
        "mysql_orphan_stuck" => "An old MySQL of this Marsa, left running when the Marsa \
             daemon stopped, still runs after SIGKILL, or can no longer be told apart from \
             another program. Run \"marsa logs mysql\" to see its pid; quit that process in \
             your system's process list only if it is mysqld, then try again."
            .into(),
        "mysql_unsupported_platform" => "Marsa has no MySQL for this system.".into(),
        "mysql_path_unsupported" => "The path of Marsa's data folder has a double quote or a \
             control character, which MySQL's settings file cannot hold, so MySQL was not \
             started. Run \"marsa logs mysql\" to see the path."
            .into(),
        "mail_not_running" => "Mailpit does not run, so there is no mail to show. Run \
             \"marsa services start mailpit\", or \"marsa install mailpit\" first."
            .into(),
        "mail_id_invalid" => "That is not a mail id. Run \"marsa mail\" to see the ids.".into(),
        "mail_not_found" => "Mailpit has no mail with that id. Run \"marsa mail\" to see the \
             ids."
            .into(),
        "mail_unavailable" => "Mailpit did not answer as expected. Run \"marsa logs mailpit\" \
             to see why."
            .into(),
        // The database browser (`marsa db`, ADR 0028). It opens Marsa's own
        // servers and the sites' SQLite files only, and changes nothing.
        "db_server_not_running" => format!(
            "{service_first} is not running, so its databases cannot be opened. Run \
             \"marsa services start {id}\", then try again."
        ),
        "db_source_invalid" => "That source, table, or column cannot be used. Write a source \
             as mysql/<database>, postgres/<database>, or site/<site>."
            .into(),
        "db_not_found" => "The server has no database by that name. Run \"marsa db\" to see \
             the databases."
            .into(),
        "db_table_not_found" => "The database has no table by that name. Run \"marsa db tables \
             <source>\" to see the tables."
            .into(),
        "db_file_not_found" => format!(
            "The site {site} has no SQLite file at database/database.sqlite. Marsa does not \
             make one."
        ),
        "db_file_outside_site" => format!(
            "The SQLite file of the site {site} is not a regular file inside the site's \
             folder, so Marsa does not open it."
        ),
        "db_sign_in_failed" => format!(
            "{service_first} refused Marsa's sign-in as root with no password. If you set a \
             password for root, \"marsa db\" cannot open this server yet."
        ),
        "db_unavailable" => format!(
            "{service_first} did not answer as expected. Run \"marsa logs {id}\" to see why."
        ),
        "db_timeout" => "The database took too long to answer, so Marsa stopped waiting.".into(),
        "db_read_only" => "This statement changes data, and \"marsa db\" is read-only, so the \
             database refused it."
            .into(),
        "db_one_statement" => "Run one statement at a time.".into(),
        "db_sql_too_long" => "The statement is longer than 256 KB.".into(),
        "db_query_failed" => "The database did not run the statement.".into(),
        "db_row_not_found" => "No row has that key now. Refresh the rows and try again.".into(),
        "db_query_cancelled" => "The statement was cancelled.".into(),
        "db_busy" => "Another statement still runs on this database. Wait for it to end, or \
             cancel it."
            .into(),
        "db_result_gone" => "Marsa no longer keeps the rows of that statement. Run it again."
            .into(),
        "job_not_cancellable" => "Only a running SQL statement can be cancelled.".into(),
        // Mailpit, the same way.
        "mailpit_not_installed" => "Mailpit is not installed. Run \"marsa install mailpit\", \
             then try again."
            .into(),
        "mailpit_init_failed" => "Marsa could not make Mailpit's mail folder. Run \
             \"marsa logs mailpit\" to see why."
            .into(),
        "mailpit_start_timeout" => "Mailpit started but did not greet on its SMTP port in time, \
             so it was stopped. Run \"marsa logs mailpit\" to see why."
            .into(),
        "mailpit_orphan_stuck" => "An old Mailpit of this Marsa, left running when the Marsa \
             daemon stopped, still runs after SIGKILL, or can no longer be told apart from \
             another program. Run \"marsa logs mailpit\" to see its pid; quit that process in \
             your system's process list only if it is mailpit, then try again."
            .into(),
        "mailpit_unsupported_platform" => "Marsa has no Mailpit for this system.".into(),
        // Valkey, the same way.
        "valkey_not_installed" => "Valkey of the version set in global.toml is not installed. \
             Run \"marsa install valkey\", then try again."
            .into(),
        "valkey_init_failed" => "Marsa could not make Valkey's data folder or write its \
             settings file. Run \"marsa logs valkey\" to see why."
            .into(),
        "valkey_start_timeout" => "Valkey started but did not answer PING in time, so it was \
             stopped. Run \"marsa logs valkey\" to see why."
            .into(),
        "valkey_orphan_stuck" => "An old Valkey of this Marsa, left running when the Marsa \
             daemon stopped, still runs after SIGKILL, or can no longer be told apart from \
             another program. Run \"marsa logs valkey\" to see its pid; quit that process in \
             your system's process list only if it is valkey-server, then try again."
            .into(),
        "valkey_unsupported_platform" => "Marsa has no Valkey for this system.".into(),
        "valkey_path_unsupported" => "The path of Marsa's data folder has a double quote or a \
             control character, which Valkey's settings file cannot hold, or on Windows it is \
             not on a drive such as C:, so Valkey was not started. Run \"marsa logs valkey\" \
             to see the path."
            .into(),
        "caddy_other_marsa" => format!(
            "The Caddy of another Marsa (another data folder, or another user) uses the ports \
             {service} needs. Stop that Marsa first. Run \"marsa logs {id}\" for details."
        ),
        "exited" => {
            format!("{service_first} stopped by itself. Run \"marsa logs {id}\" to see why.")
        }
        "spawn_failed" => format!("Cannot run {service}. The daemon's log may say why: {path}"),
        "program_not_absolute" => {
            format!("Marsa refused to run {service}: its program has no full path. Report it.")
        }
        "unknown_tool" => format!("The daemon does not know this program. {UPDATE}"),
        "invalid_path" | "invalid_query" | "invalid_body" | "not_found" | "method_not_allowed" => {
            format!("The daemon did not understand the request of this command. {UPDATE}")
        }
        "unauthorized" => format!("The daemon refused the key of this command. {RESTART_DAEMON}"),
        "origin_rejected" | "host_rejected" => {
            "The daemon refused the request of this command. Report it.".into()
        }
        "internal_error" => "The Marsa daemon hit a bug. Try again; if it repeats, report it \
             with the file logs/marsad.log."
            .into(),

        // Sites: `SiteError`, `SkipReason`, and the sites routes.
        "site_not_found" => {
            format!("There is no site named \"{site}\". Run \"marsa sites\" to see them.")
        }
        "site_name_invalid" => format!(
            "{} is not a valid site name. Use 1 to 63 of a-z, 0-9, and -, with no - at the \
             start or the end.",
            d.site
                .map_or("This".to_owned(), |name| format!("\"{name}\""))
        ),
        "site_name_taken" => format!("Another folder already has the site name \"{site}\"."),
        "site_not_laravel" => format!(
            "\"{site}\" is not a Laravel project (it has no artisan file), so it has no Laravel \
             log or workers."
        ),
        "site_log_unsafe" => format!(
            "Marsa does not read the log of \"{site}\": its storage/logs leads out of the \
             project folder, or the log file is a link. Make storage/logs a plain folder."
        ),
        "site_not_a_folder" => format!(
            "{} is not a folder. Check the path, then try again.",
            shown("The path")
        ),
        "site_path_relative" => format!(
            "{} is not a full path. Pass a full path, then try again.",
            shown("The path")
        ),
        "site_path_not_utf8" => format!(
            "{} is not valid UTF-8, so Marsa cannot serve it. Rename the folder, then try again.",
            shown("The path")
        ),
        "site_read_dir" => format!(
            "Cannot read {}. Check the folder's permissions.",
            shown("the folder")
        ),
        "no_current_dir" => "Cannot read the current folder; it may have been deleted. Go to \
             another folder, or pass a full path."
            .into(),
        // `marsa unlink`, `marsa forget`, and `marsa open`.
        "site_is_parked" => match d.path {
            Some(dir) => format!(
                "\"{site}\" comes from the parked folder {0}, so it cannot be unlinked alone. \
                 To stop serving that folder and all its sites, run \"marsa forget {0}\".",
                dir.display()
            ),
            None => format!(
                "\"{site}\" comes from a parked folder, so it cannot be unlinked alone. To stop \
                 serving that folder and all its sites, run \"marsa forget <folder>\"."
            ),
        },
        "folder_not_parked" => format!(
            "{} is not a parked folder. {}",
            shown("The folder"),
            if d.available.is_empty() {
                "No folder is parked.".to_owned()
            } else {
                format!("Parked folders: {}.", d.available.join(", "))
            }
        ),
        "open_no_program" => format!(
            "Cannot find a program that opens the browser (xdg-open on Linux). Open {} \
             yourself.",
            d.url.unwrap_or("the site's address")
        ),
        "open_failed" => format!(
            "Cannot open the browser. Open {} yourself.",
            d.url.unwrap_or("the site's address")
        ),

        // The settings files: `ConfigError`.
        "config_read" => format!(
            "Cannot read {}. Check the file's permissions.",
            shown("the settings file")
        ),
        "config_invalid" => format!(
            "{} has an error, so Marsa cannot read it. Fix the file, then try again.",
            shown("The settings file")
        ),
        "config_write" => format!("Cannot write {}. {DISK}", shown("the settings file")),

        // `marsa setup`: the setup route, its skipped steps, and
        // the failure codes of `marsa-helper`.
        "setup_running" => "Another setup is running. Wait for it to end, then try again.".into(),
        "setup_not_supported" => "\"marsa setup\" works on macOS, Linux, and Windows only.".into(),
        // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is
        // the daemon's system too. Its admin prompt is UAC (ADR 0018).
        "setup_cancelled" if cfg!(windows) => "The UAC prompt was declined, so nothing was \
             changed. Run \"marsa setup\" again when you are ready."
            .into(),
        "setup_cancelled" => "The password prompt was cancelled, so nothing was changed. Run \
             \"marsa setup\" again when you are ready."
            .into(),
        "setup_timed_out" => {
            format!("Marsa stopped waiting for setup after 5 minutes. {SETUP_MAY_HAVE_RUN}")
        }
        "setup_interrupted" => format!(
            "The Marsa daemon stopped during setup, so Marsa stopped waiting. \
             {SETUP_MAY_HAVE_RUN}"
        ),
        "setup_not_authorized" => "The system did not give Marsa admin rights, so nothing \
             was changed. The password was not accepted, or the prompt could not open (over \
             SSH, for example). Run \"marsa daemon stop\", then run \"marsa setup\" again in \
             a terminal on your desktop and enter the password of an administrator account."
            .into(),
        "setup_helper_missing" => "The marsa-helper program is not next to marsad, so nothing \
             was changed. Reinstall Marsa, then try again."
            .into(),
        // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is
        // the daemon's system too. Its admin prompt is UAC (ADR 0018).
        "setup_helper_unsafe" if cfg!(windows) => "marsa-helper.exe is outside your user \
             folder, where another user of this computer could change it, so Marsa did not ask \
             for admin rights and nothing was changed. Install Marsa into your user folder with \
             its installer, then run \"marsa setup\" again."
            .into(),
        "setup_helper_unsafe" => "Another user could change the marsa-helper program or its \
             folder, so Marsa did not ask for the password and nothing was changed. Make both \
             yours and writable only by you (chmod go-w), then run \"marsa setup\" again. The \
             path is in the file logs/marsad.log."
            .into(),
        "setup_helper_protected_folder" => "marsa-helper is inside Desktop, Documents, or \
             Downloads, and macOS keeps the admin part of Marsa out of those folders, so \
             nothing was changed. Run \"marsa daemon stop\", move Marsa's programs to \
             ~/Library/Application Support/dev.marsa.Marsa/bin, then run \"marsa setup\" from \
             there."
            .into(),
        "setup_prompt_failed" => "macOS could not show the administrator password prompt, so \
             marsa-helper did not run. Make sure you are logged in at this Mac's desktop and it \
             is unlocked. Quit the Marsa window and the menu-bar icon, run \"marsa daemon \
             stop\", then run \"marsa setup\" again in Terminal. The error number is in the \
             file logs/marsad.log."
            .into(),
        "setup_helper_failed" => "marsa-helper gave an answer Marsa cannot read, so the result \
             of setup is not known. Run \"marsa setup\" again; if it repeats, report it with \
             the file logs/marsad.log."
            .into(),
        "setup_helper_rejected" => "marsa-helper refused the setup request, so nothing was \
             changed. This is a bug in Marsa: report it with the file logs/marsad.log."
            .into(),
        // Windows: the helper service behind its pipe (ADR 0018).
        "setup_helper_not_the_service" => "Another program holds the name of Marsa's helper \
             service, so Marsa sent it nothing and nothing was changed. Restart Windows, then \
             run \"marsa setup\" again; if it repeats, report it with the file \
             logs/marsad.log."
            .into(),
        "setup_helper_install_failed" => "Marsa could not install its helper service, so \
             nothing was changed. Windows' service log and the file logs/marsad.log say why. \
             Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "setup_helper_install_running" => "The install of Marsa's helper service is still \
             running. Wait a few minutes, then run \"marsa doctor\"; run \"marsa setup\" again \
             only once it has ended."
            .into(),
        "setup_helper_pipe_failed" => "Marsa could not reach its helper service, so nothing \
             was changed. Run \"marsa setup\" again; if it repeats, report it with the file \
             logs/marsad.log."
            .into(),
        "setup_helper_busy" => "Marsa's helper service is busy with another request. Wait a \
             moment, then run \"marsa setup\" again."
            .into(),
        "setup_helper_no_answer" => "Marsa's helper service gave no answer, so the result of \
             setup is not known. Run \"marsa doctor\" to see what is set up, then run \"marsa \
             setup\" again."
            .into(),
        "setup_helper_partial" => "Marsa's helper could not do everything. Run \"marsa setup\" again; if it repeats, report it with the file logs/marsad.log."
            .into(),
        "setup_helper_running" => "A run of Marsa's helper is still going on. Wait for it to end, then run \"marsa setup\" again."
            .into(),
        "uninstall_helper_pipe_failed" => "Marsa could not reach its helper service, so nothing was removed. Run \"marsa uninstall --all\" again; if it repeats, report it with the file logs/marsad.log."
            .into(),
        "uninstall_helper_install_failed" => "Marsa could not start its helper service, so nothing was removed. Windows' service log and the file logs/marsad.log say why. Run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_install_running" => "The install of Marsa's helper service is still running. Wait a few minutes, then run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_not_the_service" => "Another program holds the name of Marsa's helper service, so Marsa sent it nothing and nothing was removed. Restart Windows, then run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_busy" => "Marsa's helper service is busy with another request. Wait a moment, then run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_no_answer" => "Marsa's helper service gave no answer, so the result of the uninstall is not known. Run \"marsa doctor\" to see what is left, then run \"marsa uninstall --all\" again."
            .into(),
        "redirect_port_held" => "Another program uses port 80 or 443, and the redirect would \
             take its traffic. Stop that program, or turn off its web server, then run \
             \"marsa setup\" again."
            .into(),
        "ports_not_checked" => "Marsa could not check which programs use ports 80 and 443, so \
             it left them alone. Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "ca_not_ready" => "Caddy did not make Marsa's certificate authority in time, so Marsa \
             cannot trust it yet. Run \"marsa logs caddy\" to see why, then run \"marsa setup\" \
             again."
            .into(),
        "caddy_not_started" => "Caddy did not start, so Marsa's certificate authority is not \
             there yet. Run \"marsa logs caddy\" to see why, then run \"marsa setup\" again."
            .into(),
        "ca_unreadable" => format!(
            "Cannot read the root certificate of Marsa's certificate authority{}, or it is \
             not a certificate. Check the file, then run \"marsa setup\" again.",
            d.path
                .map_or(String::new(), |p| format!(" at {}", p.display()))
        ),
        "op_not_implemented" => format!("This build of marsa-helper cannot do this yet. {UPDATE}"),
        "op_not_supported" => "marsa-helper cannot do this on this system.".into(),
        "resolver_foreign" => "/etc/resolver/test belongs to another tool, so Marsa left it as \
             it is. Remove that tool's .test setting, then run \"marsa setup\" again."
            .into(),
        "resolver_write" => "macOS did not let Marsa write /etc/resolver/test. Run \"marsa \
             setup\" again; if it repeats, report it."
            .into(),
        // Setup and the uninstall both meet it: no advice for one of them.
        "resolver_read" => "macOS did not let Marsa read /etc/resolver/test, so Marsa left it \
             as it is. Run the command again; if it repeats, report it."
            .into(),
        "resolver_delete" => "macOS did not let Marsa delete /etc/resolver/test, so .test \
             names still go to Marsa. Run \"marsa uninstall --all\" again; if it repeats, \
             report it."
            .into(),
        "trust_cert_invalid" => "The root certificate of Marsa's certificate authority is not \
             valid, so it was not trusted. Report it."
            .into(),
        "trust_write" => "Marsa could not add its certificate authority to the system's \
             trusted certificates. Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        // Setup and the uninstall both meet it: no advice for one of them.
        // macOS and Linux: the helper's copy of the CA in a system folder.
        "trust_read" => "Marsa could not use its copy of its certificate authority: it cannot \
             be read, or a file or link Marsa did not write is in its place (on macOS in \
             /Library/Application Support/Marsa; on Linux \
             /usr/local/share/ca-certificates/marsa-local-ca.crt or \
             /etc/pki/ca-trust/source/anchors/marsa-local-ca.pem; on Windows ca-root.pem in \
             Marsa's folder in Program Files). So Marsa left the trust for \
             all users as it is. If you put a file there, move it away; then run the command \
             again. If it repeats, report it."
            .into(),
        "trust_record_unusable" => format!(
            "Marsa cannot use {}: it is not a plain file in a folder of yours, or not the \
             certificate Marsa trusted. So Marsa left that trust as it is. If you did not put \
             the file there, delete it by hand and run the command again; to take a trust away \
             yourself, remove \"Marsa Local CA\" in Keychain Access.",
            shown("its record of the trusted certificate")
        ),
        "trust_delete" => "Marsa could not delete its copy of its certificate authority in a \
             system folder. Run the command again; if it repeats, report it."
            .into(),
        // Windows: `certutil` and the machine's Root store, not the user's
        // keychain (ADR 0018). The CLI runs on the machine marsad runs on, so
        // `cfg!` is the daemon's system too.
        "trust_add_failed" if cfg!(windows) => "Windows did not add Marsa's certificate \
             authority to the trusted root certificates, so browsers will not trust Marsa's \
             sites yet. Run \"marsa setup\" again; if it repeats, report it with the file \
             logs/marsad.log."
            .into(),
        "trust_remove_failed" if cfg!(windows) => "Windows did not remove Marsa's certificate \
             authority from the trusted root certificates. Run the command again; if it \
             repeats, remove \"Marsa Local CA\" with certlm.msc (Trusted Root Certification \
             Authorities) and report it."
            .into(),
        "trust_add_failed" => "Your browser will not trust Marsa's sites yet: the trust for \
             your user was not added. Run \"marsa setup\" in Terminal on the Mac's desktop and \
             approve the dialog macOS shows. If Marsa was started over SSH, run \"marsa daemon \
             stop\" first. If it repeats, report it with the file logs/marsad.log."
            .into(),
        "trust_remove_failed" => "Marsa could not take away the trust in its certificate \
             authority. Run the command again; if it repeats, remove \"Marsa Local CA\" in \
             Keychain Access and report it."
            .into(),
        // The Linux ops of `marsa-helper`.
        "resolved_foreign" => "/etc/systemd/system/marsa-dns-link.service belongs to another \
             tool, so Marsa left it as it is. Move that file away, then run \"marsa setup\" \
             again."
            .into(),
        "resolved_read" | "resolved_write" | "resolved_delete" => "Linux did not let Marsa \
             change its files for .test names (/etc/systemd/system/marsa-dns-link.service, or \
             the old /etc/systemd/resolved.conf.d/marsa.conf). Run the command again; if it \
             repeats, report it."
            .into(),
        // Setup then puts the names in /etc/hosts.
        "resolved_not_running" => "systemd-resolved does not run on this system, so Marsa \
             did not make its link for .test names: they go into /etc/hosts instead."
            .into(),
        // Linux before systemd 250 with systemd-networkd, or without
        // systemd-resolved, and Windows.
        "hosts_not_used" => "Marsa has no lines in the hosts file for this data folder: this \
             system sends .test names to Marsa without it, or \"marsa setup\" has not run yet."
            .into(),
        "hosts_read" => format!(
            "Marsa did not change the hosts file ({HOSTS_FILE}): it cannot be read, or it is \
             a link, or it is larger than 16 MiB. Fix the file, then run the command again."
        ),
        "hosts_write" => format!(
            "Marsa could not write the hosts file ({HOSTS_FILE}): it is read-only, or another \
             program holds it (some security tools do). Fix that, then run the command again. \
             If it repeats, report it with the file logs/marsad.log."
        ),
        "hosts_block_broken" => format!(
            "The hosts file ({HOSTS_FILE}) has a \"# Marsa start\" line without its \"# Marsa \
             end\" line, or the other way round, so Marsa left it as it is. Remove Marsa's lines \
             from it, then run the command again."
        ),
        "resolved_tool_missing" => "The ip program (package iproute2) is not on this system, \
             so Marsa could not make its link for .test names. Install iproute2, then run \
             \"marsa setup\" again."
            .into(),
        "resolved_unit_failed" => "systemd did not start or stop Marsa's link for .test names \
             (marsa-dns-link.service). Run the command again; if it repeats, report it with \
             the output of \"journalctl -u marsa-dns-link\"."
            .into(),
        "setup_pkexec_missing" => "pkexec is not installed, so Marsa cannot ask for the \
             administrator password. Install the package pkexec (Debian, Ubuntu), polkit \
             (Fedora), or policykit-1 (older systems), then run \"marsa setup\" again."
            .into(),
        // Linux, the second request of setup.
        "setup_hosts_not_written" => format!(
            "The steps above are done, but the second password prompt did not go through, so \
             the sites' .test names are not in {HOSTS_FILE} yet and the sites do not open. Run \
             \"marsa setup\" again."
        ),
        "setup_hosts_not_removed" => format!(
            "The steps above are done, and .test names now reach Marsa without {HOSTS_FILE}. \
             The second password prompt did not go through, so Marsa's old lines stay in \
             {HOSTS_FILE}; they do no harm. Run \"marsa setup\" again to take them out."
        ),
        "hosts_sites_unknown" => format!(
            "Marsa could not read its site list, so it left {HOSTS_FILE} as it is. Run \"marsa \
             sites\" to see why, then run \"marsa setup\" again."
        ),
        "hosts_too_many_names" => format!(
            "There are more sites than Marsa can put in {HOSTS_FILE} at once (1000 names), so \
             it left the file as it is. Remove sites you do not use, then run \"marsa setup\" \
             again."
        ),
        "setup_caddy_ports_failed" => "Linux now lets Caddy use ports 80 and 443, but Caddy \
             did not take them. Most often another program listens there (Apache, nginx). Stop \
             it, then run \"marsa services restart caddy\"; \"marsa logs caddy\" says why."
            .into(),
        "resolved_use_hosts" => "This system's systemd is older than version 250 and \
             systemd-networkd may run, so systemd-resolved can drop the setting that sends \
             .test names to Marsa. Marsa did not make it: .test names here go into /etc/hosts \
             instead."
            .into(),
        "resolved_systemd_unknown" => "Marsa could not read the version of systemd here, so \
             it did not make its link for .test names. Report it with the output of \
             \"systemctl --version\"."
            .into(),
        "resolved_check_failed" => "Marsa could not confirm that systemd-resolved sends .test \
             names, and only them, to its DNS server, so it took its link out again. Run \
             \"marsa setup\" again; if it repeats, report it."
            .into(),
        // Only while Marsa removes the setting of an earlier build, which
        // sent every name to its DNS server.
        "resolved_restart_failed" => "systemd-resolved did not restart after Marsa removed an \
             old setting, so it may still send every name to Marsa's DNS server. Run \"sudo \
             systemctl restart systemd-resolved\"; if it fails, report it with the output of \
             \"journalctl -u systemd-resolved\"."
            .into(),
        "resolved_stopped" => "systemd-resolved stopped and did not start again, so no name \
             resolves on this computer now. Run \"sudo systemctl start systemd-resolved\", then \
             run \"marsa setup\" again; if it repeats, report it with the output of \
             \"journalctl -u systemd-resolved\"."
            .into(),
        "ports_foreign" => "/etc/sysctl.d/60-marsa.conf belongs to another tool, so Marsa \
             left it as it is and Caddy cannot use ports 80 and 443. Move that file away, then \
             run \"marsa setup\" again."
            .into(),
        "ports_read" | "ports_write" | "ports_delete" => "Linux did not let Marsa change \
             /etc/sysctl.d/60-marsa.conf. Run the command again; if it repeats, report it."
            .into(),
        "ports_apply_failed" => "Linux did not let Caddy use ports 80 and 443, so Marsa took \
             its setting out again. Run \"marsa setup\" again; if it repeats, report it with \
             the file logs/marsad.log."
            .into(),
        "ports_restore_failed" => "Marsa removed its setting for ports 80 and 443, but could \
             not apply that now: every program may use ports 80 to 1023 until the computer \
             restarts."
            .into(),
        "trust_tool_missing" => "This system has no tool Marsa knows for its trusted \
             certificates (update-ca-certificates with /usr/local/share/ca-certificates, or \
             update-ca-trust with /etc/pki/ca-trust/source/anchors), so Marsa could not change \
             the trust in its certificate authority. On Debian, Ubuntu, or Fedora, install the \
             ca-certificates package, then run the command again. Systems whose tool reads \
             another folder (Arch, openSUSE) are not supported yet."
            .into(),
        // Chrome and Firefox on Linux.
        "nss_tool_missing" => "certutil is not installed, so Chrome and Firefox do not trust \
             Marsa's sites yet (curl and other programs do). Install libnss3-tools (Debian, \
             Ubuntu) or nss-tools (Fedora), then run \"marsa setup\" again."
            .into(),
        "nss_db_refused" => "Chrome or Firefox may not trust Marsa's sites yet: a \
             certificate database in your home folder is a link or belongs to another user \
             (often after a browser or certutil ran with sudo), so Marsa left it alone. \
             logs/marsad.log names it. Replace a link with a real folder; give a folder of \
             another user back to yours (sudo chown -R \"$USER\" <folder>). Then run \
             \"marsa setup\" again."
            .into(),
        "nss_remove_failed" => "Marsa's certificate authority is still in a certificate \
             database of Chrome or Firefox in your home folder. If certutil is missing, install \
             libnss3-tools (Debian, Ubuntu) or nss-tools (Fedora); if the database has a \
             password, remove it. Close the browser, then run \"marsa uninstall --all\" again. \
             logs/marsad.log names the database."
            .into(),
        "nss_add_failed" => "Chrome or Firefox may not trust Marsa's sites yet: a certificate \
             database in your home folder did not take Marsa's certificate authority. If it has \
             a password (Firefox's Primary Password), remove the password or add the \
             certificate by hand; otherwise close the browser and run \"marsa setup\" again. \
             logs/marsad.log names the database."
            .into(),
        "trust_store_update_failed" => "The system's store of trusted certificates could not \
             be updated, so Marsa's certificate authority is not trusted, or not removed, yet. \
             Run the command again; if it repeats, report it with the file logs/marsad.log."
            .into(),
        "pf_write" | "pf_load_failed" => "macOS did not load the redirect of ports 80 and 443. \
             Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "pf_flush_failed" | "pf_delete" => "macOS did not let Marsa remove the redirect of \
             ports 80 and 443 completely. Run \"marsa uninstall --all\" again; if it repeats, \
             report it."
            .into(),
        "pf_helper_unreadable" => "The admin part of Marsa could not read marsa-helper, so \
             ports 80 and 443 were not redirected. Run \"marsa daemon stop\" first. If \
             Marsa's programs are in Desktop, Documents, or Downloads, move them to \
             ~/Library/Application Support/dev.marsa.Marsa/bin; otherwise reinstall Marsa. \
             Then run \"marsa setup\" again."
            .into(),

        // `marsa uninstall --all`: the uninstall route, and the
        // command's own questions and the data folder.
        "uninstall_running" => {
            "An uninstall is running. Wait for it to end, then try again.".into()
        }
        "uninstall_not_supported" => "\"marsa uninstall --all\" works on macOS, Linux, and \
             Windows only. Nothing was removed."
            .into(),
        // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is
        // the daemon's system too. Its admin prompt is UAC (ADR 0018).
        "uninstall_cancelled" if cfg!(windows) => "The UAC prompt was declined, so nothing \
             was removed. Run \"marsa uninstall --all\" again when you are ready."
            .into(),
        "uninstall_cancelled" => "The password prompt was cancelled, so nothing was removed. \
             Run \"marsa uninstall --all\" again when you are ready."
            .into(),
        "uninstall_timed_out" => format!(
            "Marsa stopped waiting for the uninstall after 5 minutes. {UNINSTALL_MAY_HAVE_RUN}"
        ),
        "uninstall_interrupted" => format!(
            "The Marsa daemon stopped during the uninstall, so Marsa stopped waiting. \
             {UNINSTALL_MAY_HAVE_RUN}"
        ),
        // Linux: pkexec found no agent to ask with, or the password was
        // not accepted. The CLI runs on the machine marsad runs on, so `cfg!`
        // is the daemon's system too.
        "uninstall_not_authorized" if cfg!(target_os = "linux") => "Linux did not give Marsa \
             admin rights, so nothing was removed. The password was not accepted, or no \
             password dialog could open (over SSH, for example). Run \"marsa daemon stop\", \
             then run \"marsa uninstall --all\" again in a terminal of your desktop session."
            .into(),
        "uninstall_pkexec_missing" => "pkexec is not installed, so Marsa cannot ask for the \
             administrator password, and nothing was removed. Install the package pkexec \
             (Debian, Ubuntu), polkit (Fedora), or policykit-1 (older systems), then run \
             \"marsa uninstall --all\" again."
            .into(),
        "uninstall_not_authorized" => "macOS did not give Marsa admin rights, so nothing was \
             removed. The password was not accepted, or the prompt could not open (over SSH, \
             for example). Run \"marsa daemon stop\", then run \"marsa uninstall --all\" again \
             in Terminal on your desktop and enter the password of an administrator account."
            .into(),
        "uninstall_helper_missing" => "The marsa-helper program is not next to marsad, so \
             nothing was removed. Reinstall Marsa, then try again."
            .into(),
        // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is
        // the daemon's system too. Its admin prompt is UAC (ADR 0018).
        "uninstall_helper_unsafe" if cfg!(windows) => "marsa-helper.exe is outside your user \
             folder, or it is not a plain file, so another user of this computer could change \
             it: Marsa did not ask for admin rights and nothing was removed. Put Marsa back in \
             your user folder with its installer, then run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_unsafe" => "Another user could change the marsa-helper program or \
             its folder, so Marsa did not ask for the password and nothing was removed. Make \
             both yours and writable only by you (chmod go-w), then run \"marsa uninstall \
             --all\" again. The path is in the file logs/marsad.log."
            .into(),
        "uninstall_prompt_failed" => "macOS could not show the administrator password prompt, \
             so marsa-helper did not run and nothing was removed. Make sure you are logged in at \
             this Mac's desktop and it is unlocked. Quit the Marsa window and the menu-bar icon, \
             run \"marsa daemon stop\", then run \"marsa uninstall --all\" again in Terminal. \
             The error number is in the file logs/marsad.log."
            .into(),
        // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is
        // the daemon's system too. Its admin prompt is UAC (ADR 0018).
        "uninstall_helper_failed" if cfg!(windows) => "Marsa could not run its helper as \
             asked, so nothing was removed. The file logs/marsad.log says why. Run \"marsa \
             uninstall --all\" again; if it repeats, report it."
            .into(),
        "uninstall_helper_partial" => "Marsa's helper could not remove everything: some of \
             Marsa's lines in the hosts file, its certificate authority, or its helper service \
             may be left. Run \"marsa uninstall --all\" again; if it repeats, report it with \
             the file logs/marsad.log."
            .into(),
        "uninstall_helper_running" => "A run of Marsa's helper is still going on, so Marsa \
             started no other. Wait a few minutes, then run \"marsa uninstall --all\" again."
            .into(),
        "uninstall_helper_failed" => "marsa-helper gave an answer Marsa cannot read, so the \
             result is not known, and Marsa removed nothing else. Run \"marsa uninstall --all\" \
             again; if it repeats, report it with the file logs/marsad.log."
            .into(),
        "uninstall_helper_rejected" => "marsa-helper refused the uninstall request, so nothing \
             was removed. This is a bug in Marsa: report it with the file logs/marsad.log."
            .into(),
        "uninstall_all_needs_yes" => "The input is not a terminal, so Marsa cannot ask you to \
             type yes, and nothing was removed. Run \"marsa uninstall --all --yes\" to remove \
             Marsa without the question."
            .into(),
        "uninstall_all_not_confirmed" => "You did not type yes, so nothing was removed.".into(),
        "uninstall_all_data_dir_not_default" => format!(
            "Marsa deletes only its own data folder, not one given with --data-dir, so {} was \
             kept. Delete it yourself if you no longer need it.",
            shown("the folder")
        ),
        "uninstall_all_data_dir_unsafe" => format!(
            "{} is not a plain folder at a full path (a link, for example), so Marsa did not \
             delete it. Check it, then delete it yourself.",
            shown("The data folder")
        ),
        "uninstall_all_data_delete_failed" => format!(
            "Marsa could not delete all of {}. Check the folder's permissions, then delete \
             what is left yourself.",
            shown("the data folder")
        ),
        "uninstall_all_data_dir_in_use" => format!(
            "Windows cannot delete {} while this marsa runs from it. Delete the folder \
             yourself once this command has ended.",
            shown("the data folder")
        ),
        "uninstall_all_daemon_restarted" => format!(
            "A Marsa daemon started again, so Marsa did not delete {}. Quit the Marsa window \
             and Marsa's icon (in the menu bar or the system tray), run \"marsa daemon stop\", \
             then delete the folder yourself.",
            shown("the data folder")
        ),
        "uninstall_all_data_dir_back" => format!(
            "Marsa deleted the data folder, but a Marsa daemon made {} again meanwhile. Quit \
             the Marsa window and the menu-bar icon, run \"marsa daemon stop\", then delete \
             the folder yourself.",
            shown("it")
        ),

        // `marsa autostart`: the autostart route.
        "autostart_unsupported" => {
            "Starting Marsa at login works on macOS and Linux only for now.".into()
        }
        "tray_not_found" => "The marsa-tray program is not next to marsad, so Marsa cannot \
             start at login. Reinstall Marsa, then try again."
            .into(),
        "autostart_path_invalid" => "The folder of marsa-tray has a character a login file \
             cannot hold: a line break or another control character, or % on Linux. Move \
             Marsa to another folder, then try again."
            .into(),
        "autostart_no_home" => "Cannot find your home folder, so Marsa does not know where \
             the login file goes."
            .into(),
        "autostart_read" => format!(
            "Cannot read {}. Check the file's permissions.",
            shown("the login file")
        ),
        // Writing it and deleting it both fail with this code.
        "autostart_write" => format!(
            "Cannot change {}. Check the permissions of the file and its folder.",
            shown("the login file")
        ),

        // The Xdebug route.
        "xdebug_interrupted" => "The Marsa daemon stopped before the change reached PHP. The \
             setting is saved: Marsa applies it when it starts again."
            .into(),

        // Caddy's own configuration, the PHP settings, and the version
        // lookup of a folder (the codes of `marsa_core::codes`).
        "caddy_admin_unreachable" => {
            "Marsa cannot reach Caddy's admin port. Run \"marsa services status caddy\".".into()
        }
        "caddy_config_rejected" => {
            "Caddy refused the new configuration. Run \"marsa logs caddy\" to see why.".into()
        }
        "caddy_path_not_utf8" => {
            "A path Caddy needs is not valid UTF-8, so Marsa cannot give it to Caddy.".into()
        }
        "caddy_recipe_invalid" => "The Caddy recipe is not valid. Update Marsa.".into(),
        "caddy_unsupported_platform" => "Caddy has no build for this system.".into(),
        "daemon_already_running" => "The Marsa daemon is already running.".into(),
        "no_matching_version" => format!(
            "No installed {name} version matches {}.",
            d.requested.unwrap_or("that request")
        ),
        "no_version" => format!(
            "No {name} version is set for this folder. Run \"marsa runtimes\" to see the installed ones."
        ),
        "not_installed" => format!(
            "{name} {} is not installed. Run \"marsa runtimes\" to see the installed ones.",
            d.requested.unwrap_or("this version")
        ),
        "php_config_write" => {
            "Marsa cannot write the PHP configuration. Check the data folder's permissions.".into()
        }
        "php_fpm_not_installed" => {
            "This PHP version has no php-fpm, so a site cannot use it.".into()
        }
        "php_fpm_unavailable" => {
            "php-fpm is not available for this PHP version. Run \"marsa logs php\" for details."
                .into()
        }
        "php_path_not_allowed" => "That PHP path is not allowed.".into(),
        _ => return None,
    };
    Some(text)
}

fn no_such_version(name: &str, requested: Option<&str>, available: &[String]) -> String {
    let wanted = match requested {
        Some(text) => format!("No {name} version matches \"{text}\"."),
        None => format!("The version list has no stable {name} version."),
    };
    if available.is_empty() {
        format!("{wanted} The version list has no {name} versions.")
    } else {
        format!("{wanted} Available: {}", available.join(", "))
    }
}

pub(super) fn example_version(runtime: Option<Runtime>) -> &'static str {
    match runtime {
        Some(Runtime::Node) => "22",
        Some(Runtime::Composer) => "2.8",
        Some(Runtime::Caddy) => "2.11",
        Some(Runtime::Postgres) => "18",
        Some(Runtime::Mysql) => "8.4",
        Some(Runtime::Valkey) => "8.1",
        Some(Runtime::Mailpit) => "1.34",
        Some(Runtime::Php) | None => "8.4",
    }
}
