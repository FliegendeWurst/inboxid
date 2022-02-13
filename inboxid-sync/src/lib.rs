use std::{collections::HashMap, borrow::Cow, fmt::Display};

use anyhow::Context;
use imap::types::{Flag, NameAttribute};
use itertools::Itertools;
use maildir::Maildir;

use inboxid_lib::*;
use mailparse::parse_header;
use rusqlite::{Row, params, types::FromSql};

pub static TRASH: NameAttribute = NameAttribute::Custom(Cow::Borrowed("\\Trash"));

pub enum SyncAction {
	TrashRemote(String, MaildirID),
	TrashLocal(String, MaildirID),
	DeleteRemote(String, MaildirID),
	DeleteLocal(String, MaildirID),
	UpdateFlags(String, Vec<(MaildirID, Vec<Flag<'static>>, String)>),
	Hardlink(String, Vec<(MaildirID, String, Vec<Flag<'static>>)>),
	Fetch(String, Vec<MaildirID>),
	RemoveStale(HashMap<String, Vec<(u32, u32, u64)>>)
}

impl SyncAction {
	pub fn mailbox(&self) -> Option<&str> {
		match self {
    		TrashRemote(mailbox, _) => Some(mailbox),
    		TrashLocal(mailbox, _) => Some(mailbox),
    		DeleteRemote(mailbox, _) => Some(mailbox),
    		DeleteLocal(mailbox, _) => Some(mailbox),
			UpdateFlags(mailbox, _) => Some(mailbox),
    		Hardlink(mailbox, _) => Some(mailbox),
    		Fetch(mailbox, _) => Some(mailbox),
    		RemoveStale(_) => None,
		}
	}
}

use SyncAction::*;

impl Display for SyncAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
    		TrashRemote(mailbox, id) => write!(f, "thrash remotely: {}/{}\n", mailbox, id)?,
    		TrashLocal(mailbox, id) => write!(f, "thrash locally: {}/{}\n", mailbox, id)?,
    		DeleteRemote(mailbox, id) => write!(f, "delete remotely: {}/{}\n", mailbox, id)?,
    		DeleteLocal(mailbox, id) => write!(f, "delete locally: {}/{}\n", mailbox, id)?,
			UpdateFlags(mailbox, _) => write!(f, "updating flags of mail in {}\n", mailbox)?,
    		Hardlink(mailbox, id) => write!(f, "hardlink from local: {}/{:?}\n", mailbox, id)?,
    		Fetch(mailbox, id) => write!(f, "fetch: {}/{:?}", mailbox, id)?,
    		RemoveStale(map) => write!(f, "remove stale mail: {:?}", map)?,
		}
        Ok(())
    }
}

pub fn compute_sync_actions(
	host: &str,
	user: &str,
	password: &str,
	port: u16,
	mailboxes: &[String]
) -> Result<(Vec<SyncAction>, HashMap<String, HashMap<String, (u32, u32, MaildirID, Vec<Flag<'static>>)>>)> {
	let mut actions = Vec::new();

	let mut db = get_db()?;
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
		if !mailboxes.is_empty() && !mailboxes.iter().any(|x| x == mailbox) {
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
		remote.insert(mailbox.to_string(), mails);
	}

	// start a transaction to fully simulate fetching behaviour (drop changes afterwards)
	let tx = db.transaction()?;
	let mut have_mail = tx.prepare("SELECT mailbox, uid, flags FROM mail WHERE message_id = ?")?;
	let mut delete_mail = tx.prepare("DELETE FROM mail WHERE mailbox = ? AND uid = ?")?;
	let mut all_mail = tx.prepare("SELECT uid, message_id, flags FROM mail WHERE mailbox = ?")?;
	let mut save_mail = tx.prepare("INSERT INTO mail VALUES (?,?,?,?)")?;
	let mut maildirs: HashMap<String, Maildir> = names.iter().map(|&x| (x.name().to_owned(), get_maildir(x.name()).unwrap())).collect();
	let mut printed_trash_warning = false;
	let trash_dir = names.iter().filter(|x| x.attributes().iter().any(|x| *x == TRASH)).map(|x| x.name()).next();
	let mut to_remove: HashMap<String, _> = HashMap::new();
	for &name in &names {
		let mailbox = name.name();
		// if the user specified some mailboxes, only process those
		if !mailboxes.is_empty() && !mailboxes.iter().any(|x| x == mailbox) {
			continue;
		}
		let is_trash = name.attributes().iter().any(|x| *x == TRASH);
		let remote_mails = remote.get_mut(mailbox).unwrap();
		println!("selecting {}", mailbox);
		imap_session.select(mailbox).context("select failed")?;
		let all_mails = all_mail.query_map(params![mailbox], map3rows::<i64, String, String>)?;
		for x in all_mails {
			let (uid, mid, flags) = x?;
			let uid: MaildirID = uid.into();
			if flags.contains(TRASHED) && !is_trash {
				if let Some(_) = trash_dir {
					println!("trashing: {}/{}", mailbox, uid);
					if remote_mails.contains_key(&mid) {
						actions.push(TrashRemote(mailbox.to_owned(), uid));
					} else {
						actions.push(TrashLocal(mailbox.to_owned(), uid));
					}
					delete_mail.execute(params![mailbox, uid])?;
				} else if !printed_trash_warning {
					println!("Warning: unable to trash mail, no trash folder found!");
					printed_trash_warning = true;
				}
			} else if flags.contains(DELETE) {
				println!("deleting: {}/{}", mailbox, uid);
				if remote_mails.contains_key(&mid) {
					actions.push(DeleteRemote(mailbox.to_owned(), uid));
				} else {
					actions.push(DeleteLocal(mailbox.to_owned(), uid));
				}
				delete_mail.execute(params![mailbox, uid])?;
			}
		}

		let mut to_flag = Vec::new();
		let mut to_fetch = Vec::new();
		let mut to_hardlink = Vec::new();
		for (message_id, entry) in remote_mails.iter_mut() {
			let (uid1, uid2, full_uid, remote_flags) = entry;
			let local = have_mail.query_map(params![message_id], map3rows::<String, MaildirID, String>)?.map(|x| x.unwrap()).collect_vec();
			
			if let Some((_, full_uid, flags)) = local.iter().filter(|x| x.0 == mailbox && x.1 == *full_uid).next() {
				to_flag.push((*full_uid, remote_flags.clone(), flags.clone()));
				continue;
			}
			if !local.is_empty() {
				let (_, _, flags) = &local[0];
				let new_uid = MaildirID::new(*uid1, *uid2);
				to_hardlink.push((new_uid, message_id.clone(), remote_flags.clone()));
				save_mail.execute(params![mailbox, new_uid.to_i64(), message_id, flags])?;
			} else if !is_trash { // do not fetch trashed mail
				println!("fetching {:?} {:?} as it is not in {:?}", uid2, message_id, local);
				let new_uid = MaildirID::new(*uid1, *uid2);
				to_fetch.push(new_uid);
			}
		}
		if !to_flag.is_empty() {
			actions.push(UpdateFlags(mailbox.to_string(), to_flag));
		}
		if !to_hardlink.is_empty() {
			actions.push(Hardlink(mailbox.to_string(), to_hardlink));
		}
		if !to_fetch.is_empty() {
			actions.push(Fetch(mailbox.to_string(), to_fetch));
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
			to_remove.insert(mailbox.to_string(), removed);
		}
	}
	actions.push(RemoveStale(to_remove));

	// be nice to the server and log out
	imap_session.logout()?;

	Ok((actions, remote))
}

pub fn map3rows<A: FromSql, B: FromSql, C: FromSql>(row: &Row) -> rusqlite::Result<(A, B, C)> {
	let a = row.get::<_, A>(0)?;
	let b = row.get::<_, B>(1)?;
	let c = row.get::<_, C>(2)?;
	Ok((a, b, c))
}
