use std::{borrow::Cow, env, fs, io, net::TcpStream};

use imap::{Session, types::Flag};
use maildir::Maildir;
use rusqlite::{Connection, params};
use rustls_connector::{RustlsConnector, rustls::{ClientSession, StreamOwned}};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Session<StreamOwned<ClientSession, TcpStream>>> {
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

pub trait MaildirExtension {
	fn get_file(&self, name: &str) -> std::result::Result<String, io::Error>;
	fn save_file(&self, name: &str, content: &str) -> std::result::Result<(), io::Error>;
}

impl MaildirExtension for Maildir {
	fn get_file(&self, name: &str) -> std::result::Result<String, io::Error> {
		fs::read_to_string(self.path().join(name))
	}

	fn save_file(&self, name: &str, content: &str) -> std::result::Result<(), io::Error> {
		fs::write(self.path().join(name), content)
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
