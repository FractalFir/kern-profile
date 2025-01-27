#![feature(iter_array_chunks)]
#![feature(iter_intersperse)]

use anyhow::Result;
use elf::endian::AnyEndian;
use elf::ElfBytes;
use elf::ParseError;
use rustc_demangle::demangle;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::io;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::mem::size_of;
use std::process::exit;

struct Symbol {
    addr: u64,
    size: u64,
    name: String,
}

/// Loads the symbols list, sorted by address.
///
/// If no symbol table is present, the function returns `None`.
fn list_symbols(elf_path: &OsString) -> Result<Option<Vec<Symbol>>> {
    let elf_buf = fs::read(elf_path)?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&elf_buf)?;
    let symbol_table = elf.symbol_table()?;
    let Some((symbol_table, string_table)) = symbol_table else {
        return Ok(None);
    };

    let mut syms = symbol_table
        .iter()
        .map(|sym| {
            let name = string_table.get(sym.st_name as _)?;
            Ok::<_, ParseError>(Symbol {
                addr: sym.st_value,
                size: sym.st_size,
                name: format!("{:#}", demangle(name)),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    syms.sort_unstable_by(|s1, s2| s1.addr.cmp(&s2.addr).then_with(|| s1.size.cmp(&s2.size)));
    Ok(Some(syms))
}

/// Returns the name of the symbol in which the address is located.
fn find_symbol(symbols: &[Symbol], addr: u64) -> Option<&str> {
    let index = symbols
        .binary_search_by(|sym| {
            if addr < sym.addr {
                Ordering::Greater
            } else if addr >= sym.addr + sym.size {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
        .ok()?;
    Some(symbols[index].name.as_str())
}

/// Count the number of identical stacks.
///
/// The function returns a hashmap with each stack associated with its number of occurences.
fn fold_stacks<'s>(
    input_path: &OsString,
    symbols: &'s [Symbol],
) -> io::Result<HashMap<Vec<&'s str>, u64>> {
    // Read profile data
    let input = File::open(input_path)?;
    let reader = BufReader::new(input);
    let mut iter = reader.bytes();

    let mut folded_stacks: HashMap<Vec<&str>, u64> = HashMap::new();
    while let Some(stack_depth) = iter.next() {
        // Read and convert to symbols
        let stack_depth = stack_depth? as usize;
        let mut frames = iter
            .by_ref()
            .take(stack_depth * size_of::<u64>())
            .map(|r| r.unwrap()) // TODO handle error
            .array_chunks()
            .map(u64::from_ne_bytes)
            .map(|addr| find_symbol(symbols, addr))
            .peekable();

        // Subdivide stack into substacks (interruptions handling)
        while frames.peek().is_some() {
            let substack: Vec<_> = frames
                .by_ref()
                .take_while(Option::is_some)
                .map(|f| f.unwrap())
                .collect();
            if substack.is_empty() {
                continue;
            }

            // Increment counter
            *folded_stacks.entry(substack).or_insert(0) += 1;
        }
    }
    Ok(folded_stacks)
}

fn main() -> io::Result<()> {
    let args: Vec<OsString> = env::args_os().collect();
    let [_, input_path, elf_path, output_path] = &args[..] else {
        eprintln!("usage: kern-profile <profile file> <elf file> <output file>");
        exit(1);
    };

    // Read ELF symbols
    let symbols = match list_symbols(elf_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not read ELF: {e}");
            exit(1);
        }
    };
    let Some(symbols) = symbols else {
        eprintln!("ELF does not have a symbol table!");
        exit(1);
    };

    let folded_stacks = fold_stacks(input_path, &symbols)?;

    // Serialize
    let out = File::create(output_path)?;
    let mut writer = BufWriter::new(out);
    for (frames, count) in folded_stacks {
        let buff = frames
            .iter()
            .rev()
            .map(|f| f.as_bytes())
            .intersperse(&[b';'])
            .flatten();
        // TODO optimize (write buffers instead of byte-by-byte)
        for b in buff {
            writer.write_all(&[*b])?;
        }
        writeln!(writer, " {count}")?;
    }

    Ok(())
}
