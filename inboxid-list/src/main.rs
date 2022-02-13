use std::{array::IntoIter, collections::HashSet, env, fs};

use ascii_table::{Align, AsciiTable, Column};
use inboxid_lib::*;
use itertools::Itertools;
use mailparse::ParsedMail;
use rustyline::{Editor, error::ReadlineError};

fn main() -> Result<()> {
	let args = env::args().collect_vec();
	if args.len() > 1 {
		show_listing(&args[1])
	} else {
		show_listing("INBOX")
	}
}

fn show_listing(mailbox: &str) -> Result<()> {
	let maildir = get_maildir(mailbox)?;

	let mut mails = Vec::new();
	for x in maildir.list_cur() {
		mails.push(x?);
	}
	let mut mails = maildir.get_mails(&mut mails)?;
	mails.sort_by_key(|x| x.date);
	
	let mut rows = Vec::new();
	for (i, mail) in mails.iter().enumerate() {
		let flags = &mail.get_flags();
		let mut flags_display = String::new();
		if flags.contains('F') {
			flags_display.push('+');
		}
		if flags.contains('R') {
			flags_display.push('R');
		}
		if flags.contains('S') {
			flags_display.push(' ');
		} else {
			flags_display.push('*');
		}
		rows.push(IntoIter::new([(mails.len() - i).to_string(), flags_display, mail.from(), mail.subject.clone(), mail.date_iso.clone()]));
	}

	let mut ascii_table = AsciiTable::default();
	ascii_table.draw_lines = false;
	ascii_table.max_width = usize::MAX;
	for (i, &(header, align)) in [
		("i", Align::Right),
		("---", Align::Right),
		("From", Align::Left),
		("Subject", Align::Left),
		("Date", Align::Left),
	].iter().enumerate() {
		let mut column = Column::default();
		column.header = header.to_owned();
		column.align = align;
		column.max_width = usize::MAX;
		ascii_table.columns.insert(i, column);
	}
	ascii_table.print(rows); // prints a 0 if empty :)

	if mails.is_empty() {
		return Ok(());
	}
	let mut rl = Editor::<()>::new();
	let mut state = Initial;
	let mut to_delete = HashSet::new();
	loop {
		let readline = rl.readline(&match state {
			Initial => ">> ".to_owned(),
			MailSelected(x) => format!("{} >> ", mails.len() - x),
			AwaitingSave(_, _) => "out? >> ".to_owned()
		});
		match readline {
			Ok(line) => {
				let input_idx = line.trim().parse::<usize>();
				match state {
					Initial => {
						if let Ok(idx) = input_idx {
							let idx = mails.len() - idx;
							let mail = &mails[idx];
							if mail.ctype.mimetype.starts_with("text/") {
								let raw_body = mail.get_body_raw();
								let content = std::str::from_utf8(raw_body.as_deref().unwrap())?;
								moins::Moins::run(content, None);
							} else if mail.ctype.mimetype.starts_with("multipart/") {
								mail.print_tree_structure(0, &mut 1);
								state = MailSelected(idx);
							} else {
								state = AwaitingSave(&*mail, None);
							}
							continue;
						}
					},
					MailSelected(mail_idx) => {
						let mail = &mails[mail_idx];
						if let Ok(idx) = input_idx {
							let part = mail.get_tree_part(&mut 1, idx).unwrap();
							if part.ctype.mimetype.starts_with("text/") {
								let raw_body = part.get_body_raw();
								let content = std::str::from_utf8(raw_body.as_deref().unwrap())?;
								moins::Moins::run(content, None);
							} else {
								state = AwaitingSave(part, Some(mail_idx));
							}
							continue;
						} else if line.is_empty() {
							state = Initial;
							continue;
						}
					},
					AwaitingSave(mail, idx) => {
						if line == "open" {
							let path = if let Some(ext) = mime2ext::mime2ext(&mail.ctype.mimetype) {
								format!("/tmp/mail_content.{}", ext)
							} else {
								"/tmp/mail_content".to_owned()
							};
							fs::write(&path, &mail.get_body_raw()?)?;
							let mut p = subprocess::Popen::create(&["xdg-open", &path], Default::default())?;
							p.wait()?;
							to_delete.insert(path);
							state = if let Some(idx) = idx {
								MailSelected(idx)
							} else {
								Initial
							};
							continue;
						}
					}
				}
			},
			Err(ReadlineError::Interrupted) => {
				break
			},
			Err(ReadlineError::Eof) => {
				break
			},
			Err(err) => {
				println!("Error: {:?}", err);
				break
			}
		}
		println!("unknown command!");
	}

	for x in to_delete {
		let _ = fs::remove_file(x);
	}

	Ok(())
}

enum State<'a> {
	Initial,
	MailSelected(usize),
	AwaitingSave(&'a ParsedMail<'a>, Option<usize>)
}

use State::*;
