//! Tests of the model: the messages from the bridge and the user's actions, with no window.

use super::installs::failure;
use super::*;
use marsa_core::types::LogStream;

fn health() -> Health {
    Health {
        app_version: "0.0.0".into(),
        api_version: 1,
        pid: 4242,
    }
}

fn connected() -> Model {
    let mut model = Model::default();
    model.update(Update::Connected(health()));
    model
}

fn status(id: &str, state: ServiceState) -> ServiceStatus {
    ServiceStatus {
        id: id.into(),
        pid: (state == ServiceState::Running).then_some(7),
        state,
        restarts: 0,
        since_ms: None,
    }
}

fn changed(id: &str, state: ServiceState) -> Event {
    Event::ServiceChanged(status(id, state))
}

fn log_line(service: &str, seq: u64) -> LogLine {
    LogLine {
        service: service.into(),
        seq,
        at_ms: 0,
        stream: LogStream::Stderr,
        text: format!("line {seq}"),
    }
}

fn line(service: &str, seq: u64) -> Event {
    Event::Log(log_line(service, seq))
}

/// The text of each line of the log of `service`.
fn texts<'a>(model: &'a Model, service: &str) -> Vec<&'a str> {
    model
        .log_rows(service)
        .iter()
        .map(|row| row.text.as_str())
        .collect()
}

fn site(name: &str, secure: bool) -> Site {
    let root = PathBuf::from("/code").join(name);
    Site {
        name: name.into(),
        docroot: root.join("public"),
        root,
        kind: marsa_core::types::SiteKind::Laravel,
        php: None,
        php_source: None,
        secure,
    }
}

fn site_list(names: &[&str]) -> SiteList {
    SiteList {
        parked: vec![PathBuf::from("/code")],
        sites: names.iter().map(|name| site(name, true)).collect(),
        skipped: Vec::new(),
    }
}

fn names(model: &Model) -> Vec<&str> {
    model
        .session
        .sites
        .sites
        .iter()
        .map(|s| s.name.as_str())
        .collect()
}

/// An absolute path on this system, from Unix-style parts. `cfg!`
/// because a Windows path needs a drive to be absolute.
fn absolute(unix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
    } else {
        PathBuf::from(unix)
    }
}

#[test]
fn navigate_and_fold_change_the_window_only() {
    let mut model = connected();
    assert_eq!(model.route, Route::Home);
    assert!(!model.sidebar_folded);
    let route = Route::Runtimes(Runtime::Node);
    assert!(
        model
            .apply_action(Action::Navigate(route.clone()))
            .is_none()
    );
    assert_eq!(model.route, route);
    assert!(model.apply_action(Action::FoldSidebar).is_none());
    assert!(model.sidebar_folded);
    assert!(model.apply_action(Action::FoldSidebar).is_none());
    assert!(!model.sidebar_folded);
}

#[test]
fn a_service_action_clears_the_notice_and_becomes_a_command() {
    let mut model = connected();
    model.update(Update::Notice("service_busy".into()));
    let action = Action::Service {
        service: "caddy".into(),
        action: ServiceAction::Restart,
    };
    let command = model.apply_action(action);
    assert!(
        matches!(
            &command,
            Effect::Send(Command::Service { service, action: ServiceAction::Restart }) if service == "caddy"
        ),
        "{command:?}"
    );
    assert_eq!(model.session.notices.code(NoticeSource::Call), None);
}

#[test]
fn the_home_actions_set_the_default_queue_an_install_and_sync_the_hosts() {
    // The default picker: the change goes to the daemon, once.
    let mut model = with_php(Vec::new());
    let set = Action::SetDefault {
        runtime: Runtime::Php,
        version: v("8.3.27"),
    };
    let command = model.apply_action(set.clone());
    let want = Change::Default {
        runtime: Runtime::Php,
        version: "8.3".into(),
    };
    assert!(matches!(&command, Effect::Send(Command::Save(change)) if *change == want));
    assert!(model.apply_action(set).is_none(), "the default already");

    // A fix queues its install; `App` sends it through `next_install`.
    let job = InstallJob {
        runtime: Runtime::Caddy,
        version: None,
    };
    assert!(model.apply_action(Action::Install(job.clone())).is_none());
    assert!(model.installing());
    assert_eq!(model.next_install(), Some(job.clone()));
    // One at a time: a second fix waits for the first to end.
    assert!(!model.queue_install(job));
    assert_eq!(model.next_install(), None);

    // The hosts button only runs with names to add.
    assert!(model.apply_action(Action::SyncHosts).is_none());
    model.update(Update::Hosts(HostsStatus {
        used: true,
        missing: vec!["shop.test".into()],
    }));
    let command = model.apply_action(Action::SyncHosts);
    assert!(
        matches!(command, Effect::Send(Command::SyncHosts)),
        "{command:?}"
    );
    assert!(model.apply_action(Action::SyncHosts).is_none(), "it runs");
    assert_eq!(model.session.hosts_sync, HostsSync::Running);
}

#[test]
fn the_first_run_actions_install_set_up_and_close() {
    let mut model = with_runtimes(&[], &[]);
    assert!(model.apply_action(Action::StartFirstRun).is_none());
    assert_eq!(model.session.first_run, Some(FirstRun::Installing));
    assert_eq!(
        model.next_install(),
        Some(job(Runtime::Php, Some(FIRST_PHP)))
    );

    let command = model.apply_action(Action::RunSetup);
    assert!(
        matches!(command, Effect::Send(Command::Setup)),
        "{command:?}"
    );
    assert_eq!(model.session.setup, Setup::Running);
    assert!(
        model.apply_action(Action::RunSetup).is_none(),
        "one at a time"
    );

    // The folder dialog is `App`'s: the model keeps out of it.
    assert!(model.apply_action(Action::PickFolder(Pick::Park)).is_none());
    assert!(model.apply_action(Action::CloseFirstRun).is_none());
    assert!(model.session.first_run.is_none() && !model.needs_first_run());
}

#[test]
fn a_linked_folder_opens_its_form_on_the_sites_screen() {
    let mut model = connected();
    assert_eq!(model.route, Route::Home);
    model.pick_link_folder(Path::new("relative/folder"));
    assert_eq!(model.route, Route::Home, "a bad folder opens no form");
    assert!(model.link.is_none());
    model.pick_link_folder(&absolute("/code/My_App"));
    assert_eq!(model.route, Route::Sites);
    assert_eq!(
        model.link.as_ref().map(|form| form.name.as_str()),
        Some("my-app")
    );
}

#[test]
fn a_fix_that_installs_needs_the_daemon() {
    let mut model = Model::default();
    let job = InstallJob {
        runtime: Runtime::Php,
        version: Some(FIRST_PHP.into()),
    };
    assert!(!model.queue_install(job));
    assert!(!model.installing());
}

#[test]
fn open_site_changes_nothing_and_the_address_comes_from_the_model() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop"])));
    model.session.sites.sites[0].secure = false;
    assert!(
        model
            .apply_action(Action::OpenSite("shop".into()))
            .is_none()
    );
    assert_eq!(model.route, Route::Home);
    assert_eq!(model.site_url("shop").as_deref(), Some("http://shop.test"));
    model.session.sites.sites[0].secure = true;
    assert_eq!(model.site_url("shop").as_deref(), Some("https://shop.test"));
    assert_eq!(model.site_url("gone"), None);
}

#[test]
fn a_reconnect_keeps_the_route_and_the_fold() {
    let mut model = connected();
    model.apply_action(Action::Navigate(Route::Site("shop".into())));
    model.apply_action(Action::FoldSidebar);
    model.update(Update::Failed("daemon_start_failed".into()));
    model.update(Update::Connected(health()));
    assert_eq!(model.route, Route::Site("shop".into()));
    assert!(model.sidebar_folded);
}

#[test]
fn the_appearance_is_the_windows_own_setting_and_a_reconnect_keeps_it() {
    let mut model = connected();
    assert_eq!(model.appearance, Appearance::System);
    // No command: the daemon has nothing to do with it.
    assert!(
        model
            .apply_action(Action::SetAppearance(Appearance::Dark))
            .is_none()
    );
    assert_eq!(model.appearance, Appearance::Dark);
    model.update(Update::Failed("daemon_start_failed".into()));
    model.update(Update::Connected(health()));
    assert_eq!(model.appearance, Appearance::Dark);
}

#[test]
fn the_sites_search_is_the_models_and_a_reconnect_keeps_it() {
    let mut model = connected();
    assert!(
        model
            .apply_action(Action::SearchSites("sh".into()))
            .is_none()
    );
    assert_eq!(model.sites_search, "sh");
    model.update(Update::Connected(health()));
    assert_eq!(
        model.sites_search, "sh",
        "the user's typing, not daemon state"
    );
}

#[test]
fn the_menu_actions_of_a_site_reach_the_daemon_or_the_confirmation() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop"])));
    let pick = Action::SitePhp {
        site: "shop".into(),
        choice: Choice::Version(v("8.3.27")),
    };
    let command = model.apply_action(pick.clone());
    assert!(
        matches!(command, Effect::Send(Command::Save(_))),
        "{command:?}"
    );
    assert!(model.apply_action(pick).is_none(), "the same entry again");

    model
        .session
        .notices
        .set_sites(SitesNotice::Unlinked("old".into()));
    let secure = Action::SetSecure {
        site: "shop".into(),
        secure: false,
    };
    let command = model.apply_action(secure);
    assert!(
        matches!(command, Effect::Send(Command::Secure { ref site, secure: false }) if site == "shop"),
        "{command:?}"
    );
    assert!(
        model.session.notices.sites().is_none(),
        "the notice was about the last action"
    );

    assert!(
        model
            .apply_action(Action::AskUnlink("shop".into()))
            .is_none()
    );
    assert_eq!(
        model.session.confirm,
        Some(Removal::Unlink {
            site: "shop".into()
        })
    );
    model.cancel_removal();
    let folder = PathBuf::from("/code");
    assert!(
        model
            .apply_action(Action::AskForget(folder.clone()))
            .is_none()
    );
    assert_eq!(model.session.confirm, Some(Removal::Forget { folder }));
}

#[test]
fn the_confirmations_answers_send_the_removal_or_drop_it() {
    let mut model = with_parked();
    model.apply_action(Action::AskUnlink("api".into()));
    assert!(model.apply_action(Action::CancelRemoval).is_none());
    assert!(model.session.confirm.is_none() && model.removing().is_none());
    // Nothing waits: the answer sends nothing.
    assert!(model.apply_action(Action::ConfirmRemoval).is_none());

    model.apply_action(Action::AskForget(PathBuf::from("/code")));
    let sent = model.apply_action(Action::ConfirmRemoval);
    assert!(matches!(sent, Effect::Send(Command::Remove(ref r)) if *r == forget("/code")));
    assert_eq!(model.removing(), Some(&forget("/code")));
    assert!(model.session.confirm.is_none());
}

#[test]
fn a_site_menu_action_waits_for_the_daemon() {
    let mut down = Model::default();
    let secure = Action::SetSecure {
        site: "shop".into(),
        secure: false,
    };
    assert!(down.apply_action(secure).is_none());
    let pick = Action::SitePhp {
        site: "shop".into(),
        choice: Choice::Auto,
    };
    assert!(down.apply_action(pick).is_none());
    // The confirmation needs the daemon too: nothing to remove without it.
    assert!(
        down.apply_action(Action::AskUnlink("shop".into()))
            .is_none()
    );
    assert!(down.session.confirm.is_none());
}

#[test]
fn the_link_form_actions_edit_send_and_cancel() {
    let mut model = connected();
    model.pick_link_folder(&absolute("/work/blog"));
    model.apply_action(Action::SetLinkName("my-blog".into()));
    assert_eq!(
        model.link.as_ref().map(|f| f.name.as_str()),
        Some("my-blog")
    );

    // A name that is not valid sends nothing and says why.
    model.apply_action(Action::SetLinkName("My Blog".into()));
    assert!(model.apply_action(Action::SubmitLink).is_none());
    assert!(matches!(
        model.session.notices.sites().cloned(),
        Some(SitesNotice::Failed(_))
    ));
    model.apply_action(Action::DismissNotice(NoticeSource::Sites));
    assert!(model.session.notices.sites().is_none());

    model.apply_action(Action::SetLinkName("blog".into()));
    let command = model.apply_action(Action::SubmitLink);
    assert!(
        matches!(command, Effect::Send(Command::Link { ref name, .. }) if name == "blog"),
        "{command:?}"
    );
    model.apply_action(Action::CancelLink);
    assert!(model.link.is_none());
    // With no form there is nothing to send.
    assert!(model.apply_action(Action::SubmitLink).is_none());
    // With no daemon the form sends nothing either.
    let mut down = Model::default();
    down.pick_link_folder(&absolute("/work/blog"));
    assert!(down.apply_action(Action::SubmitLink).is_none());
}

#[test]
fn overall_says_the_daemon_first_then_the_sites_then_the_web_server() {
    let mut model = Model::default();
    assert_eq!(model.overall(), Overall::Connecting);
    model.update(Update::Failed("daemon_start_failed".into()));
    assert_eq!(model.overall(), Overall::DaemonDown);
    model.update(Update::Connected(health()));
    // No site yet: nothing fails to open, whatever Caddy does.
    assert_eq!(model.overall(), Overall::NoSites);
    model.update(Update::Sites(site_list(&["shop"])));
    // Caddy not listed yet counts as down.
    assert_eq!(model.overall(), Overall::WebServerDown);
    for (state, overall) in [
        (ServiceState::Starting, Overall::Starting),
        (ServiceState::Running, Overall::Working),
        (ServiceState::Stopping, Overall::WebServerDown),
        (ServiceState::Stopped, Overall::WebServerDown),
        (
            ServiceState::Failed {
                code: "port_in_use".into(),
            },
            Overall::WebServerDown,
        ),
    ] {
        model.update(Update::Services(vec![
            status("php-fpm-8.4", ServiceState::Running),
            status(WEB_SERVER, state),
        ]));
        assert_eq!(model.overall(), overall);
    }
    model.update(Update::Reconnecting);
    assert_eq!(model.overall(), Overall::Connecting);
}

#[test]
fn a_service_event_changes_the_table() {
    let mut model = connected();
    apply(&mut model, changed("php", ServiceState::Stopped));
    apply(&mut model, changed("caddy", ServiceState::Starting));
    apply(&mut model, changed("caddy", ServiceState::Running));
    let expected = [
        status("caddy", ServiceState::Running),
        status("php", ServiceState::Stopped),
    ];
    assert_eq!(model.session.services, expected);
}

#[test]
fn the_log_keeps_the_newest_5000_lines() {
    let mut model = connected();
    for seq in 0..6000 {
        apply(&mut model, line("caddy", seq));
    }
    let texts = texts(&model, "caddy");
    assert_eq!(texts.len(), MAX_LOG_LINES);
    assert_eq!((texts[0], texts[4999]), ("line 1000", "line 5999"));
}

#[test]
fn a_repeated_or_old_seq_is_ignored() {
    let mut model = connected();
    model.update(Update::Log(vec![log_line("caddy", 4)]));
    for seq in [4, 3, 5, 5] {
        apply(&mut model, line("caddy", seq));
    }
    assert_eq!(texts(&model, "caddy"), ["line 4", "line 5"]);
}

#[test]
fn a_new_run_may_start_its_seq_again() {
    let mut model = connected();
    apply(&mut model, line("caddy", 9));
    apply(&mut model, changed("caddy", ServiceState::Starting));
    apply(&mut model, line("caddy", 0));
    assert_eq!(texts(&model, "caddy"), ["line 9", "line 0"]);
}

#[test]
fn each_service_has_its_own_log_with_its_own_seq_and_limit() {
    let mut model = connected();
    apply(&mut model, line("caddy", 5));
    apply(&mut model, line("php-fpm-8.4", 0));
    apply(&mut model, line("php-fpm-8.4", 1));
    // The same seq in another service is another line.
    apply(&mut model, line("caddy", 5));
    assert_eq!(texts(&model, "caddy"), ["line 5"]);
    assert_eq!(texts(&model, "php-fpm-8.4"), ["line 0", "line 1"]);
    assert!(model.log_rows("php-fpm-8.3").is_empty());
    // A new run of one service does not let the other repeat a line.
    apply(&mut model, changed("php-fpm-8.4", ServiceState::Starting));
    apply(&mut model, line("php-fpm-8.4", 0));
    apply(&mut model, line("caddy", 5));
    assert_eq!(texts(&model, "php-fpm-8.4").len(), 3);
    assert_eq!(texts(&model, "caddy").len(), 1);
    // Each log is kept to the newest lines, separately.
    for seq in 0..6000 {
        apply(&mut model, line("php-fpm-8.3", seq));
    }
    assert_eq!(model.log_rows("php-fpm-8.3").len(), MAX_LOG_LINES);
    assert_eq!(model.log_rows("caddy").len(), 1);
}

#[test]
fn a_line_has_its_level_from_its_own_text_and_its_stream() {
    let mut model = connected();
    let line = |text: &str, seq| LogLine {
        text: text.to_owned(),
        stream: LogStream::Stdout,
        ..log_line("caddy", seq)
    };
    model.update(Update::Log(vec![
        line(r#"{"level":"error","msg":"boom"}"#, 0),
        line(r#"{"level":"warn","msg":"hm"}"#, 1),
        line("ready", 2),
    ]));
    let levels: Vec<_> = model.log_rows("caddy").iter().map(|r| r.level).collect();
    assert_eq!(levels, [LogLevel::Error, LogLevel::Warning, LogLevel::Info]);
    // The columns are those of the message, not of the JSON around it.
    let columns: Vec<_> = model.log_rows("caddy").iter().map(|r| r.columns).collect();
    assert_eq!(columns, ["boom".len(), "hm".len(), "ready".len()]);
    // The text stays as the program wrote it.
    assert_eq!(
        model.log_rows("caddy")[0].text,
        r#"{"level":"error","msg":"boom"}"#
    );
    assert_eq!(model.log_rows("caddy")[0].stream, LogStream::Stdout);
}

#[test]
fn the_log_of_a_service_the_daemon_stopped_listing_goes_with_it() {
    let mut model = connected();
    model.update(Update::Services(vec![
        status("caddy", ServiceState::Running),
        status("php-fpm-8.3", ServiceState::Running),
    ]));
    apply(&mut model, line("caddy", 0));
    apply(&mut model, line("php-fpm-8.3", 0));
    model.update(Update::Services(vec![status(
        "caddy",
        ServiceState::Running,
    )]));
    assert_eq!(texts(&model, "caddy"), ["line 0"]);
    assert!(model.log_rows("php-fpm-8.3").is_empty());
}

#[test]
fn a_log_source_is_a_service_and_caddy_is_never_written_twice() {
    assert_eq!(LogSource::of("caddy"), LogSource::Caddy);
    assert_eq!(LogSource::default(), LogSource::Caddy);
    let pool = LogSource::of("php-fpm-8.4");
    assert_eq!(pool, LogSource::Service("php-fpm-8.4".into()));
    assert_eq!((LogSource::Caddy.id(), pool.id()), ("caddy", "php-fpm-8.4"));
}

#[test]
fn the_log_filters_and_follow_are_the_users_and_survive_a_new_session() {
    let mut model = connected();
    assert!(!model.log_paused && model.log_hidden.is_empty());
    // They are the window's own: nothing goes to the daemon.
    let local = [
        Action::ToggleLogLevel(LogLevel::Info),
        Action::SetLogFilter("shop".into()),
        Action::SetLogFollow(false),
    ];
    for action in local {
        assert!(model.apply_action(action).is_none());
    }
    assert!(model.log_hidden.hides(LogLevel::Info) && model.log_paused);
    assert_eq!(model.log_filter, "shop");
    // A restarted daemon is a new session; the user's choices stay.
    model.update(Update::Connected(health()));
    assert!(model.log_hidden.hides(LogLevel::Info));
    assert_eq!(
        (model.log_filter.as_str(), model.log_paused),
        ("shop", true)
    );
    model.apply_action(Action::SetLogFollow(true));
    model.apply_action(Action::ToggleLogLevel(LogLevel::Info));
    assert!(!model.log_paused && model.log_hidden.is_empty());
}

#[test]
fn shutting_down_changes_the_status_line() {
    let mut model = connected();
    apply(&mut model, changed("caddy", ServiceState::Running));
    apply(&mut model, Event::ShuttingDown);
    assert_eq!(model.session.connection, Connection::ShuttingDown);
    assert!(model.session.services.is_empty());
    assert_eq!(
        crate::strings::status_line(&model.session.connection),
        "Daemon: shutting down"
    );
}

#[test]
fn a_new_session_starts_clean_and_a_lost_one_keeps_the_log() {
    let mut model = connected();
    apply(&mut model, line("caddy", 3));
    model.update(Update::Notice("service_not_found".into()));
    model.update(Update::Reconnecting);
    assert_eq!(model.session.connection, Connection::Reconnecting);
    assert_eq!(texts(&model, "caddy"), ["line 3"]);

    model.update(Update::Connected(health()));
    assert!(model.is_connected());
    assert!(
        model.log_rows("caddy").is_empty()
            && model.session.notices.code(NoticeSource::Call).is_none()
    );
    // The restarted daemon counts from 0 again.
    apply(&mut model, line("caddy", 0));
    assert_eq!(texts(&model, "caddy"), ["line 0"]);
}

#[test]
fn buttons_match_the_state() {
    use ServiceAction::{Restart, Start, Stop};
    let failed = ServiceState::Failed {
        code: "exited".into(),
    };
    let cases = [
        (ServiceState::Stopped, [true, false, false]),
        (failed, [true, false, false]),
        (ServiceState::Starting, [false, true, true]),
        (ServiceState::Running, [false, true, true]),
        (ServiceState::Stopping, [false, false, false]),
    ];
    for (state, expected) in cases {
        assert_eq!(
            [Start, Stop, Restart].map(|a| can(&state, a)),
            expected,
            "{state:?}"
        );
    }
}

#[test]
fn a_sites_event_replaces_the_whole_list() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["blog", "shop"])));
    let mut next = site_list(&["api"]);
    next.skipped.push(marsa_core::types::SkippedFolder {
        path: PathBuf::from("/code/متجر"),
        code: "site_name_invalid".into(),
    });
    apply(&mut model, Event::SitesChanged(next.clone()));
    assert_eq!(model.session.sites, next);
}

#[test]
fn sites_are_sorted_by_name() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop", "api", "blog"])));
    assert_eq!(names(&model), ["api", "blog", "shop"]);
    apply(&mut model, Event::SitesChanged(site_list(&["zoo", "app"])));
    assert_eq!(names(&model), ["app", "zoo"]);
}

#[test]
fn the_url_follows_the_https_setting() {
    assert_eq!(url(&site("shop", true)), "https://shop.test");
    assert_eq!(url(&site("shop", false)), "http://shop.test");
}

#[test]
fn the_sites_go_with_the_connection_but_the_link_form_stays() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop"])));
    model.pick_link_folder(&absolute("/work/api"));
    model.update(Update::Reconnecting);
    assert!(model.session.sites.sites.is_empty() && model.session.sites.parked.is_empty());

    model.update(Update::Connected(health()));
    let form = model.link.as_ref().unwrap();
    assert_eq!(
        (form.folder.clone(), form.name.as_str()),
        (absolute("/work/api"), "api")
    );
    assert!(model.session.notices.sites().is_none());
}

#[test]
fn a_picked_folder_needs_an_absolute_path() {
    let code = absolute("/code");
    let shown = code.display().to_string();
    for input in [shown.clone(), format!("{shown}/"), format!("{shown}/./")] {
        assert_eq!(
            folder_path(Path::new(&input)),
            Ok(code.clone()),
            "{input:?}"
        );
    }
    for input in ["", "code", "./code", "~/code"] {
        let got = folder_path(Path::new(input));
        assert_eq!(got, Err("site_path_relative"), "{input:?}");
    }
}

#[test]
fn a_relative_folder_is_refused_without_a_request() {
    let mut model = connected();
    assert_eq!(model.park_request(Path::new("code")), None);
    let failed = SitesNotice::Failed("site_path_relative".into());
    assert_eq!(model.session.notices.sites().cloned(), Some(failed.clone()));
    model.pick_link_folder(Path::new("code"));
    assert!(model.link.is_none());
    assert_eq!(model.session.notices.sites().cloned(), Some(failed));

    let picked = format!("{}/", absolute("/code").display());
    assert_eq!(
        model.park_request(Path::new(&picked)),
        Some(absolute("/code"))
    );
    assert!(model.session.notices.sites().is_none());
}

#[test]
fn a_park_says_so() {
    let mut model = connected();
    model.update(Update::Parked(absolute("/code")));
    let parked = SitesNotice::Parked(absolute("/code"));
    assert_eq!(model.session.notices.sites().cloned(), Some(parked));
}

#[test]
fn a_failed_sites_call_keeps_the_list_and_shows_the_code() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop"])));
    model.update(Update::SitesFailed("site_not_found".into()));
    assert_eq!(names(&model), ["shop"]);
    let failed = SitesNotice::Failed("site_not_found".into());
    assert_eq!(model.session.notices.sites().cloned(), Some(failed));
}

// -- The PHP and Node lists --

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

/// The daemon's runtimes list: PHP and Node with these versions and
/// defaults, Composer and Caddy with none.
fn runtime_list(php: (&[&str], Option<&str>), node: (&[&str], Option<&str>)) -> RuntimeList {
    let row = |runtime, (installed, default): (&[&str], Option<&str>)| {
        marsa_core::types::RuntimeVersions {
            runtime,
            installed: installed.iter().map(|t| (*t).to_owned()).collect(),
            default: default.map(str::to_owned),
        }
    };
    RuntimeList {
        runtimes: vec![
            row(Runtime::Php, php),
            row(Runtime::Node, node),
            row(Runtime::Composer, (&["2.8.3"], None)),
            row(Runtime::Caddy, (&[], None)),
        ],
    }
}

fn php_site(name: &str, php: Option<&str>, source: Option<Source>) -> Site {
    Site {
        php: php.map(v),
        php_source: source,
        ..site(name, true)
    }
}

/// Connected, with PHP 8.3.27 and 8.4.23 (default `8.4`), Node 22, and
/// these sites.
fn with_php(sites: Vec<Site>) -> Model {
    let mut model = connected();
    let php: &[&str] = &["8.4.23", "8.3.27"];
    model.update(Update::Runtimes(runtime_list(
        (php, Some("8.4")),
        (&["22.11.0"], None),
    )));
    model.update(Update::Sites(SiteList {
        sites,
        ..SiteList::default()
    }));
    model
}

fn site_named<'a>(model: &'a Model, name: &str) -> &'a Site {
    model
        .session
        .sites
        .sites
        .iter()
        .find(|s| s.name == name)
        .unwrap()
}

fn choice(model: &Model, name: &str) -> Choice {
    model.site_choice(site_named(model, name))
}

fn site_php(site: &str, php: Option<&str>) -> Change {
    Change::SitePhp {
        site: site.into(),
        php: php.map(str::to_owned),
    }
}

#[test]
fn the_runtimes_list_has_each_default() {
    let model = with_php(Vec::new());
    assert_eq!(model.php_versions(), [v("8.3.27"), v("8.4.23")]);
    assert_eq!(model.default_choice(Runtime::Php), Some(v("8.4.23")));
    // Node has no default yet; Composer never has one.
    assert_eq!(model.default_choice(Runtime::Node), None);
    assert_eq!(model.default_choice(Runtime::Composer), None);
    assert!(has_default(Runtime::Node) && !has_default(Runtime::Caddy));

    // A default that matches nothing installed picks nothing; its text
    // stays for the window to show.
    let mut model = connected();
    model.update(Update::Runtimes(runtime_list(
        (&["8.4.23"], Some("8.2")),
        (&[], None),
    )));
    assert_eq!(model.default_choice(Runtime::Php), None);
    assert_eq!(model.session.runtimes[0].default.as_deref(), Some("8.2"));
}

#[test]
fn picking_a_default_sends_its_line_and_shows_it_until_the_answer() {
    let mut model = with_php(Vec::new());
    let change = model.choose_default(Runtime::Php, v("8.3.27"));
    let want = Change::Default {
        runtime: Runtime::Php,
        version: "8.3".into(),
    };
    assert_eq!(change, Some(want.clone()));
    assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));

    // Saved: the daemon's value again, which its event has changed.
    apply(&mut model, default_changed(Runtime::Php, "8.3", "8.3.27"));
    model.update(Update::Saved(want));
    assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));
    assert_eq!(model.session.runtimes[0].default.as_deref(), Some("8.3"));

    // The default already, and a runtime with no default: nothing to send.
    assert_eq!(model.choose_default(Runtime::Php, v("8.3.27")), None);
    assert_eq!(model.choose_default(Runtime::Composer, v("2.8.3")), None);
    // Node lines are majors.
    let node = model.choose_default(Runtime::Node, v("22.11.0"));
    let want = Change::Default {
        runtime: Runtime::Node,
        version: "22".into(),
    };
    assert_eq!(node, Some(want));
}

fn default_changed(runtime: Runtime, default: &str, version: &str) -> Event {
    Event::DefaultChanged(marsa_core::types::DefaultVersion {
        runtime,
        default: default.into(),
        version: version.into(),
    })
}

#[test]
fn a_refused_default_goes_back_and_says_why() {
    let mut model = with_php(Vec::new());
    let change = model.choose_default(Runtime::Php, v("8.3.27")).unwrap();
    let code = "version_not_installed".to_owned();
    model.update(Update::NotSaved { change, code });
    assert_eq!(model.default_choice(Runtime::Php), Some(v("8.4.23")));
    assert_eq!(
        model.session.notices.code(NoticeSource::Runtimes),
        Some("version_not_installed")
    );
    // The next pick clears the notice.
    model.choose_default(Runtime::Php, v("8.3.27"));
    assert!(model.session.notices.code(NoticeSource::Runtimes).is_none());
}

#[test]
fn a_default_set_elsewhere_reaches_the_list() {
    // `marsa use php 8.3` in a terminal.
    let mut model = with_php(Vec::new());
    apply(&mut model, default_changed(Runtime::Php, "8.3", "8.3.27"));
    assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));
}

#[test]
fn the_line_is_saved_only_when_it_picks_the_same_version() {
    let installed = [v("8.3.27"), v("8.4.22"), v("8.4.23"), v("8.5.0-rc1")];
    for (runtime, chosen, want) in [
        (Runtime::Php, "8.4.23", "8.4"),
        // An older patch release stays the one used.
        (Runtime::Php, "8.4.22", "8.4.22"),
        (Runtime::Php, "8.3.27", "8.3"),
        // `8.5` never picks a pre-release.
        (Runtime::Php, "8.5.0-rc1", "8.5.0-rc1"),
    ] {
        assert_eq!(
            version_text(runtime, &v(chosen), &installed),
            want,
            "{chosen}"
        );
    }
    let node = [v("20.18.0"), v("22.11.0")];
    assert_eq!(version_text(Runtime::Node, &v("22.11.0"), &node), "22");
}

#[test]
fn a_site_that_follows_the_default_can_be_pinned() {
    let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![shop]);
    assert_eq!(choice(&model, "shop"), Choice::Auto);
    assert_eq!(model.choose_site_php("shop", Choice::Auto), None);

    // The same version as the default is a pin too: the default may move.
    let change = model.choose_site_php("shop", Choice::Version(v("8.4.23")));
    assert_eq!(change, Some(site_php("shop", Some("8.4"))));
    assert_eq!(choice(&model, "shop"), Choice::Version(v("8.4.23")));
}

#[test]
fn a_site_with_a_version_file_keeps_it_until_a_version_is_picked() {
    let file = Source::VersionFile(PathBuf::from("/code/shop/.php-version"));
    let shop = php_site("shop", Some("8.3.27"), Some(file));
    let mut model = with_php(vec![shop]);
    assert_eq!(choice(&model, "shop"), Choice::Auto);

    let change = model.choose_site_php("shop", Choice::Version(v("8.4.23")));
    assert_eq!(change.clone(), Some(site_php("shop", Some("8.4"))));
    // Saved, and the new list says the site's own setting won.
    let pinned = php_site("shop", Some("8.4.23"), Some(Source::ProjectSetting));
    apply(
        &mut model,
        Event::SitesChanged(SiteList {
            sites: vec![pinned],
            ..SiteList::default()
        }),
    );
    model.update(Update::Saved(change.unwrap()));
    assert_eq!(choice(&model, "shop"), Choice::Version(v("8.4.23")));
}

#[test]
fn the_first_entry_clears_the_sites_own_setting() {
    let shop = php_site("shop", Some("8.3.27"), Some(Source::ProjectSetting));
    let mut model = with_php(vec![shop]);
    assert_eq!(choice(&model, "shop"), Choice::Version(v("8.3.27")));
    assert_eq!(
        model.choose_site_php("shop", Choice::Version(v("8.3.27"))),
        None
    );

    let change = model.choose_site_php("shop", Choice::Auto);
    assert_eq!(change, Some(site_php("shop", None)));
    assert_eq!(choice(&model, "shop"), Choice::Auto);
    // No such site: nothing to send.
    assert_eq!(model.choose_site_php("gone", Choice::Auto), None);
}

#[test]
fn a_refused_site_php_goes_back_and_says_why() {
    let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![shop]);
    let change = model.choose_site_php("shop", Choice::Version(v("8.3.27")));
    let code = "config_write".to_owned();
    model.update(Update::NotSaved {
        change: change.unwrap(),
        code,
    });
    assert_eq!(choice(&model, "shop"), Choice::Auto);
    let failed = SitesNotice::Failed("config_write".into());
    assert_eq!(model.session.notices.sites().cloned(), Some(failed));
}

#[test]
fn the_newest_pick_shows_until_its_own_answer() {
    let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![shop]);
    let first = model.choose_site_php("shop", Choice::Version(v("8.3.27")));
    let second = model.choose_site_php("shop", Choice::Auto);
    assert_eq!(second, Some(site_php("shop", None)));
    // The first answer does not hide the second pick.
    model.update(Update::Saved(first.unwrap()));
    assert_eq!(choice(&model, "shop"), Choice::Auto);
}

#[test]
fn a_sites_event_moves_the_lists_without_a_reload() {
    let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![shop]);
    // `marsa` pinned it in a terminal.
    let pinned = php_site("shop", Some("8.3.27"), Some(Source::ProjectSetting));
    apply(
        &mut model,
        Event::SitesChanged(SiteList {
            sites: vec![pinned],
            ..SiteList::default()
        }),
    );
    assert_eq!(choice(&model, "shop"), Choice::Version(v("8.3.27")));
}

#[test]
fn a_lost_session_drops_the_runtimes_and_the_picks() {
    let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![shop.clone()]);
    model.choose_site_php("shop", Choice::Version(v("8.3.27")));
    model.update(Update::Reconnecting);
    assert!(model.session.runtimes.is_empty() && model.php_versions().is_empty());
    // The pick was not saved: the old value shows with the new session.
    model.update(Update::Connected(health()));
    model.update(Update::Sites(SiteList {
        sites: vec![shop],
        ..SiteList::default()
    }));
    assert_eq!(choice(&model, "shop"), Choice::Auto);
    model.update(Update::RuntimesFailed("runtimes_read".into()));
    assert_eq!(
        model.session.notices.code(NoticeSource::Runtimes),
        Some("runtimes_read")
    );
}

// -- Pools (ADR 0016) --

#[test]
fn php_fpm_and_php_cgi_pools_are_managed_by_the_sites() {
    assert!(is_pool("php-fpm-8.4"));
    assert!(is_pool("php-cgi-8.4"));
    for id in ["caddy", "php", "php-fpm", "my-php-fpm-8.4", "dns"] {
        assert!(!is_pool(id), "{id}");
    }
}

#[test]
fn a_pool_id_gives_its_program_and_its_php_line() {
    assert_eq!(pool_of("php-fpm-8.4"), Some((PoolKind::Fpm, "8.4")));
    assert_eq!(pool_of("php-cgi-8.3"), Some((PoolKind::Cgi, "8.3")));
    for id in ["caddy", "php-fpm", "mailpit", ""] {
        assert_eq!(pool_of(id), None, "{id}");
    }
}

#[test]
fn a_line_counts_the_sites_that_run_a_version_of_it() {
    let site = |name: &str, php: Option<&str>| Site {
        name: name.into(),
        root: PathBuf::from("/code").join(name),
        docroot: PathBuf::from("/code").join(name).join("public"),
        kind: marsa_core::types::SiteKind::Laravel,
        php: php.map(|text| text.parse().expect("a version")),
        php_source: None,
        secure: true,
    };
    let mut model = connected();
    model.update(Update::Sites(SiteList {
        sites: vec![
            site("a", Some("8.4.27")),
            site("b", Some("8.4.1")),
            site("c", Some("8.3.27")),
            site("d", Some("8.40.0")),
            site("e", None),
        ],
        ..SiteList::default()
    }));
    assert_eq!(model.sites_on_line("8.4"), 2);
    assert_eq!(model.sites_on_line("8.3"), 1);
    // `8.40` is another line than `8.4`, and a text that is no line has none.
    assert_eq!(model.sites_on_line("8.40"), 1);
    for line in ["8", "", "x.y", "8.", "9.9"] {
        assert_eq!(model.sites_on_line(line), 0, "{line:?}");
    }
}

// -- Installs --

use marsa_core::types::{InstallDone, SetupSkip};

fn job(runtime: Runtime, version: Option<&str>) -> InstallJob {
    InstallJob {
        runtime,
        version: version.map(str::to_owned),
    }
}

fn install(id: u64, update: InstallUpdate) -> Event {
    Event::Install(InstallProgress { id, update })
}

fn running(id: u64, event: InstallEvent) -> Event {
    install(id, InstallUpdate::Running(event))
}

fn downloading(id: u64, done: u64, total: u64) -> Event {
    let artifact = "cli".to_owned();
    running(
        id,
        InstallEvent::Downloading {
            artifact,
            done,
            total,
        },
    )
}

fn done(id: u64, runtime: Runtime, version: &str) -> Event {
    install(
        id,
        InstallUpdate::Done(InstallDone {
            runtime,
            version: version.into(),
            dir: PathBuf::from("/d/runtimes").join(version),
            already: false,
        }),
    )
}

fn failed(id: u64, code: &str) -> Event {
    install(id, InstallUpdate::Failed(failure(code.into())))
}

fn step(model: &Model) -> InstallStep {
    model.install_view().unwrap().step.clone()
}

/// Connected, with this list read: PHP and Caddy as given, no Node or
/// Composer.
fn with_runtimes(php: &[&str], caddy: &[&str]) -> Model {
    let mut model = connected();
    model.update(Update::Runtimes(list(php, caddy)));
    model
}

fn list(php: &[&str], caddy: &[&str]) -> RuntimeList {
    let row = |runtime, installed: &[&str]| marsa_core::types::RuntimeVersions {
        runtime,
        installed: installed.iter().map(|t| (*t).to_owned()).collect(),
        default: None,
    };
    RuntimeList {
        runtimes: vec![
            row(Runtime::Php, php),
            row(Runtime::Node, &[]),
            row(Runtime::Composer, &[]),
            row(Runtime::Caddy, caddy),
        ],
    }
}

#[test]
fn the_version_field_follows_the_installers_rules() {
    for (text, want) in [
        ("", None),
        ("   ", None),
        ("8.4", Some("8.4")),
        (" 8.4.23 ", Some("8.4.23")),
        ("22", Some("22")),
    ] {
        assert_eq!(
            install_version(text),
            Ok(want.map(str::to_owned)),
            "{text:?}"
        );
    }
    assert_eq!(install_version("latest"), Err("unsupported_alias"));
    assert_eq!(install_version("8.x.y"), Err("invalid_version"));
}

#[test]
fn an_install_runs_one_job_at_a_time_and_shows_each_step() {
    let mut model = with_runtimes(&["8.3.27"], &["2.11.7"]);
    model.set_install_text(Runtime::Php, " 8.4 ".into());
    assert!(model.request_install(Runtime::Php));
    // One job at a time: a second press waits for the first.
    assert!(!model.request_install(Runtime::Node));
    assert!(model.installing());

    let sent = model.next_install();
    assert_eq!(sent, Some(job(Runtime::Php, Some("8.4"))));
    assert_eq!(model.next_install(), None);
    assert_eq!(step(&model), InstallStep::Waiting);

    model.update(Update::InstallAccepted {
        job: sent.unwrap(),
        id: 7,
    });
    apply(
        &mut model,
        running(7, InstallEvent::ManifestVerified { serial: 2 }),
    );
    assert_eq!(step(&model), InstallStep::ListChecked);
    let selected = InstallEvent::Selected {
        runtime: Runtime::Php,
        version: "8.4.23".into(),
    };
    apply(&mut model, running(7, selected));
    apply(&mut model, downloading(7, 1, 4));
    let view = model.install_view().unwrap();
    assert_eq!(view.version.as_deref(), Some("8.4.23"));
    assert!((view.step.fraction() - 0.25).abs() < f32::EPSILON);
    let artifact = "cli".to_owned();
    apply(
        &mut model,
        running(7, InstallEvent::Extracting { artifact }),
    );
    assert_eq!(step(&model).fraction(), 1.0);
    apply(&mut model, running(7, InstallEvent::Checking));
    let dir = PathBuf::from("/d");
    apply(&mut model, running(7, InstallEvent::Installed { dir }));
    assert_eq!(step(&model), InstallStep::Finishing);

    apply(&mut model, done(7, Runtime::Php, "8.4.23"));
    assert_eq!(step(&model), InstallStep::Done { already: false });
    assert!(!model.installing());
    model.set_install_text(Runtime::Node, "22".into());
    assert!(model.request_install(Runtime::Node));
}

#[test]
fn a_bad_version_field_is_refused_without_a_request() {
    let mut model = with_runtimes(&[], &[]);
    model.set_install_text(Runtime::Node, "lts/*".into());
    assert!(!model.request_install(Runtime::Node));
    assert_eq!(
        model.session.notices.code(NoticeSource::Runtimes),
        Some("unsupported_alias")
    );
    assert_eq!(model.next_install(), None);
    // An empty field asks for the newest version.
    model.set_install_text(Runtime::Node, String::new());
    assert!(model.request_install(Runtime::Node));
    assert!(model.session.notices.code(NoticeSource::Runtimes).is_none());
    assert_eq!(model.next_install(), Some(job(Runtime::Node, None)));
}

#[test]
fn events_that_come_before_the_answer_are_the_jobs() {
    let mut model = with_runtimes(&[], &["2.11.7"]);
    assert!(model.request_install(Runtime::Composer));
    let sent = model.next_install().unwrap();
    // The event stream is faster than the answer to the request.
    apply(&mut model, downloading(3, 2, 4));
    apply(&mut model, done(3, Runtime::Composer, "2.8.3"));
    assert!(model.installing(), "the job waits for its answer");
    model.update(Update::InstallAccepted { job: sent, id: 3 });
    assert!(!model.installing());
    assert_eq!(step(&model), InstallStep::Done { already: false });
    assert_eq!(model.installs.len(), 1);
}

#[test]
fn a_refused_install_says_why_and_drops_the_rest() {
    let mut model = with_runtimes(&[], &[]);
    assert!(model.start_first_run());
    let sent = model.next_install().unwrap();
    let code = "install_busy".to_owned();
    model.update(Update::InstallRefused { job: sent, code });
    assert_eq!(
        step(&model),
        InstallStep::Failed(failure("install_busy".into()))
    );
    // Caddy is not sent after PHP failed; the offer is back.
    assert_eq!(model.next_install(), None);
    assert!(!model.installing());
    assert_eq!(model.session.first_run, Some(FirstRun::Offer));
}

#[test]
fn an_install_from_a_terminal_shows_too() {
    let mut model = with_runtimes(&[], &[]);
    apply(&mut model, downloading(9, 3, 4));
    let view = model.install_view().unwrap();
    assert_eq!(view.runtime, None);
    assert!((view.step.fraction() - 0.75).abs() < f32::EPSILON);
    // It is not the window's: the buttons stay.
    assert!(!model.installing());
    apply(&mut model, failed(9, "checksum_mismatch"));
    assert_eq!(
        step(&model),
        InstallStep::Failed(failure("checksum_mismatch".into()))
    );
}

#[test]
fn a_lost_session_interrupts_the_install_and_keeps_it_shown() {
    let mut model = with_runtimes(&[], &[]);
    assert!(model.start_first_run());
    let sent = model.next_install().unwrap();
    model.update(Update::InstallAccepted { job: sent, id: 1 });
    apply(&mut model, downloading(1, 1, 2));
    apply(&mut model, Event::ShuttingDown);
    let interrupted = InstallStep::Failed(failure(INSTALL_INTERRUPTED.into()));
    assert_eq!(step(&model), interrupted);
    assert!(!model.installing() && model.session.first_run.is_none());

    // A new daemon counts from 1 again: its install 1 is a new one.
    model.update(Update::Connected(health()));
    assert_eq!(step(&model), interrupted);
    apply(&mut model, downloading(1, 0, 2));
    assert_eq!(model.installs.len(), 2);
    assert_eq!(step(&model).fraction(), 0.0);
}

#[test]
fn at_most_four_installs_are_kept() {
    let mut model = with_runtimes(&[], &[]);
    for id in 1..=6 {
        apply(&mut model, failed(id, "download_failed"));
    }
    let ids: Vec<_> = model.installs.iter().map(|i| i.id).collect();
    assert_eq!(ids, [Some(3), Some(4), Some(5), Some(6)]);
}

// -- First run --

#[test]
fn the_first_run_panel_shows_with_no_php_or_no_caddy() {
    assert!(with_runtimes(&[], &[]).session.first_run == Some(FirstRun::Offer));
    // Either one missing is enough: no site opens without both.
    assert!(with_runtimes(&["8.4.23"], &[]).session.first_run == Some(FirstRun::Offer));
    assert!(with_runtimes(&[], &["2.11.7"]).session.first_run == Some(FirstRun::Offer));
    assert!(
        with_runtimes(&["8.4.23"], &["2.11.7"])
            .session
            .first_run
            .is_none()
    );
    // Not before the list is read.
    assert!(connected().session.first_run.is_none());
    assert!(!first_run_needed(&[]));

    // PHP installed from a terminal: Caddy is still missing. Then Caddy
    // closes the offer.
    let mut model = with_runtimes(&[], &[]);
    model.update(Update::Runtimes(list(&["8.4.23"], &[])));
    assert_eq!(model.session.first_run, Some(FirstRun::Offer));
    model.update(Update::Runtimes(list(&["8.4.23"], &["2.11.7"])));
    assert!(model.session.first_run.is_none());
}

#[test]
fn the_first_run_installs_only_what_is_missing() {
    let mut model = with_runtimes(&["8.4.23"], &[]);
    assert!(model.start_first_run());
    assert_eq!(
        model.next_install(),
        Some(job(Runtime::Caddy, None)),
        "PHP is there: only Caddy"
    );
    let mut model = with_runtimes(&[], &["2.11.7"]);
    assert!(model.start_first_run());
    assert_eq!(
        model.next_install(),
        Some(job(Runtime::Php, Some(FIRST_PHP)))
    );
    model.update(Update::InstallAccepted {
        job: job(Runtime::Php, Some(FIRST_PHP)),
        id: 1,
    });
    apply(&mut model, done(1, Runtime::Php, "8.4.23"));
    assert_eq!(model.next_install(), None, "Caddy is there: no second job");
    assert_eq!(model.session.first_run, Some(FirstRun::Setup));
    // Nothing to install: nothing to start.
    assert!(!with_runtimes(&["8.4.23"], &["2.11.7"]).start_first_run());
}

/// Principles 2: first run replaces Home until PHP and Caddy are
/// installed and there is a first site, unless the user said "Not now".
#[test]
fn first_run_replaces_home_until_the_first_site_can_open() {
    // Not before the runtimes are read, and not without the daemon.
    assert!(!connected().needs_first_run());
    assert!(!Model::default().needs_first_run());

    let mut model = with_runtimes(&[], &[]);
    assert!(model.needs_first_run(), "nothing installed");
    model.update(Update::Runtimes(list(&["8.4.23"], &[])));
    model.update(Update::Sites(site_list(&["shop"])));
    assert!(model.needs_first_run(), "a site cannot open without Caddy");
    model.update(Update::Runtimes(list(&["8.4.23"], &["2.11.7"])));
    assert!(!model.needs_first_run(), "installed, and a first site");
    model.update(Update::Sites(SiteList::default()));
    assert!(model.needs_first_run(), "installed, and no site yet");

    // The first site ends it, as an event from the daemon says.
    apply(&mut model, Event::SitesChanged(site_list(&["shop"])));
    assert!(!model.needs_first_run());
    apply(&mut model, Event::SitesChanged(SiteList::default()));
    assert!(model.needs_first_run());
    model.close_first_run();
    assert!(!model.needs_first_run(), "\"Not now\" stays for the window");
    // A lost session does not bring it back.
    model.update(Update::Reconnecting);
    model.update(Update::Connected(health()));
    model.update(Update::Runtimes(list(&[], &[])));
    assert!(!model.needs_first_run());
}

#[test]
fn the_first_run_installs_php_then_caddy_then_offers_setup() {
    let mut model = with_runtimes(&[], &[]);
    assert!(model.start_first_run());
    assert!(!model.start_first_run(), "once");
    assert_eq!(model.session.first_run, Some(FirstRun::Installing));

    let php = model.next_install().unwrap();
    assert_eq!(php, job(Runtime::Php, Some(FIRST_PHP)));
    model.update(Update::InstallAccepted { job: php, id: 1 });
    apply(&mut model, done(1, Runtime::Php, "8.4.23"));
    // The bridge reads the list again after each install.
    model.update(Update::Runtimes(list(&["8.4.23"], &[])));
    assert_eq!(model.session.first_run, Some(FirstRun::Installing));

    let caddy = model.next_install().unwrap();
    assert_eq!(caddy, job(Runtime::Caddy, None));
    model.update(Update::InstallAccepted { job: caddy, id: 2 });
    apply(&mut model, done(2, Runtime::Caddy, "2.11.7"));
    assert_eq!(model.session.first_run, Some(FirstRun::Setup));
    model.update(Update::Runtimes(list(&["8.4.23"], &["2.11.7"])));
    assert_eq!(model.session.first_run, Some(FirstRun::Setup));
    assert_eq!(model.next_install(), None);
}

#[test]
fn a_failed_caddy_offers_the_install_again_once_php_is_there() {
    let mut model = with_runtimes(&[], &[]);
    model.start_first_run();
    let php = model.next_install().unwrap();
    model.update(Update::InstallAccepted { job: php, id: 1 });
    apply(&mut model, done(1, Runtime::Php, "8.4.23"));
    model.update(Update::Runtimes(list(&["8.4.23"], &[])));
    let caddy = model.next_install().unwrap();
    model.update(Update::InstallAccepted { job: caddy, id: 2 });
    apply(&mut model, failed(2, "download_failed"));
    // PHP is there and Caddy is not: the panel offers the install again.
    assert_eq!(model.session.first_run, Some(FirstRun::Offer));
    assert_eq!(
        step(&model),
        InstallStep::Failed(failure("download_failed".into()))
    );
}

#[test]
fn a_closed_first_run_stays_closed() {
    let mut model = with_runtimes(&[], &[]);
    model.close_first_run();
    model.update(Update::Runtimes(list(&[], &[])));
    assert!(model.session.first_run.is_none());
    model.update(Update::Reconnecting);
    model.update(Update::Connected(health()));
    model.update(Update::Runtimes(list(&[], &[])));
    assert!(model.session.first_run.is_none());
    assert!(!model.start_first_run());
}

// -- Setup --

fn report(code: &str) -> SetupReport {
    SetupReport {
        skipped: vec![SetupSkip {
            op: "trust_install".into(),
            code: code.into(),
        }],
        ..SetupReport::default()
    }
}

/// "Add to /etc/hosts" runs only with something to add,
/// one at a time, never during a setup, and says why when the names
/// did not go in.
#[test]
fn the_hosts_button_runs_only_with_names_to_add() {
    use marsa_core::helper::OpResult;
    use marsa_core::types::{ApiError, SetupSkip};

    let mut model = connected();
    assert!(!model.start_hosts_sync(), "nothing to add");
    model.update(Update::Hosts(HostsStatus {
        used: false,
        missing: vec!["shop.test".into()],
    }));
    assert!(!model.start_hosts_sync(), "not the hosts way");
    model.update(Update::Hosts(HostsStatus {
        used: true,
        missing: vec!["shop.test".into()],
    }));
    model.session.setup = Setup::Running;
    assert!(!model.start_hosts_sync(), "during a setup");
    model.session.setup = Setup::Idle;
    assert!(model.start_hosts_sync());
    assert!(!model.start_hosts_sync(), "one at a time");

    let report = |error: Option<&str>, skipped: Option<&str>, failed: Option<&str>| SetupReport {
        results: failed
            .map(|code| OpResult {
                op: "hosts_sync".into(),
                status: OpStatus::Failed,
                code: Some(code.into()),
                message: None,
            })
            .into_iter()
            .collect(),
        skipped: skipped
            .map(|code| SetupSkip {
                op: "hosts_sync".into(),
                code: code.into(),
            })
            .into_iter()
            .collect(),
        error: error.map(|code| ApiError {
            code: code.into(),
            message: "x".into(),
        }),
        ..SetupReport::default()
    };
    model.update(Update::HostsSynced(report(None, None, None)));
    assert_eq!(model.session.hosts_sync, HostsSync::Idle);
    for (report, code) in [
        (
            report(Some("setup_cancelled"), None, None),
            "setup_cancelled",
        ),
        (
            report(None, Some("hosts_too_many_names"), None),
            "hosts_too_many_names",
        ),
        (
            report(None, None, Some("hosts_block_broken")),
            "hosts_block_broken",
        ),
    ] {
        model.update(Update::HostsSynced(report));
        assert_eq!(model.session.hosts_sync, HostsSync::Failed(code.into()));
    }
    model.update(Update::HostsSyncFailed("setup_running".into()));
    assert_eq!(
        model.session.hosts_sync,
        HostsSync::Failed("setup_running".into())
    );
    // A new session starts clean.
    model.update(Update::Connected(health()));
    assert_eq!(model.session.hosts, HostsStatus::default());
    assert_eq!(model.session.hosts_sync, HostsSync::Idle);
}

#[test]
fn setup_runs_once_and_shows_its_report() {
    let mut model = Model::default();
    assert!(!model.start_setup(), "no daemon");
    let mut model = connected();
    assert!(model.start_setup());
    assert!(!model.start_setup(), "one at a time");
    // The event and the answer carry the same report.
    apply(&mut model, Event::SetupFinished(report("ca_not_ready")));
    model.update(Update::SetupDone(report("ca_not_ready")));
    assert_eq!(model.session.setup, Setup::Finished(report("ca_not_ready")));
    assert!(model.start_setup(), "again");
    model.update(Update::SetupFailed("setup_not_supported".into()));
    assert_eq!(
        model.session.setup,
        Setup::Failed("setup_not_supported".into())
    );
}

#[test]
fn a_setup_from_a_terminal_shows_and_a_lost_one_ends() {
    let mut model = connected();
    apply(&mut model, Event::SetupFinished(report("ca_unreadable")));
    assert_eq!(
        model.session.setup,
        Setup::Finished(report("ca_unreadable"))
    );
    assert!(model.start_setup());
    model.update(Update::Reconnecting);
    assert_eq!(model.session.setup, Setup::Idle);
}

// -- Settings --

#[test]
fn start_at_login_follows_each_state() {
    let mut model = connected();
    assert_eq!(model.session.autostart, Autostart::Unknown);
    assert_eq!(model.toggle_autostart(), None);
    for (state, want, shown) in [
        (Autostart::Off, Some(true), Autostart::On),
        (Autostart::On, Some(false), Autostart::Off),
        // A login file for another `marsa-tray`: written again.
        (Autostart::Stale, Some(true), Autostart::On),
        (Autostart::Unsupported, None, Autostart::Unsupported),
    ] {
        model.update(Update::Autostart(state));
        assert_eq!(model.toggle_autostart(), want, "{state:?}");
        assert_eq!(model.session.autostart, shown, "{state:?}");
    }
    model.update(Update::AutostartFailed("autostart_write".into()));
    assert_eq!(model.session.autostart, Autostart::Unknown);
    assert_eq!(
        model.session.notices.code(NoticeSource::Autostart),
        Some("autostart_write")
    );
    // The bridge reads it again; the notice stays until the next click.
    model.update(Update::Autostart(Autostart::Off));
    assert_eq!(
        model.session.notices.code(NoticeSource::Autostart),
        Some("autostart_write")
    );
    model.toggle_autostart();
    assert!(
        model
            .session
            .notices
            .code(NoticeSource::Autostart)
            .is_none()
    );

    model.update(Update::Reconnecting);
    assert_eq!(model.session.autostart, Autostart::Unknown);
    assert_eq!(model.toggle_autostart(), None);
}

#[test]
fn the_data_folder_and_the_typing_stay_across_sessions() {
    let mut model = Model::new(absolute("/data"));
    model.update(Update::Connected(health()));
    model.set_install_text(Runtime::Php, "8.3".into());
    model.update(Update::Reconnecting);
    model.update(Update::Connected(health()));
    assert_eq!(model.data_dir, absolute("/data"));
    assert_eq!(model.install_text(Runtime::Php), "8.3");
    assert_eq!(model.install_text(Runtime::Node), "");
    assert!(!APP_VERSION.is_empty());
}

// -- Link --

#[test]
fn a_picked_folder_gets_a_name_made_from_it() {
    let mut model = connected();
    model.pick_link_folder(&absolute("/work/My_App.v2"));
    let form = model.link.clone().unwrap();
    assert_eq!(form.folder, absolute("/work/My_App.v2"));
    assert_eq!(form.name, "my-app-v2");
    // No valid name can be made: the user types one.
    model.pick_link_folder(&absolute("/work/متجر"));
    assert_eq!(model.link.as_ref().unwrap().name, "");
}

#[test]
fn a_bad_site_name_is_refused_without_a_request() {
    let mut model = connected();
    model.pick_link_folder(&absolute("/work/متجر"));
    assert_eq!(model.link_request(), None);
    let invalid = SitesNotice::Failed("site_name_invalid".into());
    assert_eq!(
        model.session.notices.sites().cloned(),
        Some(invalid.clone())
    );
    for name in ["Shop", "-shop", "shop-", "my shop", &"a".repeat(64)] {
        model.link.as_mut().unwrap().name = name.to_owned();
        assert_eq!(model.link_request(), None, "{name}");
        assert_eq!(
            model.session.notices.sites().cloned(),
            Some(invalid.clone())
        );
    }
    model.link.as_mut().unwrap().name = " shop ".into();
    let want = ("shop".to_owned(), absolute("/work/متجر"));
    assert_eq!(model.link_request(), Some(want));
    assert!(model.session.notices.sites().is_none());
}

#[test]
fn linking_over_another_folder_is_named_and_a_link_closes_the_form() {
    let mut model = connected();
    model.update(Update::Sites(site_list(&["shop"])));
    model.pick_link_folder(&absolute("/work/shop"));
    let replaced = model.link_replaces().map(|site| site.root.clone());
    assert_eq!(replaced, Some(PathBuf::from("/code/shop")));
    // The same folder again keeps the site: nothing is replaced.
    model.link.as_mut().unwrap().folder = PathBuf::from("/code/shop");
    assert!(model.link_replaces().is_none());

    model.pick_link_folder(&absolute("/work/api"));
    let (name, folder) = model.link_request().unwrap();
    model.update(Update::Linked {
        name: name.clone(),
        folder: folder.clone(),
    });
    assert!(model.link.is_none());
    assert_eq!(
        model.session.notices.sites().cloned(),
        Some(SitesNotice::Linked { name, folder })
    );
}

#[test]
fn a_link_answer_keeps_a_newer_form() {
    let mut model = connected();
    model.pick_link_folder(&absolute("/work/web"));
    let name = "api".to_owned();
    let folder = absolute("/work/api");
    model.update(Update::Linked { name, folder });
    assert_eq!(model.link.as_ref().unwrap().name, "web");
}

// -- Removals: uninstall, unlink, and forget --

fn uninstall(runtime: Runtime, version: &str) -> Removal {
    Removal::Uninstall {
        runtime,
        version: v(version),
    }
}

fn unlink(site: &str) -> Removal {
    Removal::Unlink { site: site.into() }
}

fn forget(folder: &str) -> Removal {
    Removal::Forget {
        folder: PathBuf::from(folder),
    }
}

fn not_removed(removal: Removal, code: &str) -> Update {
    Update::NotRemoved {
        removal,
        code: code.into(),
        in_use: None,
    }
}

/// Asks for `removal`, confirms it, and returns what goes to the bridge.
fn send(model: &mut Model, removal: Removal) -> Removal {
    assert!(model.ask_removal(removal), "the confirmation opens");
    model.confirm_removal().expect("sent")
}

/// `/code` and `/empty` parked, `shop` and `blog` from `/code`, and `api`
/// linked from `/work/api`.
fn with_parked() -> Model {
    let mut model = connected();
    let api = Site {
        root: PathBuf::from("/work/api"),
        ..site("api", true)
    };
    model.update(Update::Sites(SiteList {
        parked: vec![PathBuf::from("/code"), PathBuf::from("/empty")],
        sites: vec![site("shop", true), site("blog", true), api],
        skipped: Vec::new(),
    }));
    model
}

#[test]
fn a_removal_waits_for_its_confirmation() {
    let mut model = Model::default();
    assert!(!model.ask_removal(unlink("api")), "no daemon");
    let mut model = with_parked();
    assert!(model.ask_removal(forget("/code")));
    assert_eq!(model.session.confirm, Some(forget("/code")));
    model.cancel_removal();
    assert!(model.session.confirm.is_none());
    assert_eq!(model.confirm_removal(), None, "nothing to confirm");

    // An older notice goes once the user confirms.
    model.update(Update::SitesFailed("site_not_found".into()));
    assert!(model.ask_removal(unlink("api")));
    assert!(model.session.notices.sites().is_some());
    assert_eq!(model.confirm_removal(), Some(unlink("api")));
    assert!(model.session.confirm.is_none() && model.session.notices.sites().is_none());
    assert_eq!(model.removing(), Some(&unlink("api")));
    // One at a time.
    assert!(!model.can_remove());
    assert!(!model.ask_removal(forget("/code")));

    model.update(Update::Removed(unlink("api")));
    let unlinked = SitesNotice::Unlinked("api".into());
    assert_eq!(model.session.notices.sites().cloned(), Some(unlinked));
    assert!(model.removing().is_none() && model.can_remove());
}

#[test]
fn unlinking_from_the_page_of_the_site_goes_back_to_sites_and_other_pages_stay() {
    let mut model = with_parked();
    model.apply_action(Action::Navigate(Route::Site("api".into())));
    model.apply_action(Action::AskUnlink("api".into()));
    assert_eq!(model.session.confirm, Some(unlink("api")));
    assert!(model.confirm_removal().is_some());
    // Waiting for the answer: the page is still there.
    assert_eq!(model.route, Route::Site("api".into()));
    model.update(Update::Removed(unlink("api")));
    // The line about it shows on Sites.
    assert_eq!(model.route, Route::Sites);
    assert_eq!(
        model.session.notices.sites().cloned(),
        Some(SitesNotice::Unlinked("api".into()))
    );

    // The page of another site, or another screen, is not moved.
    for route in [Route::Site("blog".into()), Route::Settings] {
        let mut model = with_parked();
        model.route = route.clone();
        model.apply_action(Action::AskUnlink("api".into()));
        model.confirm_removal().expect("sent");
        model.update(Update::Removed(unlink("api")));
        assert_eq!(model.route, route);
    }
}

#[test]
fn an_uninstall_and_the_windows_installs_wait_for_each_other() {
    let mut model = with_runtimes(&["8.4.23"], &[]);
    assert!(model.request_install(Runtime::Caddy));
    assert!(!model.can_uninstall());
    assert!(!model.ask_removal(uninstall(Runtime::Php, "8.4.23")));
    let caddy = model.next_install().unwrap();
    model.update(Update::InstallAccepted { job: caddy, id: 1 });
    apply(&mut model, done(1, Runtime::Caddy, "2.11.7"));

    let sent = send(&mut model, uninstall(Runtime::Php, "8.4.23"));
    assert!(model.uninstalling());
    // An install asked for now is sent after the uninstall's answer.
    model.set_install_text(Runtime::Node, "22".into());
    assert!(model.request_install(Runtime::Node));
    assert_eq!(model.next_install(), None);
    model.update(not_removed(sent, "install_busy"));
    assert!(!model.uninstalling());
    assert_eq!(model.next_install(), Some(job(Runtime::Node, Some("22"))));
}

// -- The install dialog, the chosen runtime, and sites per version --

#[test]
fn the_install_dialog_keeps_a_field_for_each_runtime_and_closes_when_the_job_is_queued() {
    let mut model = with_runtimes(&["8.3.27"], &["2.11.7"]);
    assert!(model.open_install(Runtime::Php));
    assert_eq!(model.session.install_dialog, Some(Runtime::Php));
    // Typing is the model's, per runtime; an empty field is no entry.
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Php,
        text: " 8.4 ".into(),
    });
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Node,
        text: "22".into(),
    });
    assert_eq!(model.install_text(Runtime::Php), " 8.4 ");
    assert_eq!(model.install_text(Runtime::Node), "22");
    assert_eq!(model.install_text(Runtime::Caddy), "");
    // Install: the job waits in the queue, the dialog and the text go.
    assert!(
        model
            .apply_action(Action::SubmitInstall(Runtime::Php))
            .is_none()
    );
    assert_eq!(model.session.install_dialog, None);
    assert_eq!(model.install_text(Runtime::Php), "");
    assert_eq!(model.install_text(Runtime::Node), "22", "the others stay");
    assert_eq!(model.next_install(), Some(job(Runtime::Php, Some("8.4"))));
}

#[test]
fn a_wrong_version_keeps_the_dialog_open_and_clears_when_the_text_changes() {
    let mut model = with_runtimes(&[], &[]);
    model.apply_action(Action::AskInstall(Runtime::Node));
    model.set_install_text(Runtime::Node, "lts/*".into());
    model.apply_action(Action::SubmitInstall(Runtime::Node));
    assert_eq!(model.session.install_dialog, Some(Runtime::Node));
    assert_eq!(
        model.session.notices.code(NoticeSource::Runtimes),
        Some("unsupported_alias")
    );
    assert_eq!(model.next_install(), None);
    // The error was about the old text.
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Node,
        text: "22".into(),
    });
    assert_eq!(model.session.notices.code(NoticeSource::Runtimes), None);
    // Cancel closes it and keeps what was typed.
    model.apply_action(Action::CancelInstall);
    assert_eq!(model.session.install_dialog, None);
    assert_eq!(model.install_text(Runtime::Node), "22");
}

#[test]
fn the_install_dialog_does_not_open_while_busy_or_offline_and_closes_with_the_session() {
    let mut model = with_runtimes(&["8.3.27"], &[]);
    assert!(model.request_install(Runtime::Caddy));
    assert!(!model.open_install(Runtime::Php), "an install is queued");
    let mut idle = with_runtimes(&["8.3.27"], &[]);
    let sent = send(&mut idle, uninstall(Runtime::Php, "8.3.27"));
    assert!(!idle.open_install(Runtime::Php), "an uninstall waits");
    idle.update(Update::Removed(sent));
    assert!(idle.open_install(Runtime::Php));
    idle.update(Update::Reconnecting);
    assert_eq!(idle.session.install_dialog, None);
    assert!(!Model::default().open_install(Runtime::Php), "no daemon");
}

#[test]
fn the_runtime_the_screen_showed_last_is_kept_for_the_sidebar_and_across_sessions() {
    let mut model = connected();
    assert_eq!(model.chosen_runtime(), MAIN_RUNTIME);
    model.apply_action(Action::Navigate(Route::Runtimes(Runtime::Node)));
    assert_eq!(model.chosen_runtime(), Runtime::Node);
    model.apply_action(Action::Navigate(Route::Home));
    assert_eq!(model.chosen_runtime(), Runtime::Node, "the last one");
    model.update(Update::Reconnecting);
    model.update(Update::Connected(health()));
    assert_eq!(model.chosen_runtime(), Runtime::Node);
}

#[test]
fn sites_are_counted_per_version_by_what_runs_and_by_what_they_set() {
    let pinned = php_site("pinned", Some("8.3.27"), Some(Source::ProjectSetting));
    let by_default = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
    let mut model = with_php(vec![by_default, pinned]);
    let (old, new) = (v("8.3.27"), v("8.4.23"));
    assert_eq!(model.sites_running(Runtime::Php, &old), ["pinned"]);
    assert_eq!(model.sites_running(Runtime::Php, &new), ["shop"]);
    // Only a site's own setting keeps a version from being uninstalled.
    assert_eq!(model.sites_set_to(Runtime::Php, &old), ["pinned"]);
    assert!(model.sites_set_to(Runtime::Php, &new).is_empty());
    // The version the user just picked counts at once.
    model.choose_site_php("shop", Choice::Version(old.clone()));
    assert_eq!(model.sites_running(Runtime::Php, &old), ["pinned", "shop"]);
    assert_eq!(model.sites_set_to(Runtime::Php, &old), ["pinned", "shop"]);
    // No other runtime has a version per site.
    assert!(model.sites_running(Runtime::Node, &v("22.11.0")).is_empty());
}

#[test]
fn an_uninstall_drops_the_version_and_the_list_is_read_again() {
    let mut model = with_runtimes(&["8.3.27", "8.4.23"], &["2.11.7"]);
    let sent = send(&mut model, uninstall(Runtime::Php, "8.3.27"));
    model.update(Update::Removed(sent));
    assert_eq!(model.php_versions(), [v("8.4.23")]);
    let uninstalled = UninstallNotice::Done {
        runtime: Runtime::Php,
        version: v("8.3.27"),
    };
    assert_eq!(
        model.session.notices.uninstall().cloned(),
        Some(uninstalled)
    );

    // The last PHP: the list read again after it opens the first-run
    // panel, since no site can open without PHP.
    let sent = send(&mut model, uninstall(Runtime::Php, "8.4.23"));
    model.update(Update::Removed(sent));
    assert!(model.php_versions().is_empty());
    assert!(
        model.session.first_run.is_none(),
        "not before the list is read"
    );
    model.update(Update::Runtimes(list(&[], &["2.11.7"])));
    assert_eq!(model.session.first_run, Some(FirstRun::Offer));
}

#[test]
fn the_last_version_takes_its_default_with_it() {
    let mut model = connected();
    let php: &[&str] = &["8.4.23", "8.3.27"];
    let node = (&["22.11.0"][..], Some("22"));
    model.update(Update::Runtimes(runtime_list((php, Some("8.4")), node)));
    let default = |model: &Model, runtime| model.row(runtime).unwrap().default.clone();
    for (runtime, version, want) in [
        (Runtime::Node, "22.11.0", None),
        (Runtime::Php, "8.3.27", Some("8.4")),
        (Runtime::Php, "8.4.23", None),
    ] {
        let sent = send(&mut model, uninstall(runtime, version));
        model.update(Update::Removed(sent));
        assert_eq!(default(&model, runtime).as_deref(), want, "{version}");
    }
}

#[test]
fn a_version_in_use_says_what_uses_it() {
    let mut model = with_php(Vec::new());
    let sent = send(&mut model, uninstall(Runtime::Php, "8.4.23"));
    let in_use = VersionInUse {
        version: "8.4.23".into(),
        default: Some("8.4".into()),
        sites: vec!["shop".into()],
        ..VersionInUse::default()
    };
    model.update(Update::NotRemoved {
        removal: sent,
        code: "version_in_use".into(),
        in_use: Some(in_use.clone()),
    });
    let notice = UninstallNotice::InUse {
        runtime: Runtime::Php,
        in_use,
    };
    assert_eq!(model.session.notices.uninstall().cloned(), Some(notice));
    assert_eq!(model.php_versions().len(), 2, "nothing changed");

    // The next confirmation clears it; another failure keeps its code.
    let sent = send(&mut model, uninstall(Runtime::Php, "8.3.27"));
    assert!(model.session.notices.uninstall().is_none());
    model.update(not_removed(sent, "uninstall_io"));
    let failed = UninstallNotice::Failed {
        runtime: Runtime::Php,
        version: v("8.3.27"),
        code: "uninstall_io".into(),
    };
    assert_eq!(model.session.notices.uninstall().cloned(), Some(failed));
}

#[test]
fn only_a_linked_site_has_unlink() {
    let model = with_parked();
    let folder = |name| model.parked_folder(site_named(&model, name));
    assert_eq!(folder("shop"), Some(Path::new("/code")));
    assert_eq!(folder("blog"), Some(Path::new("/code")));
    assert_eq!(folder("api"), None);
    assert_eq!(model.sites_in(Path::new("/code")), 2);
    assert_eq!(model.sites_in(Path::new("/empty")), 0);
    assert_eq!(model.site_root("api"), Some(Path::new("/work/api")));
    assert_eq!(model.site_root("gone"), None);

    // Linked by another name to a folder of a parked folder: linked.
    let alias = Site {
        root: PathBuf::from("/code/shop"),
        ..site("alias", true)
    };
    assert_eq!(model.parked_folder(&alias), None);
}

#[test]
fn a_parked_site_that_is_refused_offers_its_folder() {
    let mut model = with_parked();
    // The window's guess may be old: the daemon has the last word.
    let sent = send(&mut model, unlink("shop"));
    model.update(not_removed(sent, "site_is_parked"));
    let refused = SitesNotice::SiteIsParked {
        site: "shop".into(),
        folder: Some(PathBuf::from("/code")),
    };
    assert_eq!(model.session.notices.sites().cloned(), Some(refused));

    let sent = send(&mut model, forget("/code"));
    model.update(Update::Removed(sent));
    let forgot = SitesNotice::Forgot(PathBuf::from("/code"));
    assert_eq!(model.session.notices.sites().cloned(), Some(forgot));

    // A site the list does not have: no folder to offer.
    let sent = send(&mut model, unlink("gone"));
    model.update(not_removed(sent, "site_is_parked"));
    let refused = SitesNotice::SiteIsParked {
        site: "gone".into(),
        folder: None,
    };
    assert_eq!(model.session.notices.sites().cloned(), Some(refused));
}

#[test]
fn a_refused_unlink_or_forget_shows_its_code_and_keeps_the_list() {
    for (removal, code) in [
        (unlink("api"), "site_not_found"),
        (unlink(".."), "invalid_id"),
        (forget("/elsewhere"), "folder_not_parked"),
        (forget("code"), "site_path_relative"),
        (forget("/code"), "site_path_not_utf8"),
    ] {
        let mut model = with_parked();
        let sent = send(&mut model, removal);
        model.update(not_removed(sent, code));
        let failed = SitesNotice::Failed(code.into());
        assert_eq!(
            model.session.notices.sites().cloned(),
            Some(failed),
            "{code}"
        );
        assert!(model.can_remove(), "{code}");
        assert_eq!(names(&model), ["api", "blog", "shop"], "{code}");
    }
}

#[test]
fn a_lost_session_closes_the_confirmation_and_drops_its_answer() {
    let mut model = with_parked();
    assert!(model.ask_removal(forget("/code")));
    model.update(Update::Reconnecting);
    assert!(model.session.confirm.is_none());
    assert_eq!(model.confirm_removal(), None);

    model.update(Update::Connected(health()));
    let sent = send(&mut model, unlink("api"));
    model.update(Update::Reconnecting);
    assert!(model.removing().is_none());
    model.update(Update::Connected(health()));
    model.update(Update::Removed(sent));
    assert!(model.session.notices.sites().is_none());
}

// -- Xdebug --

fn xdebug(enabled: bool) -> XdebugState {
    XdebugState {
        enabled,
        loaded: if enabled {
            vec!["8.4.23".into()]
        } else {
            Vec::new()
        },
        missing: vec!["8.3.27".into()],
    }
}

#[test]
fn the_xdebug_box_follows_each_state() {
    let mut model = connected();
    assert_eq!(model.xdebug_on(), None);
    assert_eq!(model.toggle_xdebug(), None, "not read yet");
    model.update(Update::Xdebug(xdebug(false)));
    assert_eq!(model.xdebug_on(), Some(false));
    assert_eq!(model.toggle_xdebug(), Some(true));
    // The box shows the new state until the answer.
    assert_eq!(model.xdebug_on(), Some(true));
    assert_eq!(model.xdebug_pending(), Some(true));
    assert_eq!(model.toggle_xdebug(), None, "one change at a time");
    // A read meanwhile does not end the wait.
    model.update(Update::Xdebug(xdebug(false)));
    assert_eq!(model.xdebug_on(), Some(true));
    model.update(Update::XdebugAnswered(Ok(xdebug(true))));
    assert_eq!(model.xdebug_pending(), None);
    assert_eq!(model.session.xdebug, Some(xdebug(true)));

    assert_eq!(model.toggle_xdebug(), Some(false));
    let code = "xdebug_interrupted".to_owned();
    model.update(Update::XdebugAnswered(Err(code)));
    assert_eq!(
        model.session.notices.code(NoticeSource::Xdebug),
        Some("xdebug_interrupted")
    );
    // The bridge reads it again; until then the last state shows.
    assert_eq!(model.xdebug_on(), Some(true));
    assert_eq!(model.toggle_xdebug(), Some(false));
    assert!(
        model.session.notices.code(NoticeSource::Xdebug).is_none(),
        "the next click clears it"
    );

    model.update(Update::Reconnecting);
    assert_eq!(model.xdebug_on(), None);
    assert_eq!(model.toggle_xdebug(), None);
    model.update(Update::Connected(health()));
    model.update(Update::XdebugFailed("config_invalid".into()));
    assert_eq!(
        model.session.notices.code(NoticeSource::Xdebug),
        Some("config_invalid")
    );
    assert_eq!(model.toggle_xdebug(), None);
}
