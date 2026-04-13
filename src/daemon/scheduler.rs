use std::collections::{HashMap, HashSet, VecDeque};

use crate::types::{Edge, Task, TaskSpec, TaskState};

/// Directed acyclic graph tracking task dependencies.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// task_id -> set of task_ids it depends on (prerequisites)
    pub deps: HashMap<String, HashSet<String>>,
    /// task_id -> set of task_ids that depend on it (dependents)
    pub reverse_deps: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    /// Build a dependency graph from tasks and edges.
    ///
    /// Validates that all edge endpoints reference existing tasks and
    /// detects cycles via Kahn's algorithm (topological sort).
    pub fn new(tasks: &[TaskSpec], edges: &[Edge]) -> Result<Self, String> {
        let task_ids: HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();

        let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
        let mut reverse_deps: HashMap<String, HashSet<String>> = HashMap::new();

        // Initialize entries for every task
        for id in &task_ids {
            deps.entry(id.clone()).or_default();
            reverse_deps.entry(id.clone()).or_default();
        }

        // Populate from edges
        for edge in edges {
            if !task_ids.contains(&edge.from) {
                return Err(format!(
                    "edge references unknown task: '{}'",
                    edge.from
                ));
            }
            if !task_ids.contains(&edge.to) {
                return Err(format!(
                    "edge references unknown task: '{}'",
                    edge.to
                ));
            }
            // edge.from must complete before edge.to can start
            // so edge.to depends on edge.from
            deps.entry(edge.to.clone())
                .or_default()
                .insert(edge.from.clone());
            reverse_deps
                .entry(edge.from.clone())
                .or_default()
                .insert(edge.to.clone());
        }

        // Cycle detection via Kahn's algorithm
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        for id in &task_ids {
            in_degree.insert(id.clone(), deps[id].len());
        }

        let mut queue: VecDeque<String> = VecDeque::new();
        for (id, &degree) in &in_degree {
            if degree == 0 {
                queue.push_back(id.clone());
            }
        }

        let mut visited = 0usize;
        while let Some(node) = queue.pop_front() {
            visited += 1;
            if let Some(dependents) = reverse_deps.get(&node) {
                for dep in dependents {
                    if let Some(d) = in_degree.get_mut(dep) {
                        *d -= 1;
                        if *d == 0 {
                            queue.push_back(dep.clone());
                        }
                    }
                }
            }
        }

        if visited != task_ids.len() {
            return Err("dependency graph contains a cycle".to_string());
        }

        Ok(Self { deps, reverse_deps })
    }

    /// Return task IDs whose dependencies are all satisfied by the completed set.
    pub fn ready_tasks(&self, completed: &HashSet<String>) -> Vec<String> {
        let mut ready = Vec::new();
        for (task_id, task_deps) in &self.deps {
            if !completed.contains(task_id) && task_deps.is_subset(completed) {
                ready.push(task_id.clone());
            }
        }
        ready.sort(); // deterministic ordering
        ready
    }

    /// Return the set of direct dependencies for a given task.
    pub fn dependencies_of(&self, task_id: &str) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> =
            std::sync::LazyLock::new(HashSet::new);
        self.deps.get(task_id).unwrap_or(&EMPTY)
    }
}

/// Task scheduler that wraps a DependencyGraph and provides
/// assignment logic respecting worker capacity.
#[derive(Debug, Clone)]
pub struct Scheduler {
    pub graph: DependencyGraph,
}

impl Scheduler {
    /// Create a new scheduler from tasks and edges.
    pub fn new(tasks: &[TaskSpec], edges: &[Edge]) -> Result<Self, String> {
        let graph = DependencyGraph::new(tasks, edges)?;
        Ok(Self { graph })
    }

    /// Return task IDs that can be assigned to workers right now.
    ///
    /// Filters ready tasks to only those in Pending state and respects
    /// the worker capacity limit.
    pub fn assignable_tasks(
        &self,
        tasks: &HashMap<String, Task>,
        max_workers: usize,
        active_count: usize,
    ) -> Vec<String> {
        let available_slots = max_workers.saturating_sub(active_count);
        if available_slots == 0 {
            return Vec::new();
        }

        let completed: HashSet<String> = tasks
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.state,
                    TaskState::Done
                        | TaskState::Completed
                        | TaskState::Accepted
                )
            })
            .map(|(id, _)| id.clone())
            .collect();

        let ready = self.graph.ready_tasks(&completed);

        ready
            .into_iter()
            .filter(|id| {
                tasks
                    .get(id)
                    .map_or(false, |t| t.state == TaskState::Pending)
            })
            .take(available_slots)
            .collect()
    }

    /// Check whether two task specs have overlapping file sets.
    pub fn has_file_overlap(task_a: &TaskSpec, task_b: &TaskSpec) -> bool {
        let files_a: HashSet<&String> = task_a.context.files.iter().collect();
        let files_b: HashSet<&String> = task_b.context.files.iter().collect();
        !files_a.is_disjoint(&files_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IsolationMode, TaskContext};

    fn make_spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            context: TaskContext::default(),
            isolation: IsolationMode::Auto,
            depends_on: Vec::new(),
            priority: 0,
        }
    }

    #[test]
    fn test_no_edges_all_ready() {
        let tasks = vec![make_spec("t1"), make_spec("t2"), make_spec("t3")];
        let graph = DependencyGraph::new(&tasks, &[]).unwrap();
        let ready = graph.ready_tasks(&HashSet::new());
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn test_linear_chain() {
        let tasks = vec![make_spec("t1"), make_spec("t2"), make_spec("t3")];
        let edges = vec![
            Edge { from: "t1".into(), to: "t2".into() },
            Edge { from: "t2".into(), to: "t3".into() },
        ];
        let graph = DependencyGraph::new(&tasks, &edges).unwrap();

        // Only t1 ready initially
        let ready = graph.ready_tasks(&HashSet::new());
        assert_eq!(ready, vec!["t1"]);

        // After t1 done, t2 ready
        let mut completed = HashSet::new();
        completed.insert("t1".into());
        let ready = graph.ready_tasks(&completed);
        assert_eq!(ready, vec!["t2"]);

        // After t2 done, t3 ready
        completed.insert("t2".into());
        let ready = graph.ready_tasks(&completed);
        assert_eq!(ready, vec!["t3"]);
    }

    #[test]
    fn test_cycle_detected() {
        let tasks = vec![make_spec("t1"), make_spec("t2")];
        let edges = vec![
            Edge { from: "t1".into(), to: "t2".into() },
            Edge { from: "t2".into(), to: "t1".into() },
        ];
        let result = DependencyGraph::new(&tasks, &edges);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cycle"));
    }
}
