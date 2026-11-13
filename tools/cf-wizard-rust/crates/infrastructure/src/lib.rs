pub mod canonical;
pub mod engine;
pub mod forms;
pub mod git;
pub mod git_source;
pub mod index;
pub mod indexing;
pub mod journal;
pub mod kinds;
pub mod metadata;
pub mod native;
pub mod output;
pub mod package;
pub mod platform;
pub mod query;
pub mod settings;
pub mod source;
pub mod stubs;
pub mod vault;
pub mod xml;
pub fn external(error: impl std::fmt::Display) -> cf_domain::Error {
    cf_domain::Error::External(error.to_string())
}
