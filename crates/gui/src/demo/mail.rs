//! The Mail demos: Mailpit installed, running or stopped, and the mail a
//! Laravel project would send. The ages count back from the time the demo
//! starts, so the picture reads "just now" and "4 min ago".

use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    MailList, MailSummary, MailText, RuntimeVersions, ServiceState, ServiceStatus,
};

use super::{Demo, runtimes};
use crate::model::{Action, MAILPIT, Model, Removal, Update};
use crate::screens::mail::now_ms;

/// Each demo mail: how many minutes ago it came, its sender, its subject,
/// and whether it is new.
const MAIL: [(u64, &str, &str, bool); 5] = [
    (
        0,
        "Laravel <hello@example.test>",
        "Reset Password Notification",
        true,
    ),
    (
        4,
        "Shop <orders@shop.test>",
        "Your order #1042 is on its way",
        true,
    ),
    (
        12,
        "Laravel <hello@example.test>",
        "Verify Email Address",
        true,
    ),
    (
        2 * 60,
        "Blog <noreply@blog.test>",
        "New comment on \u{201c}Hello world\u{201d}",
        false,
    ),
    (
        24 * 60,
        "Laravel <hello@example.test>",
        "Welcome to Laravel",
        false,
    ),
];

/// The plain text of the newest demo mail, as Laravel writes a reset mail.
const RESET_TEXT: &str = "Hello!\r\n\r\nYou are receiving this email because we received a password reset request for your account.\r\n\r\nReset Password: https://shop.test/reset-password/4f1c9a0e\r\n\r\nThis password reset link will expire in 60 minutes.\r\n\r\nIf you did not request a password reset, no further action is required.\r\n\r\nRegards,\r\nLaravel";

/// What the Mail demos show beyond `Ok`: Mailpit installed, its service,
/// and the mail. `php`: the PHP versions `Ok` has.
pub(super) fn fill(model: &mut Model, demo: Demo, php: &[&str]) {
    let mut list = runtimes(php);
    list.runtimes.push(RuntimeVersions {
        runtime: Runtime::Mailpit,
        installed: vec!["1.27.10".into()],
        default: None,
    });
    model.update(Update::Runtimes(list));
    let mailpit = if demo == Demo::MailStopped {
        ServiceState::Stopped
    } else {
        ServiceState::Running
    };
    let mut services = model.session.services.clone();
    services.push(ServiceStatus {
        id: MAILPIT,
        state: mailpit,
        pid: None,
        restarts: 0,
        since_ms: None,
    });
    model.update(Update::Services(services));
    match demo {
        Demo::MailStopped => model.update(Update::MailFailed(
            marsa_core::codes::MAIL_NOT_RUNNING.into(),
        )),
        Demo::MailEmpty => model.update(Update::Mail(MailList::default())),
        _ => caught(model),
    }
    if demo == Demo::ConfirmDeleteMail {
        model.ask_removal(Removal::Mail);
    }
}

/// Five mails of 214, three new; the newest is opened, with its text.
fn caught(model: &mut Model) {
    let now = now_ms();
    let messages: Vec<MailSummary> = MAIL
        .iter()
        .enumerate()
        .map(|(index, (minutes, from, subject, new))| MailSummary {
            id: format!("demo{index}"),
            from: (*from).to_owned(),
            to: vec!["omar@example.test".into()],
            subject: (*subject).to_owned(),
            received: now.saturating_sub(minutes * 60_000),
            size: 2048,
            read: !new,
            snippet: String::new(),
            attachments: 0,
        })
        .collect();
    let unread = messages.iter().filter(|mail| !mail.read).count() as u64;
    model.update(Update::Mail(MailList {
        total: 214,
        unread,
        messages,
    }));
    // The command goes to the demo's bridge, which is not there.
    model.apply_action(Action::ChooseMail("demo0".into()));
    let (_, from, subject, _) = MAIL[0];
    model.update(Update::MailText(MailText {
        id: "demo0".into(),
        from: from.to_owned(),
        to: vec!["omar@example.test".into()],
        subject: subject.to_owned(),
        date: now,
        text: RESET_TEXT.to_owned(),
        web_url: marsa_core::mail::web_url("demo0"),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{fill as fill_demo, parse};
    use crate::screens::mail::{Body, view};
    use crate::shell::dialogs;

    fn demo(name: &str) -> Model {
        let mut model = Model::default();
        fill_demo(&mut model, parse(name).expect(name));
        model
    }

    #[test]
    fn the_mail_demos_show_the_three_states_and_the_confirmation() {
        let model = demo("mail");
        assert!(model.has_mailpit());
        let shown = view(&model, now_ms());
        let Body::List(listed) = &shown.body else {
            panic!("a list: {:?}", shown.body);
        };
        assert_eq!(listed.messages.len(), 5);
        assert_eq!(listed.selected, Some(0));
        assert!(listed.more.is_some());
        let detail = listed.detail.as_ref().expect("the newest mail");
        assert!(
            detail
                .text
                .is_some_and(|text| text.contains("Reset Password"))
        );
        // Opening the newest marked it read: two new are left.
        assert_eq!(model.session.mail.unread(), 2);
        assert_eq!(view(&demo("mail-empty"), now_ms()).body, Body::Empty);
        assert!(matches!(
            view(&demo("mail-stopped"), now_ms()).body,
            Body::NotRunning { start_off: None }
        ));
        let asked = demo("confirm-delete-mail");
        let question = dialogs::confirm_view(&asked).expect("a question");
        assert_eq!(question.title, "Delete every mail?");
        assert_eq!(question.confirm, "Delete all");
    }
}
