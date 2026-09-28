//! Dependency Tracking for Cascading Calculations
//!
//! Per XFA 3.3 Chapter 10 (pages 379-380):
//! "Calculate objects that refer to the changed object will then be executed"

use super::som::SomPath;
use std::collections::{HashMap, HashSet};

/// Tracks dependencies between calculated fields
/// Per XFA 3.3 spec: "Calculate objects that refer to the changed object
/// will then be executed"
pub struct DependencyTracker {
    /// Map from source field -> fields that depend on it
    dependencies: HashMap<SomPath, HashSet<SomPath>>,
    /// Map from field -> fields it depends on (reverse lookup)
    reverse_deps: HashMap<SomPath, HashSet<SomPath>>,
}

impl DependencyTracker {
    pub fn new() -> Self {
        DependencyTracker {
            dependencies: HashMap::new(),
            reverse_deps: HashMap::new(),
        }
    }

    /// Record that `dependent` depends on `source`
    pub fn add_dependency(&mut self, dependent: &SomPath, source: &SomPath) {
        self.dependencies
            .entry(source.clone())
            .or_default()
            .insert(dependent.clone());

        self.reverse_deps
            .entry(dependent.clone())
            .or_default()
            .insert(source.clone());
    }

    /// Get all fields that should recalculate when `source` changes
    pub fn get_dependents(&self, source: &SomPath) -> Vec<SomPath> {
        self.dependencies
            .get(source)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all fields that this field depends on
    pub fn get_sources(&self, dependent: &SomPath) -> Vec<SomPath> {
        self.reverse_deps
            .get(dependent)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Clear dependencies for a field (useful when script changes)
    pub fn clear_for_field(&mut self, field: &SomPath) {
        // Remove from reverse deps
        if let Some(sources) = self.reverse_deps.remove(field) {
            for source in sources {
                if let Some(deps) = self.dependencies.get_mut(&source) {
                    deps.remove(field);
                }
            }
        }
        // Remove as source
        self.dependencies.remove(field);
    }

    /// Get all dependents transitively (cascading), in topological order.
    /// This ensures that if A depends on B and B depends on C, changing C
    /// will return [B, A] so B is recalculated before A.
    pub fn get_dependents_cascade(&self, source: &SomPath) -> Vec<SomPath> {
        let mut visited = HashSet::new();
        let mut result = Vec::new();

        // BFS to find all transitively dependent fields
        let mut queue = vec![source.clone()];
        while let Some(current) = queue.pop() {
            for dependent in self.get_dependents(&current) {
                if visited.insert(dependent.clone()) {
                    result.push(dependent.clone());
                    queue.push(dependent);
                }
            }
        }

        // Topological sort: fields with fewer dependencies come first
        result.sort_by(|a, b| {
            let a_deps = self.get_sources(a).len();
            let b_deps = self.get_sources(b).len();
            a_deps.cmp(&b_deps)
        });

        result
    }
}

impl Default for DependencyTracker {
    fn default() -> Self {
        Self::new()
    }
}
