//! XFA Scripting Module - Modular XFA 3.3 compliant scripting engine
//!
//! This module provides a complete implementation of XFA scripting capabilities,
//! organized into the following submodules:
//!
//! - [`events`]: Event types, script content types, and parsing
//! - [`som`]: SOM (Scripting Object Model) path handling and resolution
//! - [`dependency`]: Dependency tracking for cascading calculations
//! - [`registry`]: Script registration and categorization
//! - [`state`]: Form state, values, and presence management
//! - [`js_helpers`]: JavaScript helper code for the scripting engine
//! - [`engine`]: Core JavaScript execution engine
//! - [`form`]: High-level XFA form API
//!
//! # Architecture
//!
//! The scripting system follows a layered architecture:
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │                   XfaForm                       │  ← High-level API
//! ├─────────────────────────────────────────────────┤
//! │              XfaScriptEngine                    │  ← JS execution
//! ├──────────────┬──────────────┬──────────────────┤
//! │ ScriptRegistry│ DependencyTracker │ SomResolver │  ← Support services
//! ├──────────────┴──────────────┴──────────────────┤
//! │           Events, State, JS Helpers            │  ← Core types
//! └─────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use blueprint::scripting::{XfaForm, EventActivity};
//!
//! // Create a form from parsed XFA nodes
//! let form = XfaForm::new(nodes)?;
//!
//! // Resolve a field by SOM path
//! if let Some(field) = form.resolve("Page1.TextField1") {
//!     println!("Value: {:?}", field.raw_value());
//!     println!("Visible: {}", field.is_visible());
//! }
//!
//! // Execute events
//! form.click("Page1.SubmitButton")?;
//! form.change("Page1.DropdownField")?;
//!
//! // Refresh layout after changes
//! form.refresh()?;
//! ```

pub mod dependency;
pub mod engine;
pub mod events;
pub mod form;
pub mod js_helpers;
pub mod registry;
pub mod script_object;
pub mod som;
pub mod state;

// Re-export commonly used types at module root for convenience

// Events
pub use events::{
    EventActivity, EventRef, ListenScope, RunAt, ScriptContentType, XfaScript,
    parse_events_from_node, parse_variables_from_node,
};

// SOM
pub use som::{NodeInfo, SomPath, SomResolver, walk_som_path, walk_som_path_mut};

// Dependency tracking
pub use dependency::DependencyTracker;

// Script registry
pub use registry::{RegisteredScript, ScriptRegistry, ScriptType};

// State management
pub use state::{FormState, Presence, SharedFormState, XfaValue};

// Engine
pub use engine::XfaScriptEngine;
pub use form::{EffectiveAccess, EventResult, NodeBounds, XfaForm, XfaNodeRef, XfaNodeRefMut};

// Script object wrapper
pub use script_object::wrap_script_object;

// Form (high-level API)

// JS Helpers (for advanced usage)
pub use js_helpers::{XFA_RESOLVE_PATH_HELPER, get_all_helpers};

#[cfg(test)]
mod tests;
