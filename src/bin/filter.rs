use std::env;

use anyhow::anyhow;
use inboxid::*;
use itertools::Itertools;
use mailproc::Config;

fn main() -> Result<()> {
	let args = env::args().collect_vec();
	if args.len() < 3 {
		Err(anyhow!("required arguments: mailbox name, filter file path"))?;
		unreachable!()
	} else {
		do_filtering(&args[1], &args[2])
	}
}

fn do_filtering(mailbox: &str, config: &str) -> Result<()> {
	let config = Config::load_from_path(config)?;

	let maildir = get_maildir(mailbox)?;

	let mut mails = Vec::new();
	for x in maildir.list_cur() {
		mails.push(x?);
	}
	let mut mails = maildir.get_mails(&mut mails)?;
	mails.sort_by_key(|x| x.id);
	
	let mut imap_session = get_imap_session()?;
	imap_session.select(mailbox)?;

	for mail in mails {
		if mail.has_flag2(TRASHED) || mail.has_flag2(DELETE) {
			continue; // ignore mails marked for deletion
		}
		if let Some(action) = mailproc::handle(&mail, &[], &config) { // TODO: provide raw bytes
			println!("{:?}", action.0);
			println!(" matched {}", mail.subject);
			for action in action.0.action.as_ref().unwrap() {
				match &*action[0] {
					"mv" => {
						let uid = mail.id.to_imap();
						println!(" moving to mailbox {}", action[1]);
						// update flags
						let flags = mail.get_flags();
						let flags = maildir_flags_to_imap(&flags);
						imap_session.uid_store(&uid, &format!("FLAGS.SILENT {}", imap_flags_to_cmd(&flags)))?;
						imap_session.uid_mv(&uid, &action[1])?;
					},
					x => {
						println!("WARNING: unknown action {:?}", x);
					}
				}
			}
		}
	}

	Ok(())
}
