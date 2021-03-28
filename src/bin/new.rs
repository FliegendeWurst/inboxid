use ascii_table::{Align, AsciiTable, Column};
use chrono::{Local, NaiveDateTime, TimeZone};
use mailparse::{MailHeaderMap, dateparse};

use inboxid::*;

fn main() -> Result<()> {
	show_listing("INBOX")
}

fn show_listing(mailbox: &str) -> Result<()> {
	let maildir = get_maildir(mailbox)?;

	let mut rows = Vec::new();
	for mut mail in maildir.list_new_sorted(Box::new(|name| {
		// sort by UID
		name.splitn(2, '_').nth(1).map(|x| x.parse().unwrap_or(0)).unwrap_or(0)
	})) {
		match mail.as_mut().map(|x| x.parsed()) {
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
    ascii_table.print(rows);

	Ok(())
}
