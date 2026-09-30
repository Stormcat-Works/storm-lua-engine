//! Parsed-source to printed-code range demonstration. No optimization passes.
//! Original positions remain valid because this example never changes the AST.
use std::error::Error;
use storm_lua_syntax::{parse_source_with_positions, source_position::LineIndex, Printer};

fn main() -> Result<(), Box<dyn Error>> {
    let source = "-- 雪😀\r\nlocal value=0x10\r\nlocal function twice(value) return value*2 end\r\noutput.setNumber(1,twice(value))";
    let (ast, root, origins) = parse_source_with_positions(source)?;
    let expected = Printer::new(&ast, true).output(root);
    let printed = Printer::new(&ast, true).output_with_positions(root);
    assert_eq!(printed.code, expected);
    let original = LineIndex::new(source);
    let generated = LineIndex::new(&printed.code);
    println!("Original: {source:?}\nGenerated: {:?}", printed.code);
    for emission in &printed.emissions {
        let Some(site) = emission.site else { continue };
        let Some((start, end)) = origins.name_span(emission.node, site) else {
            continue;
        };
        let (original_line, original_column) = original
            .utf16_position(start)
            .ok_or("invalid original boundary")?;
        let (generated_line, generated_column) = generated
            .utf16_position(emission.start)
            .ok_or("invalid generated boundary")?;
        println!(
            "node {} {site:?}: generated {}:{} {:?} -> original {}:{} {:?}",
            emission.node,
            generated_line + 1,
            generated_column,
            &printed.code[emission.start..emission.end],
            original_line + 1,
            original_column,
            &source[start..end]
        );
    }
    Ok(())
}
