use std::env;

use inboxid::*;
use itertools::Itertools;
use mailparse::MailHeaderMap;
use rusqlite::params;

fn main() -> Result<()> {
	let mut db = get_db()?;
	let tx = db.transaction()?;
	{
	let mut delete_mail = tx.prepare("DELETE FROM mail WHERE mailbox = ?")?;
	let mut save_mail = tx.prepare("INSERT INTO mail VALUES (?,?,?,?)")?;
	let mailboxes = env::args().skip(1).collect_vec();
	for mailbox in mailboxes {
		println!("reading {}..", mailbox);
		let maildir = get_maildir(&mailbox)?;
		delete_mail.execute(params![&mailbox])?;
		let mut mails = Vec::new();
		for x in maildir.list_cur() {
			mails.push(x?);
		}
		println!("acquired {} mails", mails.len());
		let mut mails = maildir.get_mails(&mut mails)?;
		mails.sort_by_key(|x| x.date);
		for mail in mails {
			let headers = mail.get_headers();
			let mut message_id = headers.get_all_values("Message-ID").join(" ");
			if message_id.is_empty() {
				message_id = format!("<{}_{}_{}@no-message-id>", mailbox, mail.id.uid_validity, mail.id.uid);
			}
			save_mail.execute(params![&mailbox, mail.id.to_i64(), message_id, mail.flags])?;
		}
	}
	}
	tx.commit()?;
	db.execute("VACUUM", params![])?;
	Ok(())
}
