use std::{collections::HashMap, env};

use ascii_table::{Align, AsciiTable, Column};
use chrono::{Local, NaiveDateTime, TimeZone};
use inboxid::*;
use itertools::Itertools;
use mailparse::{MailHeaderMap, dateparse};
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

	let mut rows = Vec::new();
	let mut mail_list = Vec::new();
	let mut i = 0;
	// TODO(refactor) merge with new
	let mut list = maildir.list_cur_sorted(Box::new(|name| {
		// sort by UID
		name.splitn(2, '_').nth(1).map(|x| x.parse().unwrap_or(0)).unwrap_or(0)
	})).collect_vec();
	let list = list.iter_mut().map(
		|x| x.as_mut().map(|x| (x.flags().to_owned(), x.id().to_owned(), x.parsed()))).collect_vec();
	for maile in &list {
		match maile {
			Ok((flags, id, Ok(mail))) => {
				let headers = mail.get_headers();
				let from = headers.get_all_values("From").join(" ");
				let subj = headers.get_all_values("Subject").join(" ");
				let date = headers.get_all_values("Date").join(" ");
				let date = dateparse(&date).map(|x| {
					let dt = Local.from_utc_datetime(&NaiveDateTime::from_timestamp(x, 0));
					dt.format("%Y-%m-%d %H:%M").to_string()
				}).unwrap_or(date);
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
				rows.push(vec![i.to_string(), flags_display, from, subj, date]);
				i += 1;
				mail_list.push((flags, id, mail));
			}
			Ok((_, _, Err(e))) => {
				println!("error parsing mail: {:?}", e);
			}
			Err(e) => {
				println!("error: {:?}", e);
			}
		}
	}
	rows.sort_unstable_by(|x, y| x[4].cmp(&y[4]));
	let count = rows.len();
	let mut mails = HashMap::new();
	for (i, row) in rows.iter_mut().enumerate() {
		mails.insert(count - i, &mail_list[row[0].parse::<usize>().unwrap()]);
		row[0] = (count - i).to_string();
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
				let mail = &mails[&line.trim().parse::<usize>().unwrap()];
				println!("{}", std::str::from_utf8(&mail.2.get_body_raw().unwrap()).unwrap());
				for x in &mail.2.subparts {
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
