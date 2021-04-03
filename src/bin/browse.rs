use std::{array::IntoIter, cell::RefCell, cmp, collections::{HashMap, HashSet}, env};

use inboxid::*;
use itertools::Itertools;
use mailparse::ParsedMail;
use petgraph::{EdgeDirection, graph::{DiGraph, NodeIndex}, visit::{Dfs, IntoNodeReferences}};

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

	let mut mails_by_id = HashMap::new();
	let mut threads: HashMap<_, Vec<_>> = HashMap::new();
	for mail in &mails {
		let mid = mail.get_header("Message-ID");
		threads.entry(mid.clone()).or_default().push(mail);
		if mails_by_id.insert(mid, mail).is_some() {
			println!("error: missing/duplicate Message-ID");
			return Ok(());
		}
		for value in mail.get_header_values("References") {
			for mid in value.split(' ').map(ToOwned::to_owned) {
				threads.entry(mid).or_default().push(mail);
			}
		}
		for value in mail.get_header_values("In-Reply-To") {
			for mid in value.split(' ').map(ToOwned::to_owned) {
				threads.entry(mid).or_default().push(mail);
			}
		}
	}
	let mut threads = threads.into_iter().collect_vec();
	threads.sort_unstable_by_key(|(_, mails)| mails.len());
	threads.reverse();
	let mut graph = DiGraph::new();
	let mut nodes = HashMap::new();
	let mut nodes_inv = HashMap::new();
	for mail in &mails {
		let node = graph.add_node(mail);
		nodes.insert(mail, node);
		nodes_inv.insert(node, mail);
	}
	for mail in &mails {
		for value in mail.get_header_values("In-Reply-To") {
			for mid in value.split(' ').map(ToOwned::to_owned) {
				if let Some(other_mail) = mails_by_id.get(&mid) {
					graph.add_edge(nodes[other_mail], nodes[mail], ());
				} else {
					let pseudomail = Box::leak(Box::new(EasyMail::new_pseudo(mid.clone())));
					let node = graph.add_node(pseudomail);
					nodes.insert(pseudomail, node);
					nodes_inv.insert(node, pseudomail);
					graph.add_edge(node, nodes[mail], ());
					mails_by_id.insert(mid, pseudomail);
				}
			}
		}
	}
	let mut roots = graph.node_references().filter(|x| graph.neighbors_directed(x.0, EdgeDirection::Incoming).count() == 0).collect_vec();
	roots.sort_unstable_by_key(|x| x.1.date);
	let mails_printed = RefCell::new(HashSet::new());

	// recursive lambda
	struct PrintThread<'a> {
		f: &'a dyn Fn(&PrintThread, NodeIndex, usize)
	}
	let print_thread = |this: &PrintThread, node, depth| {
		let mail = nodes_inv[&node];
		if mails_printed.borrow().contains(mail) && depth == 0 {
			return;
		}
		println!("{}{}", "   ".repeat(depth), mail.subject);
		mails_printed.borrow_mut().insert(mail);
		let mut replies = graph.neighbors_directed(node, EdgeDirection::Outgoing).collect_vec();
		replies.sort_unstable_by_key(|&idx| {
			let mut maximum = &nodes_inv[&idx].date;
			let mut dfs = Dfs::new(&graph, idx);
			while let Some(idx) = dfs.next(&graph) {
				let other = nodes_inv[&idx];
				maximum = cmp::max(maximum, &other.date);
			}
			maximum
		});
		for r in replies {
			(this.f)(this, r, depth + 1);
		}
	};
	let print_thread = PrintThread { f: &print_thread };

	for root in roots {
		(print_thread.f)(&print_thread, root.0, 0);
	}

	Ok(())
}
