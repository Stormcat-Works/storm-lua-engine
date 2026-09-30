//! Test-only adapter: semantic fixtures also verify generated source origins.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use storm_lua_syntax::provenance::GeneratedOrigins;
use storm_lua_syntax::{Ast, NodeId};

pub(crate) fn parse_source(source: &str) -> Result<(Ast, NodeId), String> {
    storm_lua_syntax::parse_source_with_origins("pass-fixture.lua", source)
}

pub(crate) struct Printer<'a> {
    ast: &'a Ast,
    inner: storm_lua_syntax::Printer<'a>,
}
impl<'a> Printer<'a> {
    pub(crate) fn new(ast: &'a Ast, zero: bool) -> Self {
        Self {
            ast,
            inner: storm_lua_syntax::Printer::new(ast, zero),
        }
    }
    pub(crate) fn output(&mut self, root: NodeId) -> String {
        let printed = self.inner.output_with_positions(root);
        assert!(
            self.ast.nodes.tracks_origins(),
            "a semantic fixture lost its entire origin arena"
        );
        {
            let origins = GeneratedOrigins::from_print(self.ast, &printed).unwrap();
            origins.validate_for_code(&printed.code).unwrap();
            let unknown = origins
                .mappings
                .iter()
                .filter(|m| m.origin.is_none())
                .map(|m| &printed.code[m.start..m.end])
                .collect::<Vec<_>>();
            assert_eq!(
                origins.unknown_bytes(),
                0,
                "origin audit gaps {unknown:?} in {}",
                printed.code
            );
        }
        printed.code
    }
}
