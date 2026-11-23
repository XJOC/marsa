//! The mail Mailpit caught, as the daemon gave it: the list, the mail the
//! user opened, and its text. No event carries mail, so the bridge reads it
//! when the Mail screen opens, after "Delete all…", when Mailpit starts or
//! stops, and every 10 s while it runs; it sends a list only when it differs
//! from the last one it sent, so a quiet Mailpit costs no frame.

use marsa_core::codes::MAIL_NOT_RUNNING;
use marsa_core::databases::ServerKind;
use marsa_core::databases::mailpit::MAILPIT_UI_PORT;
use marsa_core::types::{MailList, MailText};

use super::*;

/// Mailpit's service: "Start Mailpit" starts it, and the bridge reads the
/// mail while it runs.
pub const MAILPIT: ServiceId = ServiceId::Database(ServerKind::Mailpit);

/// What the window knows of the mail. Part of the [`Session`]: a new
/// connection reads it again.
#[derive(Debug, Default)]
pub struct MailState {
    /// The newest mails, newest first, as last read. `None` until read, and
    /// while Mailpit does not run.
    pub list: Option<MailList>,
    /// The id of the mail the user opened.
    pub selected: Option<String>,
    /// The text of the opened mail, once read.
    pub text: Option<MailText>,
    /// Why the last read of the list failed: a client error code.
    /// `mail_not_running` is a state of the screen; any other is a notice
    /// too.
    pub error: Option<String>,
}

impl MailState {
    /// The last read found Mailpit not running.
    pub fn not_running(&self) -> bool {
        self.error.as_deref() == Some(MAIL_NOT_RUNNING)
    }

    /// The mail not opened yet, as the last list counts it.
    pub fn unread(&self) -> u64 {
        self.list.as_ref().map_or(0, |list| list.unread)
    }

    /// Opening a mail marks it read in Mailpit: the list says so at once,
    /// and a list read before the opening does not bring "New" back.
    fn mark_read(&mut self, id: &str) {
        if let Some(list) = self.list.as_mut()
            && let Some(mail) = list.messages.iter_mut().find(|mail| mail.id == id)
            && !mail.read
        {
            mail.read = true;
            list.unread = list.unread.saturating_sub(1);
        }
    }

    /// Mailpit does not run: nothing of the last list holds.
    fn stopped(&mut self) {
        *self = Self {
            error: Some(MAIL_NOT_RUNNING.to_owned()),
            ..Self::default()
        };
    }
}

/// Mailpit's own page, with every mail, its HTML, and its attachments.
pub fn mailpit_url() -> String {
    format!("http://127.0.0.1:{MAILPIT_UI_PORT}/")
}

impl Model {
    /// Mailpit is installed: only then the sidebar has a Mail item (rules.md
    /// 8).
    pub fn has_mailpit(&self) -> bool {
        !self.installed(Runtime::Mailpit).is_empty()
    }

    /// The state of Mailpit's service, if the daemon lists it.
    pub fn mailpit_state(&self) -> Option<&ServiceState> {
        self.session
            .services
            .iter()
            .find(|service| service.id == MAILPIT)
            .map(|service| &service.state)
    }

    /// A row of the list was chosen: read its text, which marks it read.
    /// Choosing the mail whose text is shown asks for nothing.
    pub(super) fn choose_mail(&mut self, id: String) -> Option<Command> {
        if !self.is_connected() {
            return None;
        }
        let mail = &mut self.session.mail;
        let same = mail.selected.as_deref() == Some(id.as_str());
        if same && mail.text.as_ref().is_some_and(|text| text.id == id) {
            return None;
        }
        if !same {
            mail.text = None;
        }
        mail.mark_read(&id);
        mail.selected = Some(id.clone());
        Some(Command::ReadMailText(id))
    }

    pub(super) fn set_mail(&mut self, list: MailList) {
        let mail = &mut self.session.mail;
        mail.list = Some(list);
        mail.error = None;
        // The mail opened may be gone: deleted in Mailpit's page, or pushed
        // out of the newest by new mail.
        let listed = mail.selected.as_deref().is_some_and(|id| {
            mail.list
                .as_ref()
                .is_some_and(|list| list.messages.iter().any(|m| m.id == id))
        });
        match mail.selected.clone() {
            Some(id) if listed => mail.mark_read(&id),
            _ => {
                mail.selected = None;
                mail.text = None;
            }
        }
    }

    /// The text of a mail: kept if it is still the chosen one.
    pub(super) fn set_mail_text(&mut self, text: MailText) {
        let mail = &mut self.session.mail;
        if mail.selected.as_deref() == Some(text.id.as_str()) {
            mail.mark_read(&text.id);
            mail.text = Some(text);
        }
    }

    /// A read of the list failed. The screen keeps the code: with no list
    /// to show, it says why.
    pub(super) fn mail_failed(&mut self, code: String) {
        if code != MAIL_NOT_RUNNING {
            self.session.mail.error = Some(code.clone());
        }
        self.mail_call_failed(code);
    }

    /// A mail call failed. Mailpit not running is what the screen shows;
    /// any other failure is a notice.
    pub(super) fn mail_call_failed(&mut self, code: String) {
        if code == MAIL_NOT_RUNNING {
            self.session.mail.stopped();
        } else {
            self.session.notices.set_code(NoticeSource::Call, code);
        }
    }

    /// The page of the chosen mail in Mailpit, for "Open in browser". The
    /// address the daemon gave, if it is Mailpit's; else the one core makes
    /// from the id.
    pub(super) fn mail_page(&self) -> Option<String> {
        let mail = &self.session.mail;
        let id = mail.selected.as_deref()?;
        let given = mail
            .text
            .as_ref()
            .filter(|text| text.id == id && text.web_url.starts_with(&mailpit_url()));
        match given {
            Some(text) => Some(text.web_url.clone()),
            None => marsa_core::mail::is_mail_id(id).then(|| marsa_core::mail::web_url(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use marsa_core::types::{MailSummary, ServiceAction};

    use super::*;
    use crate::fixtures::connected;

    fn summary(id: &str, read: bool) -> MailSummary {
        MailSummary {
            id: id.into(),
            from: "Laravel <hello@example.test>".into(),
            to: vec!["omar@example.test".into()],
            subject: format!("Mail {id}"),
            received: 1_800_000_000_000,
            size: 1024,
            read,
            snippet: String::new(),
            attachments: 0,
        }
    }

    /// Three mails, the first two not opened yet.
    fn list() -> MailList {
        MailList {
            total: 3,
            unread: 2,
            messages: vec![
                summary("a1", false),
                summary("b2", false),
                summary("c3", true),
            ],
        }
    }

    fn text(id: &str) -> MailText {
        MailText {
            id: id.into(),
            from: "Laravel <hello@example.test>".into(),
            to: vec!["omar@example.test".into()],
            subject: format!("Mail {id}"),
            date: 1_800_000_000_000,
            text: "Hello!".into(),
            web_url: marsa_core::mail::web_url(id),
        }
    }

    fn with_list() -> Model {
        let mut model = connected();
        model.update(Update::Mail(list()));
        model
    }

    fn command(effect: Effect) -> Option<Command> {
        effect.command()
    }

    #[test]
    fn choosing_a_mail_reads_its_text_and_marks_it_read_at_once() {
        let mut model = with_list();
        let sent = command(model.apply_action(Action::ChooseMail("a1".into())));
        assert!(
            matches!(&sent, Some(Command::ReadMailText(id)) if id == "a1"),
            "{sent:?}"
        );
        let mail = &model.session.mail;
        assert_eq!(mail.selected.as_deref(), Some("a1"));
        assert_eq!(mail.unread(), 1);
        let first = &mail.list.as_ref().expect("a list").messages[0];
        assert!(first.read);
        // The text comes; a text of a mail chosen before is dropped.
        model.update(Update::MailText(text("b2")));
        assert!(model.session.mail.text.is_none());
        model.update(Update::MailText(text("a1")));
        assert_eq!(
            model.session.mail.text.as_ref().map(|t| t.id.as_str()),
            Some("a1")
        );
        // The same mail again asks for nothing; another one drops the text.
        assert!(
            model
                .apply_action(Action::ChooseMail("a1".into()))
                .is_none()
        );
        let sent = command(model.apply_action(Action::ChooseMail("c3".into())));
        assert!(matches!(&sent, Some(Command::ReadMailText(id)) if id == "c3"));
        assert!(model.session.mail.text.is_none());
        // An opened mail is not counted twice.
        assert_eq!(model.session.mail.unread(), 1);
    }

    #[test]
    fn a_list_read_before_the_opening_keeps_the_opened_mail_read() {
        let mut model = with_list();
        model.apply_action(Action::ChooseMail("a1".into()));
        model.update(Update::MailText(text("a1")));
        // The bridge read this one before Mailpit marked the mail read.
        model.update(Update::Mail(list()));
        let mail = &model.session.mail;
        assert_eq!(mail.unread(), 1);
        assert_eq!(mail.selected.as_deref(), Some("a1"));
        assert!(mail.text.is_some(), "the same list keeps the text shown");
        // The opened mail was deleted in Mailpit's page: nothing is chosen.
        let mut rest = list();
        rest.messages.remove(0);
        rest.total = 2;
        model.update(Update::Mail(rest));
        assert!(model.session.mail.selected.is_none());
        assert!(model.session.mail.text.is_none());
    }

    #[test]
    fn the_same_list_again_changes_nothing() {
        let mut model = with_list();
        model.apply_action(Action::ChooseMail("b2".into()));
        model.update(Update::MailText(text("b2")));
        let before = format!("{:?}", model.session.mail);
        let mut read = list();
        // Mailpit's answer after the opening: b2 read.
        read.messages[1].read = true;
        read.unread = 1;
        model.update(Update::Mail(read));
        assert_eq!(format!("{:?}", model.session.mail), before);
        assert!(model.session.notices.code(NoticeSource::Call).is_none());
    }

    #[test]
    fn mailpit_not_running_is_a_state_and_any_other_failure_a_notice() {
        let mut model = with_list();
        model.apply_action(Action::ChooseMail("a1".into()));
        model.update(Update::MailFailed(MAIL_NOT_RUNNING.into()));
        let mail = &model.session.mail;
        assert!(mail.not_running());
        assert!(mail.list.is_none() && mail.selected.is_none());
        assert!(model.session.notices.code(NoticeSource::Call).is_none());
        // It runs again: the list replaces the state.
        model.update(Update::Mail(list()));
        assert!(!model.session.mail.not_running());
        model.update(Update::MailFailed("mail_unavailable".into()));
        assert_eq!(
            model.session.notices.code(NoticeSource::Call),
            Some("mail_unavailable")
        );
        // The list shown stays.
        assert!(model.session.mail.list.is_some());
    }

    #[test]
    fn delete_all_asks_first_and_sends_only_once_confirmed() {
        let mut model = with_list();
        model.apply_action(Action::ChooseMail("a1".into()));
        assert!(model.apply_action(Action::AskDeleteMail).is_none());
        assert_eq!(model.session.confirm, Some(Removal::Mail));
        // Cancel sends nothing.
        model.apply_action(Action::CancelRemoval);
        assert!(model.session.confirm.is_none());
        model.apply_action(Action::AskDeleteMail);
        let sent = command(model.apply_action(Action::ConfirmRemoval));
        assert!(matches!(sent, Some(Command::ClearMail)), "{sent:?}");
        assert_eq!(model.removing(), Some(&Removal::Mail));
        model.update(Update::Removed(Removal::Mail));
        assert!(model.removing().is_none());
        assert!(model.session.mail.selected.is_none());
        model.update(Update::Mail(MailList::default()));
        assert_eq!(model.session.mail.unread(), 0);
    }

    #[test]
    fn a_delete_all_that_fails_says_why() {
        let mut model = with_list();
        model.apply_action(Action::AskDeleteMail);
        model.apply_action(Action::ConfirmRemoval);
        model.update(Update::NotRemoved {
            removal: Removal::Mail,
            code: "mail_unavailable".into(),
            in_use: None,
        });
        assert!(model.removing().is_none());
        assert_eq!(
            model.session.notices.code(NoticeSource::Call),
            Some("mail_unavailable")
        );
        // Mailpit stopped meanwhile: the screen says so, with no notice.
        let mut model = with_list();
        model.apply_action(Action::AskDeleteMail);
        model.apply_action(Action::ConfirmRemoval);
        model.update(Update::NotRemoved {
            removal: Removal::Mail,
            code: MAIL_NOT_RUNNING.into(),
            in_use: None,
        });
        assert!(model.session.mail.not_running());
        assert!(model.session.notices.code(NoticeSource::Call).is_none());
    }

    #[test]
    fn start_mailpit_is_the_service_start_and_the_pages_open_in_the_browser() {
        let mut model = with_list();
        let start = Action::Service {
            service: MAILPIT,
            action: ServiceAction::Start,
        };
        let sent = command(model.apply_action(start));
        assert!(
            matches!(
                sent,
                Some(Command::Service {
                    service,
                    action: ServiceAction::Start
                }) if service == MAILPIT
            ),
            "{sent:?}"
        );
        assert_eq!(MAILPIT.to_string(), "mailpit");
        let open = model.apply_action(Action::OpenMailpit);
        assert!(matches!(&open, Effect::OpenUrl(url) if url == "http://127.0.0.1:8025/"));
        // Nothing chosen: no page.
        assert!(model.apply_action(Action::OpenMailPage).is_none());
        model.apply_action(Action::ChooseMail("a1".into()));
        let page = model.apply_action(Action::OpenMailPage);
        assert!(
            matches!(&page, Effect::OpenUrl(url) if url == "http://127.0.0.1:8025/view/a1"),
            "{page:?}"
        );
        // An address that is not Mailpit's is not opened: core's is.
        let mut odd = text("a1");
        odd.web_url = "https://example.com/".into();
        model.update(Update::MailText(odd));
        let page = model.apply_action(Action::OpenMailPage);
        assert!(matches!(&page, Effect::OpenUrl(url) if url.ends_with("/view/a1")));
    }

    #[test]
    fn the_mail_goes_with_the_connection_and_the_item_needs_mailpit_installed() {
        let mut model = with_list();
        assert!(!model.has_mailpit());
        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![marsa_core::types::RuntimeVersions {
                runtime: Runtime::Mailpit,
                installed: vec!["1.27.0".into()],
                default: None,
            }],
        }));
        assert!(model.has_mailpit());
        model.update(Update::Reconnecting);
        assert!(model.session.mail.list.is_none());
        assert!(!model.has_mailpit());
    }
}
