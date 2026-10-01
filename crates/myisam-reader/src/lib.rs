//! Streaming access to legacy MySQL 5.x FRM schemas and fixed/dynamic MyISAM records.
//!
//! Table data is read from any `std::io::Read`, including a decompressor or network stream.
//! Only the leading MYI header is required; complete indexes and extracted tables are unnecessary.
//! Fragmented rows use bounded memory and temporary SQLite storage for distant/unresolved fragments.
//! This library does not start a database server or invoke external programs.
//!
//! The reader targets the format in the checked-in real snapshots. Compressed `myisampack` tables
//! are rejected. Text scalars assume UTF-8, and numeric scalar conversion supports the unsigned
//! integer types used by these snapshots; this is not a general SQL type conversion library.
//!
//! `RowDecoder::project` and `RowDecoder::with_value_limit` can avoid copying irrelevant fields.
//! Unselected, oversized and SQL NULL fields each have a `None` slot in the result.

#![doc = include_str!("../README.md")]

mod bytes;
mod metadata;
mod records;
mod row;
mod schema;

pub use metadata::{MyisamColumnDef, MyisamInfo, read_myi_header};
pub use records::{
    WalkOptions, WalkStats, walk_records, walk_records_in, walk_records_with_options,
};
pub use row::{RowDecoder, number, value};
pub use schema::{FrmColumn, FrmSchema};

#[cfg(test)]
mod tests;
