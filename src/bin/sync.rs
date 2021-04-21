use std::{borrow::Cow, collections::HashMap, env};

use anyhow::Context;
use imap::types::{Flag, NameAttribute};
use itertools::Itertools;
use maildir::Maildir;

use inboxid::*;
use mailparse::{parse_header, parse_headers};
use rusqlite::{Row, params, types::FromSql};

const TRASH: NameAttribute = NameAttribute::Custom(Cow::Borrowed("\\Trash"));

fn main() -> Result<()> {
	let host = env::var("MAILHOST").expect("missing envvar MAILHOST");
	let user = env::var("MAILUSER").expect("missing envvar MAILUSER");
	let password = env::var("MAILPASSWORD").expect("missing envvar MAILPASSWORD");
	let port = 993;
	let args = env::args().skip(1).collect_vec();
	let args = args.iter().map(|x| &**x).collect_vec();

	sync(&host, &user, &password, port, &args)
}

fn sync(
	host: &str,
	user: &str,
	password: &str,
	port: u16,
	mailboxes: &[&str]
) -> Result<()> {
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

	let mut remote = HashMap::new();

	for &name in &names {
		let mailbox = name.name();
		// if the user specified some mailboxes, only process those
		if !mailboxes.is_empty() && !mailboxes.contains(&mailbox) {
			continue;
		}
		println!("indexing {}", mailbox);
		let resp = imap_session.examine(mailbox)?;
		let uid_validity = resp.uid_validity.unwrap();

		let mut mails = HashMap::new();
		let messages = imap_session.uid_fetch("1:*", "(FLAGS BODY[HEADER.FIELDS (MESSAGE-ID)])")?;
		for m in messages.iter() {
			let id = MaildirID::new(uid_validity, m.uid.unwrap());
			let flags = m.flags();
			if flags.contains(&Flag::Deleted) {
				continue;
			}
			let header = m.header().unwrap();
			let mut message_id = parse_header(header).map(|x| x.0.get_value()).unwrap_or_default();
			if message_id.is_empty() {
				message_id = fallback_mid(mailbox, id);
			}
			let flags = flags.iter().map(|x| remove_cow(x)).collect_vec();
			mails.insert(message_id, (id.uid_validity, id.uid, id, flags));
		}
		remote.insert(mailbox, mails);
	}

	let mut have_mail = db.prepare("SELECT mailbox, uid, flags FROM mail WHERE message_id = ?")?;
	let mut delete_mail = db.prepare("DELETE FROM mail WHERE mailbox = ? AND uid = ?")?;
	let mut all_mail = db.prepare("SELECT uid, message_id, flags FROM mail WHERE mailbox = ?")?;
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
	let mut printed_trash_warning = false;
	let trash_dir = names.iter().filter(|x| x.attributes().iter().any(|x| *x == TRASH)).map(|x| x.name()).next();
	let mut to_remove: HashMap<&str, _> = HashMap::new();
	for &name in &names {
		let mailbox = name.name();
		// if the user specified some mailboxes, only process those
		if !mailboxes.is_empty() && !mailboxes.contains(&mailbox) {
			continue;
		}
		let is_trash = name.attributes().iter().any(|x| *x == TRASH);
		let remote_mails = remote.get_mut(mailbox).unwrap();
		println!("selecting {}", mailbox);
		imap_session.select(mailbox).context("select failed")?;
		let all_mails = all_mail.query_map(params![mailbox], map3rows::<i64, String, String>)?;
		let mut deleted_some = false;
		for x in all_mails {
			let (uid, mid, flags) = x?;
			let uid: MaildirID = uid.into();
			if flags.contains(TRASHED) && !is_trash {
				if let Some(trash_dir) = trash_dir {
					println!("trashing: {}/{}", mailbox, uid);
					if remote_mails.contains_key(&mid) {
						imap_session.uid_mv(uid.to_imap(), trash_dir)?;
					} else {
						println!("Warning: only trashing locally!");
					}
					let gone = ensure_mailbox!(".gone");
					let uid_name = uid.to_string();
					let _ = maildir_cp(&maildirs[mailbox], gone, &uid_name, &uid_name, "", true);
					maildirs[mailbox].delete(&uid_name)?;
					delete_mail.execute(params![mailbox, uid])?;
				} else if !printed_trash_warning {
					println!("Warning: unable to trash mail, no trash folder found!");
					printed_trash_warning = true;
				}
			} else if flags.contains(DELETE) {
				println!("deleting: {}/{}", mailbox, uid);
				if remote_mails.contains_key(&mid) {
					imap_session.uid_store(uid.to_imap(), "+FLAGS.SILENT (\\Deleted)")?;
				} else {
					println!("Warning: only deleting locally!");
				}
				remote_mails.remove(&mid);
				delete_mail.execute(params![mailbox, uid])?;
				maildirs[mailbox].delete(&uid.to_string())?;
				deleted_some = true;
			}
		}
		if deleted_some {
			imap_session.expunge().context("expunge failed")?;
		}

		let mut to_fetch = Vec::new();
		for (message_id, entry) in remote_mails.iter_mut() {
			let (uid1, uid2, full_uid, remote_flags) = entry;
			let local = have_mail.query_map(params![message_id], map3rows::<String, MaildirID, String>)?.map(|x| x.unwrap()).collect_vec();
			macro_rules! update_flags {
				($id:expr, $flags:expr) => {
					let local_s = $flags.contains('S');
					let local_u = $flags.contains(UNREAD);
					let remote_s = remote_flags.contains(&Flag::Seen);
					if local_s && !remote_s {
						println!("setting Seen flag on {}/{}", mailbox, $id.uid);
						imap_session.uid_store($id.to_imap(), "+FLAGS.SILENT (\\Seen)")?;
						remote_flags.push(Flag::Seen);
					} else if local_u && remote_s {
						println!("removing Seen flag on {}/{}", mailbox, $id.uid);
						imap_session.uid_store($id.to_imap(), "-FLAGS.SILENT (\\Seen)")?;
						remote_flags.remove(remote_flags.iter().position(|x| x == &Flag::Seen).unwrap());
					}
				}
			}
			if let Some((_, full_uid, flags)) = local.iter().filter(|x| x.0 == mailbox && x.1 == *full_uid).next() {
				update_flags!(full_uid, flags);
				continue;
			}
			if !local.is_empty() {
				let (inbox, full_uid, flags) = &local[0];
				let local_id = full_uid.to_string();
				let new_uid = MaildirID::new(*uid1, *uid2);
				let new_id = new_uid.to_string();
				// hardlink mail
				let maildir1 = ensure_mailbox!(inbox.as_str());
				let maildir2 = &maildirs[mailbox];
				println!("hardlinking: {}/{} -> {}/{}", inbox, local_id, mailbox, new_id);
				maildir_cp(maildir1, maildir2, &local_id, &new_id, flags, false)?;
				save_mail.execute(params![mailbox, new_uid.to_i64(), message_id, flags])?;
				update_flags!(new_uid, flags);
			} else if !is_trash { // do not fetch trashed mail
				println!("fetching {:?} {:?} as it is not in {:?}", uid2, message_id, local);
				to_fetch.push(uid2);
			}
		}
		if !to_fetch.is_empty() {
			let resp = imap_session.examine(mailbox)?;
			let uid_validity = resp.uid_validity.unwrap();
			let maildir = &maildirs[mailbox];

			let fetch_range = to_fetch.into_iter().map(|x| x.to_string()).join(",");
			let fetch = imap_session.uid_fetch(fetch_range, "RFC822")?;

			for mail in fetch.iter() {
				println!("fetching: {}/{}", mailbox, mail.uid.unwrap());
				let id = MaildirID::new(uid_validity, mail.uid.unwrap());
				let id_name = id.to_string();
				if !maildir.exists(&id_name) {
					let mail_data = mail.body().unwrap_or_default();
					let flags = imap_flags_to_maildir("".into(), mail.flags());
					maildir.store_cur_with_id_flags(&id_name, &flags, mail_data)?;

					let headers = parse_headers(&mail_data)?.0;
					let message_id = headers.message_id(mailbox, id);
					save_mail.execute(params![mailbox, id.to_i64(), message_id, flags])?;
				} else {
					println!("warning: DB outdated, downloaded mail again");
				}
			}
		}
		let maildir = &maildirs[mailbox];
		for message_id in remote_mails.keys() {
			let (uid1, uid2, _, ref flags) = remote_mails[message_id];
			let id = gen_id(uid1, uid2);
			let _ = maildir.update_flags(&id, |f| {
				let f = f.replace(UNREAD, "");
				let f = imap_flags_to_maildir(f, flags);
				Maildir::normalize_flags(&f)
			});
		}
		let mails = all_mail.query_map(params![mailbox], |row|
			Ok((load_i64(row.get::<_, i64>(0)?), row.get::<_, String>(1)?)))?
			.map(|x| x.unwrap()).collect_vec();
		let mut removed = Vec::new();
		for (uid, message_id) in mails {
			let uid1 = (uid >> 32) as u32;
			let uid2 = ((uid << 32) >> 32) as u32;
			if !remote_mails.contains_key(&message_id) && !message_id.ends_with("@no-message-id>") {
				removed.push((uid1, uid2, uid));
			}
		}
		if !removed.is_empty() {
			to_remove.insert(mailbox, removed);
		}
	}
	for &mailbox in to_remove.keys() {
		for &(uid1, uid2, uid) in &to_remove[mailbox] {
			let uid_name = gen_id(uid1, uid2);
			println!("removing: {}/{}", mailbox, uid_name);
			let gone = ensure_mailbox!(".gone");
			let maildir = &maildirs[mailbox];
			// hardlink should only fail if the mail was already deleted
			let _ = maildir_cp(maildir, gone, &uid_name, &uid_name, "", true);
			maildir.delete(&uid_name)?;
			delete_mail.execute(params![mailbox, store_i64(uid)])?;
		}
	}

	// be nice to the server and log out
	imap_session.logout()?;

	Ok(())
}

pub fn map3rows<A: FromSql, B: FromSql, C: FromSql>(row: &Row) -> rusqlite::Result<(A, B, C)> {
	let a = row.get::<_, A>(0)?;
	let b = row.get::<_, B>(1)?;
	let c = row.get::<_, C>(2)?;
	Ok((a, b, c))
}
