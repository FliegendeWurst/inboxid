use std::{array::IntoIter, env};

use ascii_table::{Align, AsciiTable, Column};
use inboxid::*;
use itertools::Itertools;
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
		let flags = &mail.flags;
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
		rows.push(IntoIter::new([(mails.len() - i).to_string(), flags_display, mail.from.clone(), mail.subject.clone(), mail.date_iso.clone()]));
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
	loop {
		let readline = rl.readline(">> ");
		match readline {
			Ok(line) => {
				let idx = mails.len() - line.trim().parse::<usize>().unwrap();
				let mail = &mails[idx];
				println!("{}", std::str::from_utf8(&mail.get_body_raw().unwrap()).unwrap());
				for x in &mail.subparts {
					if x.ctype.mimetype == "text/html" {
						continue; // TODO
					}
					let mut content = x.get_body().unwrap();
					moins::Moins::run(&mut content, None);
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
	}

	Ok(())
}
