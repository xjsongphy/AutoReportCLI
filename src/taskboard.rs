//! Shared task board used by `manage_tasks` / `send_to_agent` to coordinate
//! Main ↔ sub-agent work. Thread-safe via a mutex.

use crate::types::{AgentType, TaskItem, TaskStatus};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct TaskBoard {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    tasks: HashMap<String, TaskItem>,
    counter: u64,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tasks: HashMap::new(),
                counter: 0,
            })),
        }
    }

    fn next_id(&self, g: &mut Inner) -> String {
        g.counter += 1;
        format!("T{}", g.counter)
    }

    pub fn create(
        &self,
        source: AgentType,
        target: AgentType,
        brief: String,
        blocking: bool,
        session_id: Option<String>,
    ) -> TaskItem {
        let mut g = self.inner.lock().unwrap();
        let id = self.next_id(&mut g);
        let task = TaskItem {
            task_id: id.clone(),
            brief,
            source_agent: source,
            target_agent: target,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            completed_at: None,
            blocking,
            session_id,
            reply: None,
        };
        g.tasks.insert(id, task.clone());
        task
    }

    pub fn add_local(&self, agent: AgentType, brief: String) -> TaskItem {
        self.create(agent, agent, brief, false, None)
    }

    fn set_status(&self, task_id: &str, status: TaskStatus) -> Option<TaskItem> {
        let mut g = self.inner.lock().unwrap();
        let task = g.tasks.get_mut(task_id)?;
        task.status = status;
        if status.is_settled() {
            task.completed_at = Some(Utc::now());
        }
        Some(task.clone())
    }

    pub fn start(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::InProgress)
    }
    pub fn complete(&self, task_id: &str, reply: Option<String>) -> Option<TaskItem> {
        let mut g = self.inner.lock().unwrap();
        let task = g.tasks.get_mut(task_id)?;
        task.status = TaskStatus::Completed;
        task.completed_at = Some(Utc::now());
        if let Some(r) = reply {
            task.reply = Some(r);
        }
        Some(task.clone())
    }
    pub fn fail(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Failed)
    }
    pub fn cancel(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Cancelled)
    }
    /// Mark a delegated task BLOCKED — the target cannot proceed and needs the
    /// dispatcher (source) to act. Returns the updated task.
    pub fn block(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Blocked)
    }

    /// Look up a task by id with optional filters. `active_only` restricts to
    /// pending/in_progress.
    pub fn get_task(
        &self,
        task_id: &str,
        target_agent: Option<AgentType>,
        active_only: bool,
    ) -> Option<TaskItem> {
        let g = self.inner.lock().unwrap();
        g.tasks
            .get(task_id)
            .filter(|t| {
                target_agent.is_none_or(|a| t.target_agent == a)
                    && (!active_only
                        || matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress))
            })
            .cloned()
    }

    /// Tasks assigned *to* `agent` that are still open (pending / in progress).
    pub fn todolist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap();
        g.tasks
            .values()
            .filter(|t| {
                t.target_agent == agent
                    && matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress)
            })
            .cloned()
            .collect()
    }

    /// Tasks `agent` assigned *to others* that are still open.
    pub fn waitlist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap();
        g.tasks
            .values()
            .filter(|t| {
                t.source_agent == agent
                    && t.target_agent != agent
                    && matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress)
            })
            .cloned()
            .collect()
    }

    /// Tasks `agent` dispatched that are currently BLOCKED (need its action).
    pub fn blocked_waitlist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap();
        g.tasks
            .values()
            .filter(|t| {
                t.source_agent == agent
                    && t.target_agent != agent
                    && t.status == TaskStatus::Blocked
            })
            .cloned()
            .collect()
    }

    pub fn all(&self) -> Vec<TaskItem> {
        self.inner.lock().unwrap().tasks.values().cloned().collect()
    }
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new()
    }
}
