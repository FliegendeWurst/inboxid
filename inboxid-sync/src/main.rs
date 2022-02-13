use std::{env, collections::HashMap};

use anyhow::Context;
use imap::types::Flag;
use itertools::Itertools;

use inboxid_lib::*;
use inboxid_sync::*;
use inboxid_sync::SyncAction::*;
use maildir::Maildir;
use mailparse::parse_headers;
use rusqlite::params;

fn main() -> Result<()> {
	let host = env::var("MAILHOST").expect("missing envvar MAILHOST");
	let user = env::var("MAILUSER").expect("missing envvar MAILUSER");
	let password = env::var("MAILPASSWORD").expect("missing envvar MAILPASSWORD");
	let port = 993;
	let mut args = env::args().skip(1).peekable();
	let dry_run = args.peek().map(|x| x == "--dry-run").unwrap_or(false);

	let args = args.collect_vec();

	sync(&host, &user, &password, port, &args, dry_run)
}

fn sync(
	host: &str,
	user: &str,
	password: &str,
	port: u16,
	mailboxes: &[String],
	dry_run: bool
) -> Result<()> {
	let (actions, remote) = compute_sync_actions(host, user, password, port, mailboxes)?;
	if dry_run {
		for action in actions {
			println!("{}", action);
		}
		return Ok(());
	}
	// perform actions
	let db = get_db()?;
	let mut imap_session = connect(host, port, user, password)?;
	println!("getting capabilities..");
	let caps = imap_session.capabilities()?;
	println!("capabilities: {}", caps.iter().map(|x| format!("{:?}", x)).join(" "));

	let mut names = Vec::new();
	let list = imap_session.list(None, Some("*"))?;
	for x in list.iter() {
		println!("{:?}", x);
		names.push(x);
	}
	let trash_dir = names.iter().filter(|x| x.attributes().iter().any(|x| *x == TRASH)).map(|x| x.name()).next();
	if trash_dir.is_none() {
		println!("Warning: unable to trash mail, no trash folder found!");
	}

	let mut have_mail = db.prepare("SELECT mailbox, uid, flags FROM mail WHERE message_id = ?")?;
	let mut delete_mail = db.prepare("DELETE FROM mail WHERE mailbox = ? AND uid = ?")?;
	let mut save_mail = db.prepare("INSERT INTO mail VALUES (?,?,?,?)")?;
	let mut maildirs: HashMap<String, Maildir> = names.iter().map(|&x| (x.name().to_owned(), get_maildir(x.name()).unwrap())).collect();
	macro_rules! ensure_mailbox {
		($name:expr) => {{
			if !maildirs.contains_key($name) {
				maildirs.insert($name.to_owned(), get_maildir($name)?);
			}
			&maildirs[$name]
		}}
	}
	let mut selection = None;

	for action in actions {
		let mut uid_valid = None;
		if let Some(mailbox) = action.mailbox() {
			if selection.is_none() || selection.as_ref().unwrap() != mailbox {
				if selection.is_some() {
					println!("expunging..");
					imap_session.expunge().context("expunge failed")?;
				}
				println!("selecting {}", mailbox);
				uid_valid = imap_session.select(mailbox).context("select failed")?.uid_validity;
				selection = Some(mailbox.to_string());
			}
		}
		macro_rules! check_valid {
			($uid_validity:expr) => {
				if uid_valid.is_none() || $uid_validity != uid_valid.unwrap() {
					println!("Warning: uid validity value changed, unable to process action!");
					continue;
				}
			}
		}
		macro_rules! update_flags {
			($mailbox:expr, $id:expr, $remote_flags:expr, $flags:expr) => {
				let local_s = $flags.contains('S');
				let local_u = $flags.contains(UNREAD);
				let remote_s = $remote_flags.contains(&Flag::Seen);
				if local_s && !remote_s {
					println!("setting Seen flag on {}/{}", $mailbox, $id.uid);
					imap_session.uid_store($id.to_imap(), "+FLAGS.SILENT (\\Seen)")?;
					$remote_flags.push(Flag::Seen);
				} else if local_u && remote_s {
					println!("removing Seen flag on {}/{}", $mailbox, $id.uid);
					imap_session.uid_store($id.to_imap(), "-FLAGS.SILENT (\\Seen)")?;
					$remote_flags.remove($remote_flags.iter().position(|x| x == &Flag::Seen).unwrap());
				}
			}
		}
		match action {
    		TrashRemote(mailbox, id) => {
				check_valid!(id.uid_validity);
				if let Some(trash_dir) = trash_dir {
					println!("trashing: {}/{}", mailbox, id.uid);
					imap_session.uid_mv(id.to_imap(), trash_dir)?;
					let gone = ensure_mailbox!(".gone");
					let uid_name = id.to_string();
					let _ = maildir_cp(&maildirs[&mailbox], gone, &uid_name, &uid_name, "", true);
					maildirs[&mailbox].delete(&uid_name)?;
					delete_mail.execute(params![mailbox, id])?;
				}
			},
    		TrashLocal(mailbox, id) => {
				check_valid!(id.uid_validity);
				println!("trashing: {}/{}", mailbox, id.uid);
				let gone = ensure_mailbox!(".gone");
				let uid_name = id.to_string();
				let _ = maildir_cp(&maildirs[&mailbox], gone, &uid_name, &uid_name, "", true);
				maildirs[&mailbox].delete(&uid_name)?;
				delete_mail.execute(params![mailbox, id])?;
			},
    		DeleteRemote(mailbox, id) => {
				imap_session.uid_store(id.to_imap(), "+FLAGS.SILENT (\\Deleted)")?;
				delete_mail.execute(params![mailbox, id])?;
				maildirs[&mailbox].delete(&id.to_string())?;
			},
    		DeleteLocal(mailbox, id) => {
				delete_mail.execute(params![mailbox, id])?;
				maildirs[&mailbox].delete(&id.to_string())?;
			},
			UpdateFlags(mailbox, mut ids) => {
				for (id, remote_flags, flags) in &mut ids {
					check_valid!(id.uid_validity);
					update_flags!(mailbox, id, remote_flags, flags);
				}
			},
    		Hardlink(mailbox, mut ids) => {
				for (new_uid, message_id, remote_flags) in &mut ids {
					check_valid!(new_uid.uid_validity);
					let local = have_mail.query_map(params![&*message_id], map3rows::<String, MaildirID, String>)?.map(|x| x.unwrap()).collect_vec();
					let (inbox, full_uid, flags) = &local[0];
					let local_id = full_uid.to_string();
					let new_id = new_uid.to_string();
					// hardlink mail
					let maildir1 = ensure_mailbox!(inbox.as_str());
					let maildir2 = &maildirs[&mailbox];
					println!("hardlinking: {}/{} -> {}/{}", inbox, local_id, mailbox, new_id);
					maildir_cp(maildir1, maildir2, &local_id, &new_id, flags, false)?;
					save_mail.execute(params![mailbox, &*new_uid, &*message_id, flags])?;
					update_flags!(mailbox, new_uid, remote_flags, flags);
				}
			},
    		Fetch(mailbox, to_fetch) => {
				let maildir = ensure_mailbox!(&mailbox);
				check_valid!(to_fetch[0].uid_validity);

				let fetch_range = to_fetch.into_iter().map(|x| x.uid.to_string()).join(",");
				let fetch = imap_session.uid_fetch(fetch_range, "RFC822")?;
				
				for mail in fetch.iter() {
					println!("fetching: {}/{}", mailbox, mail.uid.unwrap());
					let id = MaildirID::new(uid_valid.unwrap(), mail.uid.unwrap());
					let id_name = id.to_string();
					if !maildir.exists(&id_name) {
						let mail_data = mail.body().unwrap_or_default();
						let flags = imap_flags_to_maildir("".into(), mail.flags());
						maildir.store_cur_with_id_flags(&id_name, &flags, mail_data)?;
					
						let headers = parse_headers(&mail_data)?.0;
						let message_id = headers.message_id(&mailbox, id);
						save_mail.execute(params![mailbox, id.to_i64(), message_id, flags])?;
					} else {
						println!("warning: DB outdated, downloaded mail again");
					}
				}
			},
    		RemoveStale(to_remove) => {
				for mailbox in to_remove.keys() {
					for &(uid1, uid2, uid) in &to_remove[&*mailbox] {
						let uid_name = gen_id(uid1, uid2);
						println!("removing: {}/{}", mailbox, uid_name);
						let gone = ensure_mailbox!(".gone");
						let maildir = &maildirs[&*mailbox];
						// hardlink should only fail if the mail was already deleted
						let _ = maildir_cp(maildir, gone, &uid_name, &uid_name, "", true);
						maildir.delete(&uid_name)?;
						delete_mail.execute(params![mailbox, store_i64(uid)])?;
					}
				}
			},
		}
	}
	// final flag update
	for (mailbox, remote_mails) in remote {
		let maildir = ensure_mailbox!(&mailbox);
		for message_id in remote_mails.keys() {
			let (uid1, uid2, _, ref flags) = remote_mails[message_id];
			let id = gen_id(uid1, uid2);
			let _ = maildir.update_flags(&id, |f| {
				let f = f.replace(UNREAD, "");
				let f = imap_flags_to_maildir(f, flags);
				Maildir::normalize_flags(&f)
			});
		}
	}
	Ok(())
}
