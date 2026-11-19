//! NslApp subsystems. Main owns shared state; each file owns related methods.
//! Method bodies are moved intact so behavior and event order do not change.
//! The frame module still owns panel/dialog orchestration.

mod downloads;
mod frame;
mod images;
mod library;
mod maintenance;
mod scanning;
mod settings;
mod startup;
mod store;
mod updates;
