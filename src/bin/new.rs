use std::env;

use ascii_table::{Align, AsciiTable, Column};
use chrono::{Local, NaiveDateTime, TimeZone};
use itertools::Itertools;
use mailparse::{MailHeaderMap, dateparse};

use inboxid::*;

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
	let mut seen = Vec::new();
	for mut maile in maildir.list_new_sorted(Box::new(|name| {
		// sort by UID
		name.splitn(2, '_').nth(1).map(|x| x.parse().unwrap_or(0)).unwrap_or(0)
	})) {
		match maile.as_mut().map(|x| x.parsed()) {
		    Ok(Ok(mail)) => {
				let headers = mail.get_headers();
				let from = headers.get_all_values("From").join(" ");
				let subj = headers.get_all_values("Subject").join(" ");
				let date = headers.get_all_values("Date").join(" ");
				let date = dateparse(&date).map(|x| {
					let dt = Local.from_utc_datetime(&NaiveDateTime::from_timestamp(x, 0));
					dt.format("%Y-%m-%d %H:%M").to_string()
				}).unwrap_or(date);
				rows.push(vec![from, subj, date]);
				seen.push(maile.as_ref().unwrap().id().to_owned());
			}
		    Ok(Err(e)) => {
				println!("error parsing mail: {:?}", e);
			}
			Err(e) => {
				println!("error: {:?}", e);
			}
		}
	}

	let mut ascii_table = AsciiTable::default();
    ascii_table.draw_lines = false;
	ascii_table.max_width = usize::MAX;
    for (i, &(header, align)) in [
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

	// only after the user saw the new mail, move it out of 'new'
	for seen in seen {
		maildir.move_new_to_cur(&seen)?;
	}

	Ok(())
}
