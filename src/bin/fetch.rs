use std::{cmp, env, error::Error, fs, io, net::TcpStream, time::Duration};

use itertools::Itertools;
use maildir::Maildir;
use rustls_connector::RustlsConnector;

fn main() -> Result<(), Box<dyn Error>> {
	let host = env::var("MAILHOST").expect("missing envvar MAILHOST");
	let user = env::var("MAILUSER").expect("missing envvar MAILUSER");
	let password = env::var("MAILPASSWORD").expect("missing envvar MAILPASSWORD");
	let maildir = env::var("MAILDIR").expect("missing envvar MAILDIR");
	let maildir = Maildir::from(maildir);
	maildir.create_dirs()?;
	let port = 993;

	fetch_inbox_top(&host, user, password, port, "INBOX", maildir)
}

fn fetch_inbox_top(
	host: &str,
	user: String,
	password: String,
	port: u16,
	mailbox: &str,
	maildir: Maildir,
) -> Result<(), Box<dyn Error>> {
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
	let mut imap_session = client.login(&user, &password).map_err(|e| e.0)?;
	println!("getting capabilities..");
	let caps = imap_session.capabilities()?;
	println!("capabilities: {}", caps.iter().map(|x| format!("{:?}", x)).join(" "));

	while let Ok(x) = imap_session.unsolicited_responses.recv_timeout(Duration::from_millis(50)) {
		println!("aah what is this: {:?}", x);
	}

	// we want to fetch the first email in the INBOX mailbox
	let resp = imap_session.examine(mailbox)?;
	// TODO(errors)
	let uid_validity = resp.uid_validity.unwrap();
	let uid_next = resp.uid_next.unwrap();
	println!("uid: {} {}", uid_validity, uid_next);

	let (prev_uid_validity, prev_uid) = maildir.get_file(".uid").map(
		|x| {
			let mut fields = x.splitn(2, ',');
			// TODO(2038): check if mailservers still just return the mailbox creation time in seconds
			let uid_validity = fields.next().map(|x| x.trim().parse::<u32>().ok()).unwrap_or_default().unwrap_or(0);
			let uid_last = fields.next().map(|x| x.trim().parse::<u32>().ok()).unwrap_or_default().unwrap_or(0);
			(uid_validity, uid_last)
		}
	).unwrap_or((0, 0));
	let fetch_range;
	if uid_validity != prev_uid_validity {
		fetch_range = "1:*".to_owned();
		// TODO: somehow remove invalidated messages
	} else if uid_next != prev_uid + 1 {
		fetch_range = format!("{}:*", prev_uid + 1);
	} else {
		println!("no new mail.");
		imap_session.logout()?;
		return Ok(());
	}
	println!("fetching {:?}", fetch_range);

	let messages = imap_session.uid_fetch(&fetch_range, "RFC822")?;
	let mut largest_uid = prev_uid;
	for mail in messages.iter() {
		let uid = mail.uid.unwrap();
		largest_uid = cmp::max(largest_uid, uid);
		println!("mail {:?}", uid);
		let id = format!("{}_{}", uid_validity, uid);
		if !maildir.exists(&id).unwrap_or(false) {
			let mail_data = mail.body().unwrap_or_default();
			maildir.store_new_with_id(&id, mail_data)?;
		}
	}
	let uid = cmp::max(uid_next - 1, largest_uid);
	maildir.save_file(".uid", &format!("{},{}", uid_validity, uid))?;

	// be nice to the server and log out
	imap_session.logout()?;

	Ok(())
}

trait MaildirExtension {
	fn get_file(&self, name: &str) -> Result<String, io::Error>;
	fn save_file(&self, name: &str, content: &str) -> Result<(), io::Error>;
}

impl MaildirExtension for Maildir {
	fn get_file(&self, name: &str) -> Result<String, io::Error> {
		fs::read_to_string(self.path().join(name))
	}

	fn save_file(&self, name: &str, content: &str) -> Result<(), io::Error> {
		fs::write(self.path().join(name), content)
	}
}
