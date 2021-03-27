use ascii_table::{Align, AsciiTable, Column};
use mailparse::MailHeaderMap;

use inboxid::*;

fn main() -> Result<()> {
	show_listing("INBOX")
}

fn show_listing(mailbox: &str) -> Result<()> {
	let maildir = get_maildir(mailbox)?;

	let mut rows = Vec::new();
	for mail in maildir.list_new_sorted(Box::new(|name| {
		// sort by UID
		name.splitn(2, '_').nth(1).map(|x| x.parse().unwrap_or(0)).unwrap_or(0)
	})) {
		match mail {
		    Ok(mut mail) => {
				let mail = mail.parsed()?;
				let headers = mail.get_headers();
				let from = headers.get_all_values("From").join(" ");
				let subj = headers.get_all_values("Subject").join(" ");
				rows.push(vec![from, subj]);
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
