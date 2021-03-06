use std::{borrow::Cow, convert::{TryFrom, TryInto}, env, fmt::{Debug, Display}, fs, hash::Hash, io, net::TcpStream, ops::Deref, path::PathBuf};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use cursive::{theme::{BaseColor, Color, ColorStyle, ColorType, Effect, Style}, utils::span::{IndexedCow, IndexedSpan, SpannedString}};
use cursive_tree_view::TreeEntry;
use directories_next::ProjectDirs;
use imap::{Session, types::Flag};
use log::info;
use maildir::{MailEntry, Maildir};
use mailparse::{MailHeaderMap, ParsedMail, SingleInfo, addrparse, dateparse};
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use petgraph::{Graph, graph::NodeIndex};
use rusqlite::{Connection, ToSql, params, types::{FromSql, ToSqlOutput}};
use rustls_connector::{RustlsConnector, rustls::{ClientSession, StreamOwned}};
use serde::{Deserializer, Serializer};
use serde::de::Visitor;
use serde_derive::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type ImapSession = Session<StreamOwned<ClientSession, TcpStream>>;

pub const UNREAD: char = 'U';
pub const TRASHED: char = 'T';
pub const DELETE: char = 'E'; // Exterminate
pub const SEEN: char = 'S';
pub const REPLIED: char = 'R';
pub const FLAGGED: char = 'F';

pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<ImapSession> {
	println!("connecting..");
	let stream = TcpStream::connect((host, port)).context("TCP connect failed")?;
	let tls = RustlsConnector::new_with_native_certs().context("TLS configuration failed")?;
	println!("initializing TLS..");
	let tlsstream = tls.connect(host, stream).context("TLS connection failed")?;
	println!("initializing client..");
	let client = imap::Client::new(tlsstream);

	// the client we have here is unauthenticated.
	// to do anything useful with the e-mails, we need to log in
	println!("logging in..");
	Ok(client.login(user, password).map_err(|e| e.0)?)
}

pub fn get_maildirs() -> Result<Vec<String>> {
	let maildir = env::var("MAILDIR").expect("missing envvar MAILDIR");
	let mut dirs = vec![];
	for dir in fs::read_dir(&maildir)? {
		let dir = dir?;
		if dir.file_type()?.is_dir() {
			let name = dir.file_name().into_string().map_err(|_| anyhow!("failed to decode directory name"))?;
			if !name.starts_with('.') {
				dirs.push(name);
			}
		}
	}
	Ok(dirs)
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
		message_id STRING NOT NULL,
		flags STRING NOT NULL
	)", params![])?;

	Ok(conn)
}

pub fn gen_id(uid_validity: u32, uid: u32) -> String {
	format!("{}_{}", uid_validity, uid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaildirID {
	pub uid_validity: u32,
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

impl From<i64> for MaildirID {
	fn from(x: i64) -> Self {
		let x = load_i64(x);
		Self::new((x >> 32) as u32, ((x << 32) >> 32) as u32)
	}
}

impl ToSql for MaildirID {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'static>> {
        Ok(ToSqlOutput::from(self.to_i64()))
    }
}

impl FromSql for MaildirID {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let x = i64::column_result(value)?;
		Ok(MaildirID::from(x))
    }
}

impl Display for MaildirID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}_{}", self.uid_validity, self.uid)
    }
}

impl MaildirID {
	pub fn new(uid_validity: u32, uid: u32) -> Self {
		Self {
			uid_validity,
			uid
		}
	}

	pub fn to_u64(&self) -> u64 {
		((self.uid_validity as u64) << 32) | self.uid as u64
	}

	pub fn to_i64(&self) -> i64 {
		store_i64(self.to_u64())
	}

	pub fn to_imap(&self) -> String {
		self.uid.to_string()
	}
}

pub fn maildir_cp(maildir1: &Maildir, maildir2: &Maildir, id1: &str, id2: &str, flags: &str, new: bool) -> Result<()> {
	let name = maildir1.find_filename(id1).context("mail not found")?;
	if new {
		maildir2.store_new_from_path(id2, name)?;
	} else {
		maildir2.store_cur_from_path(id2, flags, name)?;
	}
	Ok(())
}

pub struct EasyMail<'a> {
	mail: Option<ParsedMail<'a>>,
	pub id: MaildirID,
	flags: RwLock<String>,
	from: Option<SingleInfo>,
	from_raw: String,
	pub subject: String,
	pub date: DateTime<Local>,
	pub date_iso: String,
}

impl EasyMail<'_> {
	pub fn new_pseudo(subject: String) -> Self {
		Self {
			mail: None,
			id: MaildirID::new(0, 0),
			flags: "S".to_owned().into(),
			from: None,
			from_raw: String::new(),
			subject,
			date: Local.from_utc_datetime(&NaiveDateTime::from_timestamp(0, 0)),
			date_iso: "????-??-??".to_owned()
		}
	}

	pub fn is_pseudo(&self) -> bool {
		self.mail.is_none()
	}

	pub fn from(&self) -> String {
		if let Some(from) = self.from.as_ref() {
			let name = from.display_name.as_deref().unwrap_or_default();
			if let Some(config) = CONFIG.get() {
				if config.read().browse.show_email_addresses {
					return format!("{} <{}>", name, from.addr);
				}
			}
			name.to_owned()
		} else {
			self.from_raw.clone()
		}
	}

	pub fn has_flag(&self, flag: &Flag) -> bool {
		self.flags.read().contains(imap_flag_to_maildir(flag).unwrap())
	}

	pub fn has_flag2(&self, flag: char) -> bool {
		self.flags.read().contains(flag)
	}

	pub fn add_flag(&self, flag: Flag) {
		self.flags.write().push(imap_flag_to_maildir(&flag).unwrap());
	}

	pub fn add_flag2(&self, flag: char) {
		self.flags.write().push(flag);
	}

	pub fn remove_flag(&self, flag: Flag) {
		self.remove_flag2(imap_flag_to_maildir(&flag).unwrap());
	}

	pub fn remove_flag2(&self, flag: char) {
		let mut f = self.flags.write();
		*f = f.replace(flag, "");
	}

	pub fn mark_as_read(&self, read: bool) {
		if read {
			self.add_flag(Flag::Seen);
			self.remove_flag2(UNREAD);
		} else {
			self.remove_flag(Flag::Seen);
			self.add_flag2(UNREAD);
		}
	}

	pub fn save_flags(&self, maildir: &Maildir) -> Result<()> {
		maildir.set_flags(&self.id.to_string(), &self.flags.read())?;
		Ok(())
	}

	pub fn get_flags(&self) -> String {
		self.flags.read().clone()
	}

	pub fn get_header_values(&self, header: &str) -> Vec<String> {
		self.get_headers().get_all_values(header)
	}
}

impl Debug for EasyMail<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Mail[ID={},Subject={:?}]", self.id.uid, self.subject)
	}
}

impl Display for EasyMail<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.subject)
	}
}

impl PartialEq for EasyMail<'_> {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id && self.from == other.from && self.subject == other.subject
	}
}

impl Eq for EasyMail<'_> {}

impl Hash for EasyMail<'_> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.id.hash(state);
		if let Some(from) = self.from.as_ref() {
			from.display_name.hash(state);
			from.addr.hash(state);
		}
		self.subject.hash(state);
	}
}

impl<'a> Deref for EasyMail<'a> {
	type Target = ParsedMail<'a>;

	fn deref(&self) -> &Self::Target {
		&self.mail.as_ref().unwrap()
	}
}

impl TreeEntry for &EasyMail<'_> {
	fn display(&self, width: usize) -> SpannedString<Style> {
		if self.is_pseudo() {
			return self.subject.clone().into();
		}
		let from = self.from();
		let mut line = self.subject.clone();
		let mut i = width.saturating_sub(1 + from.len() + 1 + self.date_iso.len());
		while i <= line.len() && !line.is_char_boundary(i) {
			if i == 0 {
				break;
			}
			i -= 1;
		}
		line.truncate(i);
		let subj_len = line.len();
		while line.len() < i {
			line.push(' ');
		}
		line.push(' ');
		line += &from;
		line.push(' ');
		line += &self.date_iso;

		let style = if self.has_flag2(DELETE) {
			CONFIG.get().unwrap().read().browse.deleted_style
		} else if self.has_flag(&Flag::Deleted) {
			CONFIG.get().unwrap().read().browse.trashed_style
		} else if !self.has_flag(&Flag::Seen) {
			CONFIG.get().unwrap().read().browse.unread_style
		} else {
			Style::default()
		};
		let spans = vec![
			IndexedSpan {
				content: IndexedCow::Borrowed {
					start: 0,
					end: subj_len
				},
				attr: style,
				width: subj_len
			},
			IndexedSpan {
				content: IndexedCow::Borrowed {
					start: 0,
					end: 0
				},
				attr: style,
				width: line.len() - subj_len - from.len() - self.date_iso.len() - 1
			},
			IndexedSpan {
				content: IndexedCow::Borrowed {
					start: line.len() - self.date_iso.len() - 1 - from.len(),
					end: line.len() - self.date_iso.len() - 1
				},
				attr: style,
				width: from.len()
			},
			IndexedSpan {
				content: IndexedCow::Borrowed {
					start: 0,
					end: 0
				},
				attr: style,
				width: 1
			},
			IndexedSpan {
				content: IndexedCow::Borrowed {
					start: line.len() - self.date_iso.len(),
					end: line.len()
				},
				attr: style,
				width: self.date_iso.len()
			},
		];
		SpannedString::with_spans(&line, spans)
	}
}

pub trait MailExtension {
	fn get_tree_structure<'a>(&'a self, graph: &mut Graph<&'a ParsedMail<'a>, ()>, parent: Option<NodeIndex>);
	fn print_tree_structure(&self, depth: usize, counter: &mut usize);
	fn get_tree_part(&self, counter: &mut usize, target: usize) -> Option<&ParsedMail>;
	fn get_header(&self, header: &str) -> String;
}

impl MailExtension for ParsedMail<'_> {
	fn get_tree_structure<'a>(&'a self, graph: &mut Graph<&'a ParsedMail<'a>, ()>, parent: Option<NodeIndex>) {
		let parent = if parent.is_none() {
			graph.add_node(&self)
		} else {
			parent.unwrap()
		};
		for mail in &self.subparts {
			let new = graph.add_node(mail);
			graph.add_edge(parent, new, ());
			mail.get_tree_structure(graph, Some(new));
		}
	}

	fn print_tree_structure(&self, depth: usize, counter: &mut usize) {
		if depth == 0 {
			println!("{}", self.ctype.mimetype);
		}
		for mail in &self.subparts {
			println!("{}-> {} [{}]", "   ".repeat(depth), mail.ctype.mimetype, counter);
			*counter += 1;
			mail.print_tree_structure(depth + 1, counter);
		}
	}

	fn get_tree_part(&self, counter: &mut usize, target: usize) -> Option<&ParsedMail> {
		for mail in &self.subparts {
			if *counter == target {
				return Some(mail);
			}
			*counter += 1;
			if let Some(x) = mail.get_tree_part(counter, target) {
				return Some(x);
			}
		}
		None
	}

	fn get_header(&self, header: &str) -> String {
		self.get_headers().get_header(header)
	}
}

pub trait HeadersExtension {
	fn message_id(&self, mailbox: &str, id: MaildirID) -> String;
	fn get_header(&self, header: &str) -> String;
}

impl<T: MailHeaderMap + ?Sized> HeadersExtension for T {
	fn message_id(&self, mailbox: &str, id: MaildirID) -> String {
		let mid = self.get_header("Message-ID");
		if mid.is_empty() {
			fallback_mid(mailbox, id)
		} else {
			mid
		}
	}

	fn get_header(&self, header: &str) -> String {
		self.get_all_values(header).join(" ")
	}
}

pub fn fallback_mid(mailbox: &str, id: MaildirID) -> String {
	format!("<{}_{}_{}@no-message-id>", mailbox, id.uid_validity, id.uid)
}

pub trait MaildirExtension {
	fn get_file(&self, name: &str) -> std::result::Result<String, io::Error>;
	fn save_file(&self, name: &str, content: &str) -> std::result::Result<(), io::Error>;
	fn get_mails<'a>(&self, entries: &'a mut [MailEntry]) -> Result<Vec<EasyMail<'a>>>;
	fn get_mails2<'a>(&self, entries: &'a mut [&'a mut MailEntry]) -> Result<Vec<EasyMail<'a>>>;
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
			let from_raw = headers.get_all_values("From").join(" ");
			let from = addrparse(&from_raw).map(|x| x.extract_single_info()).ok().flatten();
			let subject = headers.get_all_values("Subject").join(" ");
			let date = headers.get_all_values("Date").join(" ");
			let date = dateparse(&date).map(|x|
				Local.from_utc_datetime(&NaiveDateTime::from_timestamp(x, 0))
			)?;
			mails.push(EasyMail {
				mail: Some(mail),
				flags: flags.into(),
				id,
				from,
				from_raw,
				subject,
				date_iso: date.format("%Y-%m-%d %H:%M").to_string(),
				date,
			});
		}
		Ok(mails)
	}

	// TODO this should be unified with the above
	fn get_mails2<'a>(&self, entries: &'a mut [&'a mut MailEntry]) -> Result<Vec<EasyMail<'a>>> {
		let mut mails = Vec::new();
		for maile in entries {
			let id = maile.id().try_into()?;
			let flags = maile.flags().to_owned();
			let mail = maile.parsed()?;
			let headers = mail.get_headers();
			let from_raw = headers.get_all_values("From").join(" ");
			let from = addrparse(&from_raw).map(|x| x.extract_single_info()).ok().flatten();
			let subject = headers.get_all_values("Subject").join(" ");
			let date = headers.get_all_values("Date").join(" ");
			let date = dateparse(&date).map(|x|
				Local.from_utc_datetime(&NaiveDateTime::from_timestamp(x, 0))
			)?;
			mails.push(EasyMail {
				mail: Some(mail),
				flags: flags.into(),
				id,
				from,
				from_raw,
				subject,
				date_iso: date.format("%Y-%m-%d %H:%M").to_string(),
				date,
			});
		}
		Ok(mails)
	}
}

#[deprecated]
pub fn store_i64(x: u64) -> i64 {
	unsafe { std::mem::transmute(x) }
}

#[deprecated]
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

pub fn load_config() {
	CONFIG.get_or_init(|| {
		let config = Config::load_from_fs();
		let cfg = match config {
			Ok(config) => if let Some(config) = config {
				config.into()
			} else {
				Config::default().into()
			},
			Err(e) => panic!("failed to load configuration: {:?}", e)
		};
		info!("config {:?}", cfg);
		cfg
	});
}

pub static CONFIG: OnceCell<RwLock<Config>> = OnceCell::new();

#[derive(Deserialize, Serialize, Debug)]
pub struct Config {
	#[serde(default)]
	pub browse: Browse
}

fn get_paths() -> Result<ProjectDirs> {
	Ok(directories_next::ProjectDirs::from("", "", "Inboxid").context("unable to determine configuration directory")?)
}

fn get_config_path() -> Result<PathBuf> {
	let paths = get_paths()?;
	Ok(paths.config_dir().join("config.toml"))
}

impl Config {
	fn load_from_fs() -> Result<Option<Self>> {
		let config = get_config_path()?;
		if config.exists() {
			let content = fs::read_to_string(&config)?;
			Ok(Some(toml::from_str(&content)?))
		} else {
			Ok(None)
		}
	}

	pub fn save(&self) -> Result<()> {
		let config = get_config_path()?;
		fs::create_dir_all(config.parent().unwrap())?;
		fs::write(config, toml::to_string(&self)?)?;
		Ok(())
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			browse: Browse::default()
		}
	}
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Browse {
	#[serde(default)]
	pub show_email_addresses: bool,
	#[serde(default = "default_unread_style")]
	#[serde(deserialize_with = "deserialize_style")]
	#[serde(serialize_with = "serialize_style")]
	pub unread_style: Style,
	#[serde(default = "default_trashed_style")]
	#[serde(deserialize_with = "deserialize_style")]
	#[serde(serialize_with = "serialize_style")]
	pub trashed_style: Style,
	#[serde(default = "default_deleted_style")]
	#[serde(deserialize_with = "deserialize_style")]
	#[serde(serialize_with = "serialize_style")]
	pub deleted_style: Style,
	#[serde(default)]
	pub base_save_path: PathBuf,
}

impl Default for Browse {
	fn default() -> Self {
		Self {
			show_email_addresses: Default::default(),
			unread_style: default_unread_style(),
			trashed_style: default_trashed_style(),
			deleted_style: default_deleted_style(),
			base_save_path: directories_next::UserDirs::new().expect("no user dirs").download_dir().expect("no download directory").to_owned()
		}
	}
}

pub fn style_to_str(x: &Style) -> &'static str {
	match x.effects.iter().next() {
		Some(x) => match x {
			Effect::Simple => "simple",
			Effect::Reverse => "reverse",
			Effect::Bold => "bold",
			Effect::Italic => "italic",
			Effect::Strikethrough => "strikethrough",
			Effect::Underline => "underline",
			Effect::Blink => "blink"
		},
		None => "none"
	}
}

fn serialize_style<S>(x: &Style, s: S) -> std::result::Result<S::Ok, S::Error> where S: Serializer {
	s.serialize_str(style_to_str(x))
}

fn deserialize_style<'de, D>(de: D) -> std::result::Result<Style, D::Error> where D: Deserializer<'de> {
	struct StrVisitor;
	impl<'de> Visitor<'de> for StrVisitor {
		type Value = Style;

		fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
			formatter.write_str("style specification")
		}

		fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
			parse_effect(v).map(Into::into).ok_or(serde::de::Error::invalid_value(serde::de::Unexpected::Str(v), &self))
		}
	}
	let vis = StrVisitor;
	de.deserialize_str(vis)
}

pub fn parse_effect(effect: &str) -> Option<Effect> {
	match effect {
		"simple" => Some(Effect::Simple),
		"reverse" => Some(Effect::Reverse),
		"bold" => Some(Effect::Bold),
		"italic" => Some(Effect::Italic),
		"strikethrough" => Some(Effect::Strikethrough),
		"underline" => Some(Effect::Underline),
		"blink" => Some(Effect::Blink),
		_ => None
	}
}

fn default_unread_style() -> Style {
	Effect::Reverse.into()
}

fn default_trashed_style() -> Style {
	let mut color = ColorStyle::primary();
	color.front = ColorType::Color(Color::Light(BaseColor::Black));
	color.into()
}

fn default_deleted_style() -> Style {
	Effect::Strikethrough.into()
}

pub fn imap_flags_to_maildir(mut f: String, flags: &[Flag]) -> String {
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
	f
}

pub fn imap_flag_to_maildir(flag: &Flag) -> Option<char> {
	match flag {
		Flag::Seen => Some('S'),
		Flag::Answered => Some('R'),
		Flag::Flagged => Some('F'),
		Flag::Deleted => Some('T'),
		_ => None
	}
}

pub fn maildir_flags_to_imap(flags: &str) -> Vec<Flag> {
	let mut x = vec![];
	for c in flags.chars() {
		if let Some(f) = match c {
			REPLIED => Some(Flag::Answered),
			SEEN => Some(Flag::Seen),
			FLAGGED => Some(Flag::Flagged),
			TRASHED => Some(Flag::Deleted),
			_ => None
		} {
			x.push(f);
		}
	}
	x
}

pub fn imap_flags_to_cmd(flags: &[Flag]) -> String {
	let mut x = "(".to_owned();
	for f in flags {
		x += &f.to_string();
		x.push(' ');
	}
	if x.ends_with(' ') {
		x.pop();
	}
	x.push(')');
	x
}
