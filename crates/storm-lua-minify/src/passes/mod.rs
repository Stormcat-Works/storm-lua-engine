//! `passes/` モジュールルート。各最適化パスは 1 ファイル 1 テーマ。

pub mod api_aliases;
pub mod available_expressions;
pub mod binding_rescaling;
pub mod boolean_numerics;
pub mod callback_slots;
pub mod captured_use;
pub mod coefficient_carriers;
pub mod color_unpack;
pub mod common_offsets;
pub mod conditionals;
pub mod decode_chains;
pub mod default_hoisting;
pub mod destructive_results;
pub mod final_stores;
pub mod global_stores;
pub mod immutable_synthesis;
pub mod immutable_values;
pub mod induction_values;
pub mod inline_functions;
pub mod interval_shifting;
pub mod literal_folding;
pub mod locals;
pub mod multiplicative_carriers;
pub mod numeric_literals;
pub mod omit_arguments;
pub mod output_loops;
pub mod parentheses;
pub mod quotient_remainder;
pub mod radix_helpers;
pub mod root_globals;
pub mod scratch_coalescing;
pub mod screen_buttons;
pub mod screen_call_factoring;
pub mod screen_loops;
pub mod signed_factoring;
pub mod single_use_forwarding;
pub mod sparse_boolean;
pub mod split_sign;
pub mod tables;
pub mod temporary_globals;
pub mod uniform_tables;
pub mod wrapper_functions;

pub mod function_globalization;

pub mod property_reads;

pub mod closed_fields;

pub mod namespace_functions;

pub mod literal_pool;

/// Ordered data-driven draw command packing.
pub mod draw_records;

/// Repeated ordered literal drawing motifs.
pub mod draw_sequences;

/// Pack independent neighboring local declarations.
pub mod adjacent_locals;

mod origins;
