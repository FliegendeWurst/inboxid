use std::{env, net::TcpStream};

use imap::Session;
use maildir::Maildir;
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
