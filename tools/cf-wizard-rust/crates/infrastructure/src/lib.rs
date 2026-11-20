mod assets;
pub mod canonical;
pub mod engine;
#[cfg(test)]
pub mod forms;
pub mod git;
mod git_recursive;
pub mod git_source;
mod git_tree_cache;
mod identity;
pub mod index;
mod index_integrity;
pub mod indexing;
pub mod journal;
pub mod kernel;
pub mod kinds;
#[cfg(test)]
pub mod metadata;
pub mod native;
pub mod output;
pub mod package;
mod package_compiler;
pub mod platform;
mod predefined;
#[cfg(test)]
pub mod query;
mod requirements;
pub mod settings;
pub mod source;
mod source_forms;
mod source_literals;
mod source_query;
pub mod stubs;
pub mod vault;
pub mod xml;
pub fn external(error: impl std::fmt::Display) -> cf_domain::Error {
    cf_domain::Error::External(error.to_string())
}

pub(crate) mod emission_guard;
pub(crate) mod facts_cache;
pub(crate) mod projection;
pub mod readback;
pub(crate) mod reference_lexer;
pub(crate) mod registrar_facts;
mod resource_readback;
mod source_edits;
#[cfg(test)]
mod source_proof_tests;
pub(crate) mod source_reader;
pub(crate) mod source_schema;
pub(crate) mod source_xml;

mod sqlite_cache;
