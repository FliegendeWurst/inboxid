#![feature(internal_output_capture)]

use std::{cell::RefCell, cmp, collections::{HashMap, HashSet}, env, fmt::Display, io, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use std::result::Result as StdResult;

use cursive::{Cursive, Vec2, WrapMethod, traits::Boxable, view::ViewWrapper, views::{Dialog, EditView}};
use cursive::align::HAlign;
use cursive::event::{Event, Key};
use cursive::traits::Identifiable;
use cursive::view::{Scrollable, SizeConstraint, View};
use cursive::views::{Checkbox, LinearLayout, NamedView, OnEventView, Panel, ResizedView, ScrollView, SelectView, TextView};
use cursive_tree_view::{Placement, TreeEntry, TreeView};
use inboxid_lib::*;
use io::Write;
use itertools::Itertools;
use log::error;
use mailparse::{MailHeaderMap, ParsedMail};
use parking_lot::{Mutex, RwLock};
use petgraph::{EdgeDirection, graph::{DiGraph, NodeIndex}, visit::{Dfs, IntoNodeReferences}};
use rusqlite::params;

fn main() -> Result<()> {
	load_config();
	let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
	std::io::set_output_capture(Some(sink.clone()));
	let result = std::panic::catch_unwind(|| {
		let args = env::args().collect_vec();
		if args.len() > 1 {
			show_listing(&args[1])
		} else {
			show_listing("INBOX")
		}
	});
	if let Err(e) = io::stderr().lock().write_all(&sink.lock().unwrap()) {
		println!("{:?}", e);
	}
	match result {
		Ok(res) => res,
		Err(_) => {
			Err("panicked".into()) // not displayed
		}
	}
}

fn show_listing(mailbox: &str) -> Result<()> {
	let db = Box::leak(Box::new(get_db()?));
	let update_flags = Arc::new(Mutex::new(db.prepare("UPDATE mail SET flags = ? WHERE uid = ?")?));
	let maildir = Box::leak(Box::new(get_maildir(mailbox)?));
	let maildir = &*maildir;

	let mut mails = Vec::new();
	for x in maildir.list_cur() {
		mails.push(x?);
	}
	let mails = Box::leak(Box::new(mails.into_iter().map(Box::new).map(Box::leak).collect_vec()));
	let mut mails = maildir.get_mails2(mails)?;
	mails.sort_by_key(|x| x.date);
	let mails = Box::leak(Box::new(mails.into_iter().map(Box::new).map(Box::leak).collect_vec()));

	let mut mails_by_id = HashMap::new();
	let mut threads: HashMap<_, Vec<_>> = HashMap::new();
	for i in 0..mails.len() {
		let mail = &*mails[i];
		let mid = mail.get_headers().message_id(mailbox, mail.id);
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
			for mid in value.split(' ') {
				if let Some(other_mail) = mails_by_id.get(mid) {
					graph.add_edge(nodes[other_mail], nodes[mail], ());
				} else {
					let pseudomail = Box::leak(Box::new(EasyMail::new_pseudo(mid.to_owned())));
					let node = graph.add_node(pseudomail);
					nodes.insert(pseudomail, node);
					nodes_inv.insert(node, pseudomail);
					graph.add_edge(node, nodes[mail], ());
					mails_by_id.insert(mid.to_owned(), pseudomail);
				}
			}
		}
	}
	let mut roots = graph.node_references().filter(|x| graph.neighbors_directed(x.0, EdgeDirection::Incoming).count() == 0).collect_vec();
	roots.sort_by_cached_key(|&(idx, mail)| {
		let mut maximum = mail.date;
		let mut dfs = Dfs::new(&graph, idx);
		while let Some(idx) = dfs.next(&graph) {
			let other = &nodes_inv[&idx];
			maximum = cmp::max(maximum, other.date);
		}
		maximum
	});
	let mails_printed = RefCell::new(HashSet::new());

	let mut siv = Cursive::new();

	let tree = RefCell::new(TreeView::new());
	// recursive lambda
	struct PrintThread<'a> {
		f: &'a dyn Fn(&PrintThread, NodeIndex, Placement, usize)
	}
	let print_thread = |this: &PrintThread, node, placement, parent| {
		let mail = nodes_inv[&node];
		if mails_printed.borrow().contains(&mail) { // TODO: placement == Placement::After ?
			return;
		}
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
	let (tree_present, last_row) = if tree.len() != 0 {
		let last_row = tree.len() - 1;
		tree.set_selected_row(last_row);
		(true, last_row)
	} else {
		(false, 0)
	};
	let tree_on_select = |siv: &mut Cursive, row| {
		let item = siv.call_on_name("tree", |tree: &mut MailTreeView| {
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
			siv.call_on_name("mail_info", |view: &mut MailInfoView| {
				view.set(item);
			});
			siv.call_on_name("mail", |view: &mut MailPartView| {
				view.set_part(mail);
			});
		}
	};
	tree.set_on_submit(|siv, _row| {
		siv.focus_name("mail").unwrap();
	});
	let tree = tree.on_select(tree_on_select).with_name("tree").scrollable().with_name("tree_scroller");
	let update_flags2 = Arc::clone(&update_flags);
	let update_flags3 = Arc::clone(&update_flags);
	let update_flags4 = Arc::clone(&update_flags);
	let update_flags5 = Arc::clone(&update_flags);
	let tree = OnEventView::new(tree)
		.on_event('r', move |siv| {
			siv.call_on_name("tree", |tree: &mut MailTreeView| {
				if let Some(r) = tree.row() {
					let mail = tree.borrow_item_mut(r).unwrap();
					mail.mark_as_read(true);
					// TODO error handling
					let _ = mail.save_flags(&maildir);
					let _ = update_flags2.lock().execute(params![mail.get_flags(), mail.id.to_i64()]);
				}
			});
		})
		.on_event('u', move |siv| {
			siv.call_on_name("tree", |tree: &mut MailTreeView| {
				if let Some(r) = tree.row() {
					let mail = tree.borrow_item_mut(r).unwrap();
					mail.mark_as_read(false);
					// TODO error handling
					let _ = mail.save_flags(&maildir);
					let _ = update_flags3.lock().execute(params![mail.get_flags(), mail.id.to_i64()]);
				}
			});
		})
		.on_event('t', move |siv| {
			siv.call_on_name("tree", |tree: &mut MailTreeView| {
				if let Some(r) = tree.row() {
					let mail = tree.borrow_item_mut(r).unwrap();
					mail.mark_as_read(true);
					mail.add_flag2(TRASHED);
					// TODO error handling
					let _ = mail.save_flags(&maildir);
					let _ = update_flags4.lock().execute(params![mail.get_flags(), mail.id.to_i64()]);
				}
			});
		})
		.on_event('d', move |siv| {
			siv.call_on_name("tree", |tree: &mut MailTreeView| {
				if let Some(r) = tree.row() {
					let mail = tree.borrow_item_mut(r).unwrap();
					mail.add_flag2(DELETE);
					// TODO error handling
					let _ = mail.save_flags(&maildir);
					let _ = update_flags5.lock().execute(params![mail.get_flags(), mail.id.to_i64()]);
				}
			});
		});
	let tree_resized = ResizedView::new(SizeConstraint::Fixed(120), SizeConstraint::Full, tree);
	let mail_info = MailInfoView::new().with_name("mail_info");
	let mail_content = MailPartView::empty().with_name("mail");
	static MAIL_FULLSCREEN: AtomicBool = AtomicBool::new(false);
	let dummy = std::rc::Rc::new(RefCell::new(Some(OnEventView::new(MailView::empty().with_name("dummy")))));
	let dummy_ = dummy.clone();
	let mail_content = OnEventView::new(mail_content)
		.on_event('f', move |s| {
			let dummy__ = dummy_.clone();
			if MAIL_FULLSCREEN.load(Ordering::SeqCst) {
				let layer = s.pop_layer().unwrap();
				if let Ok(textview) = layer.downcast::<ResizedView<MailScrollerView>>() {
					let mut it = textview.into_inner().unwrap_or_else(|_| panic!("?"));
					it.get_inner_mut().get_mut().set_scroll(true);
					it.get_inner_mut().get_mut().set_wrap_method(WrapMethod::XiUnicode);
					dummy__.borrow_mut().replace(it);
					s.call_on_name("mail_scroller", move |this: &mut MailScrollerView| {
						std::mem::swap(dummy__.borrow_mut().as_mut().unwrap(), this);
					});
				}
				MAIL_FULLSCREEN.store(false, Ordering::SeqCst);
			} else {
				s.call_on_name("mail_scroller", move |this: &mut MailScrollerView| {
					std::mem::swap(dummy__.borrow_mut().as_mut().unwrap(), this);
				});
				let mut it = dummy_.borrow_mut().take().unwrap();
				it.get_inner_mut().get_mut().set_scroll(false);
				it.get_inner_mut().get_mut().set_wrap_method(WrapMethod::Newlines);
				eprintln!("adding fullscreen layer!");
				s.add_fullscreen_layer(ResizedView::with_full_screen(it));
				MAIL_FULLSCREEN.store(true, Ordering::SeqCst);
			}
		})
		.on_event('s', |s| {
			if let Some((bytes, name)) = s.call_on_name("mail", |mail: &mut MailPartView| {
				mail.part.map(|x| (x.get_body_raw().unwrap(), x.get_content_disposition().params.get("filename").cloned()))
			}).flatten() {
				let mut default_path = CONFIG.get().unwrap().read().browse.base_save_path.display().to_string();
				if let Some(name) = name {
					default_path.push('/');
					default_path += &name;
				}
				let bytes = Rc::new(bytes);
				let bytes2 = bytes.clone();
				s.add_layer(
					Dialog::new()
						.title("Enter filename")
						.padding_lrtb(1, 1, 1, 0)
						.content(
							EditView::new()
								.content(default_path)
								.on_submit(move |s, path| {
									std::fs::write(path, bytes.as_ref()).unwrap();
									s.pop_layer();
								})
								.with_name("filename")
								.fixed_width(100),
						)
						.button("Ok", move |s| {
							let path = s
								.call_on_name("filename", move |view: &mut EditView| {
									view.get_content()
								})
								.unwrap();
							std::fs::write(path.as_ref(), bytes2.as_ref()).unwrap();
							s.pop_layer();
						}),
				);
			}
		});
	let mail_content: MailScrollerView = mail_content;
	let mail_content = mail_content.with_name("mail_scroller");
	let mut mail_part_select = TreeView::<MailPart>::new();
	mail_part_select.set_on_select(|siv, row| {
		let mail = siv.call_on_name("part_select", |tree: &mut TreeView<MailPart>| {
			tree.borrow_item(row).unwrap().part
		}).unwrap();
		siv.call_on_name("mail", |view: &mut MailView| {
			view.set_part(mail);
		});
	});
	mail_part_select.set_on_submit(|siv, _row| {
		siv.focus_name("mail").unwrap();
	});
	let mail_wrapper = LinearLayout::vertical()
		.child(ResizedView::new(SizeConstraint::Full, SizeConstraint::Fixed(5), Panel::new(mail_info).title("Mail")))
		.child(ResizedView::with_full_screen(mail_content))
		.child(Panel::new(mail_part_select.with_name("part_select"))
			.title("Multipart selection"));
	let mail_content_resized = ResizedView::new(SizeConstraint::Fixed(127), SizeConstraint::Full, mail_wrapper);
	let main = LinearLayout::horizontal()
		.child(tree_resized)
		.child(mail_content_resized);
	siv.add_fullscreen_layer(ResizedView::with_full_screen(main));
	if tree_present {
		tree_on_select(&mut siv, last_row); // show selected mail
	}

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
	let mut style_select = SelectView::new().h_align(HAlign::Left);
	let values = ["simple", "reverse", "bold", "italic", "strikethrough", "underline", "blink"];
	for &x in &values {
		style_select.add_item(x, x);
	}
	let current = style_to_str(&config.browse.unread_style);
	style_select.set_selection(values.iter().position(|&x| x == current).unwrap());
	style_select.set_on_select(|_s, style| {
		CONFIG.get().unwrap().write().browse.unread_style = parse_effect(style).unwrap().into();
	});
	setup.add_child(ResizedView::new(SizeConstraint::AtLeast(28), SizeConstraint::Free, Panel::new(style_select).title("Unread message styling")));
	}
	// most horrible hack
	let setup: Arc<RwLock<Option<Box<dyn View>>>> = Arc::new(RwLock::new(Some(Box::new(ResizedView::new(SizeConstraint::Free, SizeConstraint::Full, setup)))));
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
	*setup.write() = Some(Box::new(Panel::new(setup_view).title("Settings")));

	let setup2 = Arc::clone(&setup);
	siv.add_global_callback(Event::Key(Key::F2), move |s| {
		let setup = setup2.write().take().unwrap();
		s.add_fullscreen_layer(setup);
	});

	siv.add_global_callback('q', |s| s.quit());

	// manual event loop (to scroll to end of ScrollView)
	let mut siv = siv.into_runner(cursive::backends::termion::Backend::init()?);
	siv.set_autorefresh(false);
	siv.refresh();
	siv.call_on_name("tree_scroller", |tree: &mut ScrollView<NamedView<MailTreeView>>| {
		tree.on_event(Event::Key(Key::End))
	}).unwrap().process(&mut siv);
	siv.refresh();
	siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().terminal.get_mut().write_all(&[b'\x1b', b'\x50', b't', b'm', b'u', b'x', b';', b'\x1b', b'\x1b', b'\x50', b'=', b'2', b's', b'\x1b', b'\x1b', b'\x5c', b'\x1b', b'\\']).unwrap();
	siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().terminal.get_mut().flush().unwrap();
	*siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().locked.get_mut() = false;
	while siv.is_running() {
		while !siv.step() {}
		siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().terminal.get_mut().write_all(&[b'\x1b', b'\x50', b't', b'm', b'u', b'x', b';', b'\x1b', b'\x1b', b'\x50', b'=', b'2', b's', b'\x1b', b'\x1b', b'\x5c', b'\x1b', b'\\']).unwrap();
		siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().terminal.get_mut().flush().unwrap();
		*siv.backend.as_any_mut().downcast_mut::<cursive::backends::termion::Backend>().unwrap().locked.get_mut() = false;
	}
	Ok(())
}

type MailScrollerView = OnEventView<NamedView<MailView>>;
type MailView = MailPartView;
type MailTreeView<'a> = TreeView<&'a EasyMail<'a>>;

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

struct MailPartView {
	part: Option<&'static ParsedMail<'static>>,
	wrap: WrapMethod,
	scroll: bool,
	text: Option<ScrollView<TextView>>,
	cached_size: Option<Vec2>,
	expected_text_height: Option<usize>,
	layouted_text_with_scroll: bool
}

impl MailPartView {
	fn empty() -> Self {
		MailPartView {
			part: None,
			wrap: WrapMethod::XiUnicode,
			scroll: true,
			text: None,
			cached_size: None,
			expected_text_height: None,
			layouted_text_with_scroll: false
		}
	}

	fn set_wrap_method(&mut self, wrap: WrapMethod) {
		if let Some(text) = self.text.as_mut() {
			text.get_inner_mut().set_wrap_method(wrap);
		}
		self.wrap = wrap;
	}

	fn set_scroll(&mut self, scroll: bool) {
		self.scroll = scroll;
		self.layouted_text_with_scroll = !scroll;
		if let Some(text) = self.text.as_mut() {
			text.set_show_scrollbars(scroll);
		}
	}

	fn set_part(&mut self, part: &'static ParsedMail<'static>) {
		self.part = Some(part);
		self.text = None;
		self.cached_size = None;
		self.expected_text_height = None;
		self.layouted_text_with_scroll = false;
	}

	fn setup_text(&mut self, size: Vec2) {
		if self.part.is_none() {
			return;
		}
		let part = self.part.unwrap();
		let body = if part.ctype.mimetype == "text/html" {
			let html = part.get_body().unwrap();
			eprintln!("HTML layout using {} width, length {:?}", size.x, html.len());
			html2text::from_read(html.as_bytes(), size.x)
		} else if part.ctype.mimetype.starts_with("text/") {
			part.get_body().unwrap()
		} else {
			"binary data".into()
		};
		let mut text = TextView::new(body);
		text.set_wrap_method(self.wrap);
		let text = text.scrollable()
			.show_scrollbars(self.scroll);
		self.text = Some(text);
	}
}

impl View for MailPartView {
	fn draw(&self, printer: &cursive::Printer) {
		if let Some(text) = self.text.as_ref() {
			text.draw(printer)
		}
	}

	fn layout(&mut self, given_size: Vec2) {
		eprintln!("layout called with {:?}", given_size);
		if self.cached_size.is_some() {
			if self.cached_size != Some(given_size) {
				self.setup_text(given_size);
			} else {
				if self.layouted_text_with_scroll != self.scroll && self.expected_text_height.unwrap_or(0) > given_size.y {
					eprintln!("reconsidering given {:?}", given_size);
					self.setup_text(given_size.map_x(|x| x-2));
					self.layouted_text_with_scroll = self.scroll;
				} else {
					return;
				}
			}
		}
		self.cached_size = Some(given_size);
		if let Some(text) = self.text.as_mut() {
			text.layout(given_size);
		} else if self.part.is_some() {
			self.setup_text(given_size);
			self.text.as_mut().unwrap().layout(given_size);
		}
	}

	fn needs_relayout(&self) -> bool {
		true
	}

	fn required_size(&mut self, constraint: Vec2) -> Vec2 {
		if self.expected_text_height.is_none() && self.text.is_some() {
			self.expected_text_height = Some(self.text.as_mut().unwrap().required_size(constraint).y);
		}
		constraint
	}

	fn on_event(&mut self, ev: Event) -> cursive::event::EventResult {
		if let Some(text) = self.text.as_mut() {
			text.on_event(ev)
		} else {
			cursive::event::EventResult::Ignored
		}
	}

	fn call_on_any<'a>(&mut self, sel: &cursive::view::Selector<'_>, cb: cursive::event::AnyCb<'a>) {
		if let Some(text) = self.text.as_mut() {
			text.call_on_any(sel, cb)
		}
	}

	fn focus_view(&mut self, sel: &cursive::view::Selector<'_>) -> StdResult<(), cursive::view::ViewNotFound> {
		if let Some(text) = self.text.as_mut() {
			text.focus_view(sel)
		} else {
			Err(cursive::view::ViewNotFound)
		}
	}

	fn take_focus(&mut self, _source: cursive::direction::Direction) -> bool {
		true
	}

	fn important_area(&self, view_size: Vec2) -> cursive::Rect {
		if let Some(text) = self.text.as_ref() {
			text.important_area(view_size)
		} else {
			cursive::Rect::from_size((0, 30), view_size)
		}
	}
}

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
