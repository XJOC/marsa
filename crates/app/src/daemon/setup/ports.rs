//! Tests of the port check inside `marsa setup`. The check itself is
//! `marsa_core::platform::ports`.

#[cfg(test)]
mod tests {
    use super::super::osascript::OSASCRIPT;
    use marsa_core::platform::runner::MAX_OUTPUT;

    use super::super::test_support::*;
    use super::super::user_trust::SECURITY;
    use super::super::*;
    use marsa_core::platform::ports::{NETSTAT, PS};
    use marsa_core::platform::runner::RunOutput;

    use marsa_core::types::PortHolder;

    use std::io;

    use std::time::Duration;

    #[test]
    fn herd_on_80_and_443_means_no_pf_install() {
        let dir = Dir::new(true);
        let answer = all_done(&[OpName::ResolverInstall]);
        let fake = Fake::scripted(NETSTAT_HERD, move || ok(&answer));
        let report = dir.run(&fake);

        let nginx = |port| PortHolder {
            port,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: Some("dev".into()),
            // The worker's title gives no hint; netstat's name says Herd.
            command: Some("nginx: worker process".into()),
            app: Some("Herd".into()),
        };
        assert_eq!(report.port_holders, [nginx(80), nginx(443)]);
        assert_eq!(
            report.skipped,
            [skip(OpName::PfInstall, "redirect_port_held")]
        );
        assert_eq!(fake.request().ops, [resolver()]);
        assert_eq!(
            report.results,
            [done(OpName::ResolverInstall), done(OpName::TrustInstall)]
        );
        assert_eq!(report.error, None);

        // `ps` once for the one PID that holds both ports.
        let calls = fake.calls();
        assert_eq!(
            fake.programs(),
            [NETSTAT, PS, OSASCRIPT, SECURITY, SECURITY]
        );
        assert_eq!(calls[0].args, ["-anv", "-p", "tcp"]);
        assert_eq!(calls[1].args, ["-p", "15464", "-o", "user=,comm="]);
        assert_eq!(calls[0].limit, Duration::from_secs(10));
        assert_eq!(calls[1].limit, Duration::from_secs(10));
    }

    #[test]
    fn netstat_output_cut_at_the_limit_means_no_pf_install() {
        // Real lines with nobody on 80 or 443, repeated to just under the
        // limit: read as usual.
        let free = netstat_free();
        let mut text = String::new();
        while text.len() + free.len() + 1 < MAX_OUTPUT as usize {
            text.push_str(free);
            text.push('\n');
        }
        let run = |text: String, done: &[OpName]| {
            let dir = Dir::new(true);
            let answer = all_done(done);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => ok(&text),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            (report, fake.request().ops, fake.programs())
        };
        let all = [OpName::ResolverInstall, OpName::PfInstall];
        let (report, ops, _) = run(text.clone(), &all);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.error, None);

        // Filled up to the limit, as `RealRunner` keeps a longer output: a
        // holder could sit in the part that was not read.
        text.push_str(&" ".repeat(MAX_OUTPUT as usize - text.len()));
        let (report, ops, programs) = run(text, &all[..1]);
        assert_eq!(
            report.skipped,
            [skip(OpName::PfInstall, "ports_not_checked")]
        );
        assert_eq!(ops, [resolver()]);
        assert_eq!(programs, [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        assert_eq!(report.error, None);
    }

    #[test]
    fn netstat_that_fails_or_is_not_understood_means_no_pf_install() {
        let outputs: [fn() -> io::Result<RunOutput>; 3] = [
            || Err(io::Error::from(io::ErrorKind::NotFound)),
            || script_error("netstat: sysctl: Operation not permitted"),
            || ok("Active Internet connections\nsomething new\n"),
        ];
        for netstat in outputs {
            let dir = Dir::new(true);
            let answer = all_done(&[OpName::ResolverInstall]);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => netstat(),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            assert_eq!(
                report.skipped,
                [skip(OpName::PfInstall, "ports_not_checked")]
            );
            assert_eq!(fake.request().ops, [resolver()]);
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
            assert_eq!(report.error, None);
        }
    }
}
