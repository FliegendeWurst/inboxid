use std::env;

use inboxid::*;
use itertools::Itertools;
use mailparse::MailHeaderMap;
use rusqlite::params;

fn main() -> Result<()> {
	let db = get_db()?;
	let mut delete_mail = db.prepare("DELETE FROM mail WHERE mailbox = ?")?;
	let mut save_mail = db.prepare("INSERT INTO mail VALUES (?,?,?)")?;
	let mailboxes = env::args().skip(1).collect_vec();
	for mailbox in mailboxes {
		let maildir = get_maildir(&mailbox)?;
		delete_mail.execute(params![&mailbox])?;
		let mut mails = Vec::new();
		for x in maildir.list_cur() {
			mails.push(x?);
		}
		let mut mails = maildir.get_mails(&mut mails)?;
		mails.sort_by_key(|x| x.date);
		for mail in mails {
			let headers = mail.get_headers();
			let message_id = headers.get_all_values("Message-ID").join(" ");
			save_mail.execute(params![&mailbox, mail.id.to_i64(), message_id])?;
		}
	}
	Ok(())
}
