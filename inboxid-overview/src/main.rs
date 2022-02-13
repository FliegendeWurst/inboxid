use std::array::IntoIter;

use ascii_table::{AsciiTable, Align, Column};
use inboxid_lib::*;

fn main() -> Result<()> {
	let mut dirs = get_maildirs()?;
	dirs.sort_unstable();
	let mut rows = vec![];
	for dir in dirs {
		let maildir = get_maildir(&dir)?;
		let unread = maildir
			.list_cur()
			.map(|x| if x.map(|x| !x.flags().contains(SEEN)).unwrap_or(true) { 1 } else { 0 })
			.sum::<usize>();
		if unread > 0 {
			rows.push(IntoIter::new([dir, unread.to_string()]));
		}
	}
	let mut ascii_table = AsciiTable::default();
	ascii_table.draw_lines = false;
	ascii_table.max_width = usize::MAX;
	for (i, &(header, align)) in [
		("Mailbox", Align::Left),
		("Unread", Align::Right),
	].iter().enumerate() {
		let mut column = Column::default();
		column.header = header.to_owned();
		column.align = align;
		column.max_width = usize::MAX;
		ascii_table.columns.insert(i, column);
	}
	ascii_table.print(rows); // prints a 0 if empty :)
	Ok(())
}