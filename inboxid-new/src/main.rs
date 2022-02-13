use std::{array::IntoIter, env};

use ascii_table::{Align, AsciiTable, Column};
use itertools::Itertools;

use inboxid_lib::*;

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
	for x in maildir.list_new() {
		mails.push(x?);
	}
	let mut mails = maildir.get_mails(&mut mails)?;
	mails.sort_by_key(|x| x.id);

	let mut rows = Vec::new();
	for mail in &mails {
		rows.push(IntoIter::new([mail.from(), mail.subject.clone(), mail.date_iso.clone()]));
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
	for seen in mails {
		maildir.move_new_to_cur(&seen.id.to_string())?;
	}

	Ok(())
}
