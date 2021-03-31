use std::{collections::HashMap, env};

use imap::types::Flag;
use itertools::Itertools;
use maildir::Maildir;

use inboxid::*;
use mailparse::{MailHeaderMap, parse_header, parse_headers};
use rusqlite::params;

fn main() -> Result<()> {
	let host = env::var("MAILHOST").expect("missing envvar MAILHOST");
	let user = env::var("MAILUSER").expect("missing envvar MAILUSER");
	let password = env::var("MAILPASSWORD").expect("missing envvar MAILPASSWORD");
	let port = 993;

	sync(&host, &user, &password, port)
}

fn sync(
	host: &str,
	user: &str,
	password: &str,
	port: u16,
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
		names.push(x.name());
	}
	names = vec!["INBOX", "Github", "nebenan"];

	let mut remote = HashMap::new();

	for &mailbox in &names {
		println!("indexing {}", mailbox);
		let resp = imap_session.examine(mailbox)?;
		let uid_validity = resp.uid_validity.unwrap();

		let mut mails = HashMap::new();
		let messages = imap_session.uid_fetch("1:*", "(FLAGS BODY[HEADER.FIELDS (MESSAGE-ID)])")?;
		for m in messages.iter() {
			let flags = m.flags();
			if flags.contains(&Flag::Deleted) {
				continue;
			}
			let header = m.header().unwrap();
			let header = parse_header(header)?.0;
			let uid = m.uid.unwrap();
			let full_uid = ((uid_validity as u64) << 32) | uid as u64;
			let flags = flags.iter().map(|x| remove_cow(x)).collect_vec();
			mails.insert(header.get_value(), (uid_validity, uid, full_uid, flags));
		}
		remote.insert(mailbox, mails);
	}

	let mut have_mail = db.prepare("SELECT mailbox, uid FROM mail WHERE message_id = ?")?;
	let mut delete_mail = db.prepare("DELETE FROM mail WHERE mailbox = ? AND uid = ?")?;
	let mut all_mail = db.prepare("SELECT uid, message_id FROM mail WHERE mailbox = ?")?;
	let mut save_mail = db.prepare("INSERT INTO mail VALUES (?,?,?)")?;
	let mut maildirs: HashMap<&str, Maildir> = names.iter().map(|&x| (x, get_maildir(x).unwrap())).collect();
	let mut to_remove: HashMap<&str, _> = HashMap::new();
	for &mailbox in &names {
		let remote_mails = &remote[mailbox];

		let mut to_fetch = Vec::new();
		for message_id in remote_mails.keys() {
			let (uid1, uid2, full_uid, ref _flags) = remote_mails[message_id];
			let local = have_mail.query_map(params![message_id], |row| Ok((row.get::<_, String>(0)?, load_i64(row.get::<_, i64>(1)?))))?.map(|x| x.unwrap()).collect_vec();
			if local.iter().any(|x| x.0 == mailbox && x.1 == full_uid) {
				continue;
			}
			if !local.is_empty() {
				let (inbox, full_uid) = &local[0];
				let local_uid1 = (full_uid >> 32) as u32;
				let local_uid2 = ((full_uid << 32) >> 32) as u32;
				let local_id = gen_id(local_uid1, local_uid2);
				// hardlink mail
				let maildir1 = &maildirs[&**inbox];
				let name = maildir1.find_filename(&local_id).unwrap();
				let maildir2 = &maildirs[mailbox];
				let new_id = gen_id(uid1, uid2);
				println!("hardlinking: {}/{} -> {}/{}", inbox, local_id, mailbox, new_id);
				maildir2.store_cur_from_path(&new_id, name)?;
				save_mail.execute(params![mailbox, store_i64(*full_uid), message_id])?;
			} else {
				to_fetch.push(uid2);
			}
		}
		if !to_fetch.is_empty() {
			let maildir = &maildirs[mailbox];
			let resp = imap_session.examine(mailbox)?;
			let uid_validity = resp.uid_validity.unwrap();

			let fetch_range = to_fetch.into_iter().map(|x| x.to_string()).join(",");
			let fetch = imap_session.uid_fetch(fetch_range, "RFC822")?;

			for mail in fetch.iter() {
				let uid = mail.uid.unwrap();
				println!("fetching: {}/{}", mailbox, uid);
				let id = gen_id(uid_validity, uid);
				if !maildir.exists(&id) {
					let mail_data = mail.body().unwrap_or_default();
					maildir.store_cur_with_id(&id, mail_data)?;

					let headers = parse_headers(&mail_data)?.0;
					let message_id = headers.get_all_values("Message-ID").join(" ");
					let full_uid = ((uid_validity as u64) << 32) | uid as u64;
					save_mail.execute(params![mailbox, store_i64(full_uid), message_id])?;
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
				let mut f = f.to_owned();
				if flags.contains(&Flag::Seen) {
					f.push('S');
				} else {
					f = f.replace('S', "");
				}
				if flags.contains(&Flag::Answered) {
					f.push('R');
				} else {
					f = f.replace('R', "");
				}
				if flags.contains(&Flag::Flagged) {
					f.push('F');
				} else {
					f = f.replace('F', "");
				}
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
			if !remote_mails.contains_key(&message_id) {
				removed.push((uid1, uid2, uid));
			}
		}
		if !removed.is_empty() {
			to_remove.insert(mailbox, removed);
		}
	}
	for mailbox in to_remove.keys() {
		for &(uid1, uid2, uid) in &to_remove[mailbox] {
			let uid_name = gen_id(uid1, uid2);
			println!("removing: {}/{}", mailbox, uid_name);
			if !maildirs.contains_key(".gone") {
				maildirs.insert(".gone", get_maildir(".gone")?);
			}
			let maildir = &maildirs[mailbox];
			let name = maildir.find_filename(&uid_name).unwrap();
			maildirs[".gone"].store_new_from_path(&format!("{}_{}", mailbox, uid_name), name)?;
			maildir.delete(&uid_name)?;
			delete_mail.execute(params![mailbox, store_i64(uid)])?;
		}
	}

	// be nice to the server and log out
	imap_session.logout()?;

	Ok(())
}
