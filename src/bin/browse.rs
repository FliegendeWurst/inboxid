#![feature(internal_output_capture)]

use std::{array::IntoIter, cell::RefCell, cmp, collections::{HashMap, HashSet}, env, fmt::Display, io, sync::{Arc, Mutex}};

use cursive::{Cursive, CursiveExt, Vec2};
use cursive::event::{Event, Key};
use cursive::traits::Identifiable;
use cursive::view::{Scrollable, SizeConstraint, View};
use cursive::views::{Checkbox, LinearLayout, OnEventView, Panel, ResizedView, TextView};
use cursive_tree_view::{Placement, TreeEntry, TreeView};
use inboxid::*;
use io::Write;
use itertools::Itertools;
use log::error;
use mailparse::{MailHeaderMap, ParsedMail};
use parking_lot::RwLock;
use petgraph::{EdgeDirection, graph::{DiGraph, NodeIndex}, visit::{Dfs, IntoNodeReferences}};

fn main() -> Result<()> {
	load_config();
	let sink = Arc::new(Mutex::new(Vec::new()));
	std::io::set_output_capture(Some(sink.clone()));
	let result = std::panic::catch_unwind(|| {
		let args = env::args().collect_vec();
		if args.len() > 1 {
			show_listing(&args[1])
		} else {
			show_listing("INBOX")
		}
	});
	match result {
		Ok(res) => res,
		Err(_) => {
			if let Err(e) = io::stderr().lock().write_all(&sink.lock().unwrap()) {
				println!("{:?}", e);
			}
			Err("panicked".into()) // not displayed
		}
	}
}

fn show_listing(mailbox: &str) -> Result<()> {
	let maildir = get_maildir(mailbox)?;

	let mut mails = Vec::new();
	for x in maildir.list_cur() {
		mails.push(x?);
	}
	let mails = Box::leak(Box::new(mails.into_iter().map(Box::new).map(Box::leak).collect_vec()));
	let mut mails = maildir.get_mails2(mails)?;
	mails.sort_by_key(|x| x.date);
	let mails = Box::leak(Box::new(mails.into_iter().map(Box::new).map(Box::leak).collect_vec()));
	
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
		rows.push(IntoIter::new([(mails.len() - i).to_string(), flags_display, mail.from(), mail.subject.clone(), mail.date_iso.clone()]));
	}

	let mut mails_by_id = HashMap::new();
	let mut threads: HashMap<_, Vec<_>> = HashMap::new();
	for i in 0..mails.len() {
		let mail = &*mails[i];
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
	for i in 0..mails.len() {
		let mail = &*mails[i];
		let node = graph.add_node(mail);
		nodes.insert(mail, node);
		nodes_inv.insert(node, mail);
	}
	for i in 0..mails.len() {
		let mail = &*mails[i];
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

	let mut siv = Cursive::new();

	let tree = RefCell::new(TreeView::new());
	// recursive lambda
	struct PrintThread<'a> {
		f: &'a dyn Fn(&PrintThread, NodeIndex, Placement, usize)
	}
	let print_thread = |this: &PrintThread, node, placement, parent| {
		let mail = nodes_inv[&node];
		if mails_printed.borrow().contains(mail) && placement == Placement::After {
			return;
		}
		//println!("{}{}", "   ".repeat(depth), mail.subject);
		let entry = tree.borrow_mut().insert_item(mail, placement, parent);
		mails_printed.borrow_mut().insert(mail);
		let mut replies = graph.neighbors_directed(node, EdgeDirection::Outgoing).collect_vec();
		replies.sort_unstable_by_key(|&idx| {
			let mut maximum = &nodes_inv[&idx].date;
			let mut dfs = Dfs::new(&graph, idx);
			while let Some(idx) = dfs.next(&graph) {
				let other = &nodes_inv[&idx];
				maximum = cmp::max(maximum, &other.date);
			}
			maximum
		});
		for r in replies {
			(this.f)(this, r, Placement::LastChild, entry.unwrap());
		}
	};
	let print_thread = PrintThread { f: &print_thread };

	let mut x = tree.borrow().len();
	for root in roots {
		let y = tree.borrow().len();
		(print_thread.f)(&print_thread, root.0, Placement::After, x);
		x = y
	}

	let mut tree = tree.into_inner();
	tree.set_on_select(|siv, row| {
		let item = siv.call_on_name("tree", |tree: &mut TreeView<&EasyMail>| {
			*tree.borrow_item(row).unwrap()
		}).unwrap();
		if item.is_pseudo() {
			return;
		}
		let mut mail_struct = DiGraph::new();
		item.get_tree_structure(&mut mail_struct, None);
		if let Some(mail) = siv.call_on_name("part_select", |view: &mut TreeView<MailPart>| {
			view.clear();
			let mut part_to_display = None;
			let mut idx_select = 0;
			let mut idxes = HashMap::new();
			let mut i = 0;
			for idx in mail_struct.node_indices() {
				let part = mail_struct[idx];
				let mime = &part.ctype.mimetype;
				let incoming = mail_struct.neighbors_directed(idx, EdgeDirection::Incoming).next();
				let tree_idx = if let Some(parent) = incoming {
					let parent_idx = idxes[&parent];
					let tree_idx = view.insert_item(MailPart::from(part), Placement::LastChild, parent_idx).unwrap();
					tree_idx
				} else {
					let tree_idx = view.insert_item(MailPart::from(part), Placement::After, i).unwrap();
					i = tree_idx;
					tree_idx
				};
				idxes.insert(idx, tree_idx);
				if mime.starts_with("text/") {
					if part_to_display.is_none() {
						part_to_display = Some(part);
						idx_select = tree_idx;
					} else if mime == "text/plain" {
						if let Some(part) = part_to_display.as_ref() {
							if part.ctype.mimetype != "text/plain" {
								part_to_display = Some(part);
								idx_select = tree_idx;
							}
						}
					}
				}
			}
			if part_to_display.is_some() {
				view.set_selected_row(idx_select);
			}
			part_to_display
		}).unwrap() {
			siv.call_on_name("mail", |view: &mut TextView| {
				view.set_content(mail.get_body().unwrap());
			});
		}
		siv.call_on_name("mail_info", |view: &mut MailInfoView| {
			view.set(item);
		});
	});
	tree.set_on_submit(|siv, _row| {
		siv.focus_name("mail").unwrap();
	});
	let tree = tree.with_name("tree").scrollable();
	let tree_resized = ResizedView::new(SizeConstraint::AtMost(120), SizeConstraint::Free, tree);
	let mail_info = MailInfoView::new().with_name("mail_info");
	let mail_content = TextView::new("").with_name("mail").scrollable();
	let mut mail_part_select = TreeView::<MailPart>::new();
	mail_part_select.set_on_select(|siv, row| {
		let item = siv.call_on_name("part_select", |tree: &mut TreeView<MailPart>| {
			tree.borrow_item(row).unwrap().part
		}).unwrap();
		siv.call_on_name("mail", |view: &mut TextView| {
			view.set_content(item.get_body().unwrap());
		});
	});
	mail_part_select.set_on_submit(|siv, _row| {
		siv.focus_name("mail").unwrap();
	});
	let mail_wrapper = LinearLayout::vertical()
		.child(ResizedView::with_full_width(Panel::new(mail_info).title("Mail")))
		.child(ResizedView::with_full_height(mail_content))
		.child(Panel::new(mail_part_select.with_name("part_select"))
			.title("Multipart selection"));
	let mail_content_resized = ResizedView::new(SizeConstraint::Full, SizeConstraint::Free, mail_wrapper);
	let main = LinearLayout::horizontal()
		.child(tree_resized)
		.child(mail_content_resized);
	siv.add_fullscreen_layer(ResizedView::with_full_screen(main));

	let mut setup = LinearLayout::vertical();
	{
	let config = CONFIG.get().unwrap().read();
	let show_email_addresses = Checkbox::new()
		.with_checked(config.browse.show_email_addresses)
		.on_change(|_siv, checked| {
		CONFIG.get().unwrap().write().browse.show_email_addresses = checked;
	});
	setup.add_child(
		LinearLayout::horizontal()
		.child(show_email_addresses)
		.child(TextView::new(" Show email addresses"))
	);
	}
	// most horrible hack
	let setup: Arc<RwLock<Option<Box<dyn View>>>> = Arc::new(RwLock::new(Some(Box::new(ResizedView::new(SizeConstraint::Full, SizeConstraint::Full, setup)))));
	let setup2 = Arc::clone(&setup);
	let setup_view: ResizedView<LinearLayout> = *setup.write().take().unwrap().as_boxed_any().downcast().unwrap();
	let setup_view = OnEventView::new(setup_view)
		.on_event(Event::Key(Key::F10), move |s| {
		let setup = s.pop_layer().unwrap();
		*setup2.write() = Some(setup);
		if let Err(e) = CONFIG.get().unwrap().read().save() {
			error!("failed to save config {:?}", e);
		}
	});
	*setup.write() = Some(Box::new(setup_view));

	let setup2 = Arc::clone(&setup);
	siv.add_global_callback(Event::Key(Key::F2), move |s| {
		let setup = setup2.write().take().unwrap();
		s.add_fullscreen_layer(setup);
	});

	siv.add_global_callback('q', |s| s.quit());

	siv.run();

	Ok(())
}

#[derive(Debug)]
struct MailPart {
	part: &'static ParsedMail<'static>
}

impl Display for MailPart {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.part.ctype.mimetype)
	}
}

impl From<&'static ParsedMail<'static>> for MailPart {
	fn from(part: &'static ParsedMail<'static>) -> Self {
		Self {
			part
		}
	}
}

impl TreeEntry for MailPart {}

struct MailInfoView {
	email: Option<&'static ParsedMail<'static>>
}

impl MailInfoView {
	fn new() -> Self {
		Self {
			email: None
		}
	}

	fn set(&mut self, mail: &'static ParsedMail<'static>) {
		self.email = Some(mail);
	}
}

const HEADERS_TO_DISPLAY: &[&str] = &["From", "Subject", "To"];

impl View for MailInfoView {
    fn draw(&self, printer: &cursive::Printer) {
		if let Some(mail) = self.email {
			let mut y = 0;
			for header in HEADERS_TO_DISPLAY {
				let mut x = 0;
				printer.print((x, y), header);
				x += header.len(/* ASCII-only */);
				printer.print((x, y), ": ");
				x += 2;
        		printer.print((x, y), &mail.headers.get_all_values(header).join(" "));
				y += 1;
			}
		}
    }

	fn required_size(&mut self, _constraint: Vec2) -> Vec2 {
		(42, HEADERS_TO_DISPLAY.len()).into()
	}
}
