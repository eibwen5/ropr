use clap::Parser;
use colored::control::set_override;
use core::panic;
use rayon::prelude::*;
use regex::Regex;
use ropr::{
	binary::Binary, disassembler::Disassembly, formatter::ColourFormatter, gadgets::Gadget,
};
use std::{
	collections::HashSet,
	error::Error,
	io::{stdout, BufWriter, Write},
	path::PathBuf,
	time::Instant,
};

#[derive(Parser)]
#[clap(version)]
struct Opt {
	/// Includes potentially low-quality gadgets such as prefixes, conditional branches, and near branches (will find significantly more gadgets)
	#[clap(short = 'n', long)]
	noisy: bool,

	/// Forces output to be in colour or plain text (`true` or `false`)
	#[clap(short = 'c', long)]
	colour: Option<bool>,

	/// Removes normal "ROP Gadgets"
	#[clap(short = 'r', long)]
	norop: bool,

	/// Removes syscalls and other interrupts
	#[clap(short = 's', long)]
	nosys: bool,

	/// Removes "JOP Gadgets" - these may have a controllable branch, call, etc. instead of a simple `ret` at the end
	#[clap(short = 'j', long)]
	nojop: bool,

	/// Filters for gadgets which alter the stack pointer
	#[clap(short = 'p', long)]
	stack_pivot: bool,

	/// Filters for gadgets which alter the base pointer
	#[clap(short = 'b', long)]
	base_pivot: bool,

	/// Maximum number of instructions in a gadget
	#[clap(short, long, default_value = "6")]
	max_instr: u8,

	/// Perform a regex search on the returned gadgets for easy filtering
	#[clap(short = 'R', long)]
	regex: Vec<String>,

	/// Treats the input file as a blob of code (`true` or `false`)
	#[clap(long)]
	raw: Option<bool>,

	/// Search between address ranges (in hexadecial) eg. `0x1234-0x4567`
	#[clap(long)]
	range: Vec<String>,

	/// The path of the file to inspect
	binary: PathBuf,
}

fn write_gadgets(mut w: impl Write, gadgets: &[(Gadget, String)]) {
	let mut output = ColourFormatter::new();
	for (gadget, _) in gadgets {
		output.clear();
		gadget.format_full(&mut output);
		match writeln!(w, "{}", output) {
			Ok(_) => (),
			Err(_) => return, // Pipe closed - finished writing gadgets
		}
	}
}

fn main() -> Result<(), Box<dyn Error>> {
	let start = Instant::now();

	let opts = Opt::parse();

	let b = opts.binary;
	let b = Binary::new(&b)?;
	let sections = b.sections(opts.raw)?;

	let noisy = opts.noisy;
	let colour = opts.colour;
	let rop = !opts.norop;
	let sys = !opts.nosys;
	let jop = !opts.nojop;
	let stack_pivot = opts.stack_pivot;
	let base_pivot = opts.base_pivot;
	let max_instructions_per_gadget = opts.max_instr as usize;

	if max_instructions_per_gadget == 0 {
		panic!("Max instructions must be >0");
	}

	let ranges = opts
		.range
		.iter()
		.filter_map(|s| s.split_once('-'))
		.filter_map(|(mut from, mut to)| {
			if from.starts_with("0x") {
				from = &from[2..];
			}
			if to.starts_with("0x") {
				to = &to[2..];
			}
			let from = usize::from_str_radix(from, 16).ok()?;
			let to = usize::from_str_radix(to, 16).ok()?;
			Some((from, to))
		})
		.collect::<Vec<_>>();

	let regices = opts
		.regex
		.into_iter()
		.map(|r| Regex::new(&r))
		.collect::<Result<Vec<_>, _>>()?;

	let deduped = sections
		.iter()
		.filter_map(Disassembly::new)
		.flat_map(|dis| {
			(0..dis.bytes().len())
				.into_par_iter()
				.filter(|offset| dis.is_tail_at(*offset, rop, sys, jop, noisy))
				.flat_map_iter(|tail| {
					dis.gadgets_from_tail(tail, max_instructions_per_gadget, noisy)
				})
				.collect::<Vec<_>>()
		})
		.filter(|g| {
			if ranges.is_empty() {
				return true;
			}
			ranges
				.iter()
				.any(|(from, to)| -> bool { *from <= g.address() && g.address() <= *to })
		})
		.collect::<HashSet<_>>();

	let mut gadgets = deduped
		.into_iter()
		.map(|g| {
			let mut formatted = String::new();
			g.format_instruction(&mut formatted);
			(g, formatted)
		})
		.filter(|(_, formatted)| regices.iter().all(|r| r.is_match(formatted)))
		.filter(|(g, _)| !stack_pivot | g.is_stack_pivot())
		.filter(|(g, _)| !base_pivot | g.is_base_pivot())
		.collect::<Vec<_>>();
	gadgets.sort_unstable_by(|(_, a), (_, b)| a.cmp(b));

	let gadget_count = gadgets.len();

	// Don't account for time it takes to print gadgets since this depends on terminal implementation
	let elapsed = Instant::now() - start;

	// Stdout uses a LineWriter internally, therefore we improve performance by wrapping stdout in a BufWriter
	let mut stdout = BufWriter::new(stdout());

	if let Some(colour) = colour {
		set_override(colour);
	}

	write_gadgets(&mut stdout, &gadgets);

	drop(stdout);

	eprintln!(
		"\n==> Found {} gadgets in {:.3} seconds",
		gadget_count,
		elapsed.as_secs_f32()
	);

	Ok(())
}
