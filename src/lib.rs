use std::{borrow::Cow, convert::{TryFrom, TryInto}, env, fs, io, net::TcpStream, ops::Deref};

use anyhow::Context;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use imap::{Session, types::Flag};
use maildir::{MailEntry, Maildir};
use mailparse::{MailHeaderMap, ParsedMail, dateparse};
use rusqlite::{Connection, params};
use rustls_connector::{RustlsConnector, rustls::{ClientSession, StreamOwned}};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type ImapSession = Session<StreamOwned<ClientSession, TcpStream>>;

pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<ImapSession> {
	println!("connecting..");
	let stream = TcpStream::connect((host, port))?;
	let tls = RustlsConnector::new_with_native_certs()?;
	println!("initializing TLS..");
	let tlsstream = tls.connect(host, stream)?;
	println!("initializing client..");
	let client = imap::Client::new(tlsstream);

	// the client we have here is unauthenticated.
	// to do anything useful with the e-mails, we need to log in
	println!("logging in..");
	Ok(client.login(user, password).map_err(|e| e.0)?)
}

pub fn get_maildir(mailbox: &str) -> Result<Maildir> {
	let maildir = env::var("MAILDIR").expect("missing envvar MAILDIR");
	let maildir = format!("{}/{}", maildir, mailbox);
	let maildir = Maildir::from(maildir);
	maildir.create_dirs()?;
	Ok(maildir)
}

pub fn get_db() -> Result<Connection> {
	let db = env::var("MAILDB").expect("missing envvar MAILDB");
	let conn = Connection::open(&db)?;

	conn.execute("
	CREATE TABLE IF NOT EXISTS mail(
		mailbox STRING NOT NULL,
		uid INTEGER NOT NULL,
		message_id STRING NOT NULL
	)", params![])?;

	Ok(conn)
}

pub fn gen_id(uid_validity: u32, uid: u32) -> String {
	format!("{}_{}", uid_validity, uid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaildirID {
	uid_validity: u32,
	pub uid: u32,
}

impl TryFrom<&str> for MaildirID {
	type Error = Box<dyn std::error::Error>;

	fn try_from(id: &str) -> Result<Self> {
		let mut parts = id.splitn(2, '_');
		let uid_validity = parts.next().context("invalid ID")?.parse()?;
		let uid = parts.next().context("invalid ID")?.parse()?;
		Ok(MaildirID {
			uid_validity,
			uid,
		})
	}
}

impl ToString for MaildirID {
	fn to_string(&self) -> String {
		format!("{}_{}", self.uid_validity, self.uid)
	}
}

pub struct EasyMail<'a> {
	pub mail: ParsedMail<'a>,
	pub id: MaildirID,
	pub flags: String,
	pub from: String,
	pub subject: String,
	pub date: DateTime<Local>,
	pub date_iso: String,
}

impl<'a> Deref for EasyMail<'a> {
	type Target = ParsedMail<'a>;

	fn deref(&self) -> &Self::Target {
		&self.mail
	}
}

pub trait MaildirExtension {
	fn get_file(&self, name: &str) -> std::result::Result<String, io::Error>;
	fn save_file(&self, name: &str, content: &str) -> std::result::Result<(), io::Error>;
	fn get_mails<'a>(&self, entries: &'a mut [MailEntry]) -> Result<Vec<EasyMail<'a>>>;
}

impl MaildirExtension for Maildir {
	fn get_file(&self, name: &str) -> std::result::Result<String, io::Error> {
		fs::read_to_string(self.path().join(name))
	}

	fn save_file(&self, name: &str, content: &str) -> std::result::Result<(), io::Error> {
		fs::write(self.path().join(name), content)
	}

	fn get_mails<'a>(&self, entries: &'a mut [MailEntry]) -> Result<Vec<EasyMail<'a>>> {
		let mut mails = Vec::new();
		for maile in entries {
			let id = maile.id().try_into()?;
			let flags = maile.flags().to_owned();
			let mail = maile.parsed()?;
			let headers = mail.get_headers();
			let from = headers.get_all_values("From").join(" ");
			let subject = headers.get_all_values("Subject").join(" ");
			let date = headers.get_all_values("Date").join(" ");
			let date = dateparse(&date).map(|x|
				Local.from_utc_datetime(&NaiveDateTime::from_timestamp(x, 0))
			)?;
			mails.push(EasyMail {
				mail,
				flags,
				id,
				from,
				subject,
				date_iso: date.format("%Y-%m-%d %H:%M").to_string(),
				date,
			});
		}
		Ok(mails)
	}
}

pub fn store_i64(x: u64) -> i64 {
	unsafe { std::mem::transmute(x) }
}

pub fn load_i64(x: i64) -> u64 {
	unsafe { std::mem::transmute(x) }
}

pub fn remove_cow<'a>(x: &Flag<'a>) -> Flag<'static> {
	match x {
		Flag::Custom(x) => Flag::Custom(Cow::Owned(x.to_string())),
		Flag::Seen => Flag::Seen,
		Flag::Answered => Flag::Answered,
		Flag::Flagged => Flag::Flagged,
		Flag::Deleted => Flag::Deleted,
		Flag::Draft => Flag::Draft,
		Flag::Recent => Flag::Recent,
		Flag::MayCreate => Flag::MayCreate,
	}
}

pub fn get_imap_session() -> Result<ImapSession> {
	let host = env::var("MAILHOST").expect("missing envvar MAILHOST");
	let user = env::var("MAILUSER").expect("missing envvar MAILUSER");
	let password = env::var("MAILPASSWORD").expect("missing envvar MAILPASSWORD");
	let port = 993;
	connect(&host, port, &user, &password)
}
